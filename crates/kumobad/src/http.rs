use crate::{
    catalog::Operation,
    model::{
        CreateRequest, DeleteSecretsRequest, ExecRequest, RunRequest, SecretsRequest, UpdateRequest,
    },
    service::{ApiError, ApiResult, Service},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::{Bytes, BytesMut};
use http_body_util::{BodyExt, Full, Limited, StreamBody, combinators::BoxBody};
use hyper::{
    Request, Response,
    body::{Frame, Incoming},
};
use kumoba::{Command, Termination};
use kumoba_protocol::MAX_DATA;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::{convert::Infallible, pin::Pin, sync::Arc, task::Poll, time::Duration};
use tokio::{io::AsyncWrite, sync::mpsc};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tokio_util::sync::PollSender;

pub type Body = BoxBody<Bytes, Infallible>;
const JSON_LIMIT: usize = 1024 * 1024;

fn json_response(status: u16, value: Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("cache-control", "no-store")
        .body(Full::new(Bytes::from(value.to_string())).boxed())
        .expect("static response headers")
}

fn error_response(error: ApiError) -> Response<Body> {
    json_response(
        error.status(),
        json!({"error": error.code(), "message": error.to_string()}),
    )
}

fn stream_response(content_type: &str, receiver: mpsc::Receiver<Bytes>) -> Response<Body> {
    let stream = ReceiverStream::new(receiver).map(|bytes| Ok::<_, Infallible>(Frame::data(bytes)));
    Response::builder()
        .status(200)
        .header("content-type", content_type)
        .header("cache-control", "no-store")
        .body(BodyExt::boxed(StreamBody::new(stream)))
        .expect("static response headers")
}

async fn read_json<T: DeserializeOwned>(body: Incoming) -> ApiResult<T> {
    let bytes = tokio::time::timeout(
        Duration::from_secs(30),
        Limited::new(body, JSON_LIMIT).collect(),
    )
    .await
    .map_err(|_| ApiError::Status(408, "request body timed out".into()))?
    .map_err(|_| ApiError::Status(413, "request body too large".into()))?
    .to_bytes();
    let bytes = if bytes.is_empty() {
        Bytes::from_static(b"{}")
    } else {
        bytes
    };
    serde_json::from_slice(&bytes).map_err(|error| ApiError::bad_request(error.to_string()))
}

fn query(request: &Request<Incoming>) -> Vec<(String, String)> {
    request
        .uri()
        .query()
        .map(|query| {
            url::form_urlencoded::parse(query.as_bytes())
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect()
        })
        .unwrap_or_default()
}

fn required_param(params: &[(String, String)], name: &str) -> ApiResult<String> {
    params
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
        .ok_or_else(|| ApiError::bad_request(format!("missing {name}")))
}

fn optional_param(params: &[(String, String)], name: &str) -> Option<String> {
    params
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}

pub fn authorized(request: &Request<Incoming>, token: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|presented| presented.as_bytes().ct_eq(token).into())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Listener {
    Api,
    Browser,
}

pub async fn handle(
    request: Request<Incoming>,
    service: Arc<Service>,
    token: Arc<Vec<u8>>,
    listener: Listener,
) -> Result<Response<Body>, Infallible> {
    if request.uri().path() == "/healthz" {
        return Ok(json_response(200, json!({"ok": true})));
    }
    if request.uri().path().starts_with("/v1/preview/") {
        return Ok(preview(request, service, listener == Listener::Browser)
            .await
            .unwrap_or_else(error_response));
    }
    if listener == Listener::Browser {
        return Ok(json_response(
            404,
            json!({"error": "not_found", "message": "this listener serves previews only"}),
        ));
    }
    if !authorized(&request, &token) {
        return Ok(json_response(401, json!({"error": "unauthorized"})));
    }
    Ok(route(request, service).await.unwrap_or_else(error_response))
}

async fn route(mut request: Request<Incoming>, service: Arc<Service>) -> ApiResult<Response<Body>> {
    let Some(resolved) = crate::catalog::resolve(request.method(), request.uri().path()) else {
        return Err(ApiError::Status(404, "route not found".into()));
    };
    let id = resolved.id.unwrap_or_default();
    match resolved.operation {
        Operation::Commands => Ok(json_response(200, crate::catalog::catalog())),
        Operation::Images => Ok(json_response(200, json!({"images": service.image_names()}))),
        Operation::AccessLog => {
            let params = query(&request);
            let now = crate::service::now_ms();
            let number = |name: &str, default: u64| -> ApiResult<u64> {
                optional_param(&params, name)
                    .map(|value| {
                        value
                            .parse()
                            .map_err(|_| ApiError::Status(400, format!("invalid {name}")))
                    })
                    .unwrap_or(Ok(default))
            };
            let since_ms = number("sinceMs", now.saturating_sub(2 * 3_600_000))?;
            let until_ms = number("untilMs", now)?;
            let limit = usize::try_from(number("limit", 10_000)?).unwrap_or(usize::MAX);
            let cursor = optional_param(&params, "cursor")
                .map(|value| {
                    crate::access_log::Cursor::parse(&value)
                        .ok_or_else(|| ApiError::Status(400, "invalid cursor".into()))
                })
                .transpose()?;
            let page = service
                .access_entries(since_ms, until_ms, limit, cursor)
                .await?;
            Ok(json_response(
                200,
                json!({
                    "entries": page.entries,
                    "truncated": page.truncated,
                    "nextCursor": page.next_cursor.map(|cursor| cursor.to_string()),
                }),
            ))
        }
        Operation::Create => {
            let body: CreateRequest = read_json(request.into_body()).await?;
            Ok(json_response(
                200,
                serde_json::to_value(service.create(body).await?).unwrap(),
            ))
        }
        Operation::List => {
            let params = query(&request);
            let limit = optional_param(&params, "limit")
                .and_then(|value| value.parse().ok())
                .unwrap_or(1000usize);
            let filter = crate::model::label_filter(
                params
                    .iter()
                    .filter(|(key, _)| key == "label")
                    .map(|(_, value)| value.as_str()),
            )
            .map_err(ApiError::bad_request)?;
            let (sandboxes, truncated) = service.list(&filter, limit).await?;
            Ok(json_response(
                200,
                json!({"sandboxes": sandboxes, "truncated": truncated}),
            ))
        }
        Operation::Get => {
            let entry = service.entry(&id).await?;
            Ok(json_response(
                200,
                serde_json::to_value(service.summary(&entry).await?).unwrap(),
            ))
        }
        Operation::Update => {
            let body: UpdateRequest = read_json(request.into_body()).await?;
            Ok(json_response(
                200,
                serde_json::to_value(service.update(&id, body).await?).unwrap(),
            ))
        }
        Operation::Delete => {
            service.delete(&id).await?;
            Ok(json_response(200, json!({"deleted": true})))
        }
        Operation::Start => Ok(json_response(
            200,
            serde_json::to_value(service.start(&id).await?).unwrap(),
        )),
        Operation::Stop => Ok(json_response(
            200,
            serde_json::to_value(service.stop(&id).await?).unwrap(),
        )),
        Operation::SetSecrets => {
            let body: SecretsRequest = read_json(request.into_body()).await?;
            let mounted = service.put_secrets(&id, body.secrets).await?;
            Ok(json_response(200, json!({"mountedEnvVars": mounted})))
        }
        Operation::DeleteSecrets => {
            let body: DeleteSecretsRequest = read_json(request.into_body()).await?;
            let deleted = service.delete_secrets(&body.names).await?;
            Ok(json_response(200, json!({"deletedNames": deleted})))
        }
        Operation::Expose => {
            let body: crate::model::PreviewRequest = read_json(request.into_body()).await?;
            service.entry(&id).await?;
            let exp_ms =
                crate::service::now_ms() + u64::from(body.ttl_seconds.clamp(1, 3600)) * 1000;
            let token = service.preview_key.sign(&crate::preview::PreviewClaims {
                sandbox_id: id,
                port: body.port,
                exp_ms,
                host_view: false,
            });
            let path = format!("/v1/preview/{token}/");
            Ok(json_response(
                200,
                json!({"url": service.public_link(&path), "path": path, "expiresAtMs": exp_ms}),
            ))
        }
        Operation::Desktop => {
            let body: crate::model::DesktopRequest = read_json(request.into_body()).await?;
            desktop(&service, id, body).await
        }
        Operation::Exec => {
            let body: ExecRequest = read_json(request.into_body()).await?;
            exec(service, &id, body, false).await
        }
        Operation::Run => {
            let body: RunRequest = read_json(request.into_body()).await?;
            run(service, body).await
        }
        Operation::Shell => {
            let upgrading = request
                .headers()
                .get(hyper::header::UPGRADE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.eq_ignore_ascii_case(crate::terminal::PROTOCOL));
            if !upgrading {
                return Err(ApiError::Status(
                    426,
                    "shell is interactive: use the kumoba CLI 0.3 or later, or upgrade the connection to kumoba-shell.v1".into(),
                ));
            }
            let upgrade = hyper::upgrade::on(&mut request);
            let body: crate::model::ShellRequest = read_json(request.into_body()).await?;
            shell(service, id, body, upgrade).await
        }
        Operation::ReadFile => {
            let params = query(&request);
            let user = optional_param(&params, "user");
            download(service, &id, &required_param(&params, "path")?, user).await
        }
        Operation::WriteFile => {
            let params = query(&request);
            let path = required_param(&params, "path")?;
            let user = optional_param(&params, "user");
            upload(service, &id, &path, user.as_deref(), request.into_body()).await
        }
        Operation::ListFiles => {
            let params = query(&request);
            let (_entry, agent, _) = service.running_agent(&id, None).await?;
            let user = optional_param(&params, "user");
            let entries = agent
                .list(&required_param(&params, "path")?, user.as_deref())
                .await?;
            Ok(json_response(200, json!({"entries": entries})))
        }
    }
}

const PATH_LOOKUP: &str = "/usr/bin/env";

async fn shell(
    service: Arc<Service>,
    id: String,
    body: crate::model::ShellRequest,
    upgrade: hyper::upgrade::OnUpgrade,
) -> ApiResult<Response<Body>> {
    let login = body.argv.is_empty();
    let (program, args) = match body.argv.split_first() {
        None => ("/bin/sh".to_string(), Vec::new()),
        Some((program, args)) if program.starts_with('/') => (program.clone(), args.to_vec()),
        Some(_) => (PATH_LOOKUP.to_string(), body.argv.clone()),
    };
    let mut command = Command::new(program)
        .args(args)
        .timeout(Duration::from_secs(3600));
    for (name, value) in body.env {
        command = command.env(name, value);
    }
    if let Some(cwd) = body.cwd {
        command = command.cwd(kumoba::GuestPath::new(cwd)?);
    }
    if let Some(user) = body.user {
        command = command.user(user);
    }
    let entry = service.entry(&id).await?;
    if !service.supports_shell(&entry).await {
        return Err(ApiError::Status(
            422,
            "this sandbox's image has no shell support; boot it from an image of kumoba 0.3 or later"
                .into(),
        ));
    }
    let (entry, agent, command) = service.running_agent(&id, Some(command)).await?;
    let (mut sender, mut receiver) = agent
        .shell(
            command.expect("prepared command"),
            body.rows,
            body.cols,
            login,
        )
        .await?;
    tokio::spawn(async move {
        let _operation = entry.begin();
        let Ok(upgraded) = upgrade.await else {
            return;
        };
        let (mut client_reader, mut client_writer) =
            tokio::io::split(hyper_util::rt::TokioIo::new(upgraded));
        let from_client = async {
            while let Ok(Some(frame)) = crate::terminal::read_frame(&mut client_reader).await {
                let input = match frame {
                    crate::terminal::Frame::Data(data) => {
                        kumoba::ShellInput::Data(STANDARD.encode(data))
                    }
                    crate::terminal::Frame::Resize { rows, cols } => {
                        kumoba::ShellInput::Resize { rows, cols }
                    }
                    crate::terminal::Frame::Eof => kumoba::ShellInput::Eof,
                    crate::terminal::Frame::Exit(_) => break,
                };
                if sender.send(&input).await.is_err() {
                    break;
                }
            }
        };
        let to_client = async {
            loop {
                let frame = match receiver.next().await {
                    Ok(Some(kumoba::ShellOutput::Data(data))) => match STANDARD.decode(data) {
                        Ok(bytes) => crate::terminal::Frame::Data(bytes),
                        Err(_) => break,
                    },
                    Ok(Some(kumoba::ShellOutput::Exit { code, signal })) => {
                        let status = code.or(signal.map(|signal| 128 + signal)).unwrap_or(1);
                        let _ = crate::terminal::write_frame(
                            &mut client_writer,
                            &crate::terminal::Frame::Exit(status),
                        )
                        .await;
                        break;
                    }
                    Ok(None) | Err(_) => break,
                };
                if crate::terminal::write_frame(&mut client_writer, &frame)
                    .await
                    .is_err()
                {
                    break;
                }
            }
        };
        tokio::select! {
            () = from_client => {}
            () = to_client => {}
        }
    });
    Response::builder()
        .status(101)
        .header(hyper::header::CONNECTION, "upgrade")
        .header(hyper::header::UPGRADE, crate::terminal::PROTOCOL)
        .body(Full::new(Bytes::new()).boxed())
        .map_err(|error| ApiError::Status(500, error.to_string()))
}
const DESKTOP_HELPER: &str = "/usr/local/bin/kumoba-desktop";
const DESKTOP_VNC_PORT: u16 = 6080;
const DESKTOP_STREAM_PORT: u16 = 6090;

const STREAM_VIEW: (u16, &str) = (DESKTOP_STREAM_PORT, "stream");
const VNC_VIEW: (u16, &str) = (DESKTOP_VNC_PORT, "vnc");

fn desktop_view(requested: crate::model::DesktopViewer, processes: &Value) -> (u16, &'static str) {
    let running = processes.get("view").and_then(Value::as_bool) == Some(true);
    if requested == crate::model::DesktopViewer::Stream && running {
        STREAM_VIEW
    } else {
        VNC_VIEW
    }
}

async fn listening(agent: &kumoba::Agent, port: u16, limit: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        if agent.connect(port).await.is_ok() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn desktop_link(
    service: &Service,
    id: String,
    view: (u16, &str),
    host_view: bool,
    ttl_seconds: u32,
    processes: &Value,
) -> Response<Body> {
    let (port, name) = view;
    let exp_ms = crate::service::now_ms() + u64::from(ttl_seconds.clamp(1, 3600)) * 1000;
    let token = service.preview_key.sign(&crate::preview::PreviewClaims {
        sandbox_id: id,
        port,
        exp_ms,
        host_view,
    });
    let base = format!("/v1/preview/{token}/");
    let viewer = if view == VNC_VIEW {
        format!(
            "{base}vnc.html?autoconnect=1&resize=remote&path={}",
            url::form_urlencoded::byte_serialize(base.trim_start_matches('/').as_bytes())
                .collect::<String>()
        )
    } else {
        base
    };
    json_response(
        200,
        json!({
            "url": service.public_link(&viewer),
            "path": viewer,
            "expiresAtMs": exp_ms,
            "processes": processes,
            "viewer": name,
        }),
    )
}

async fn desktop(
    service: &Service,
    id: String,
    body: crate::model::DesktopRequest,
) -> ApiResult<Response<Body>> {
    if let Some((entry, _)) = service.live_view(&id, true).await? {
        let _operation = entry.begin();
        return Ok(desktop_link(
            service,
            id,
            STREAM_VIEW,
            true,
            body.ttl_seconds,
            &json!({"view": true}),
        ));
    }
    let mut command = Command::new(DESKTOP_HELPER)
        .arg("start")
        .timeout(Duration::from_secs(90))
        .output_limit(64 * 1024);
    if let Some(user) = body.user {
        command = command.user(user);
    }
    let (entry, agent, command) = service.running_agent(&id, Some(command)).await?;
    let _operation = entry.begin();
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let no_desktop = |detail: String| {
        ApiError::Status(
            422,
            format!("the desktop did not start; this image may not include one ({detail})"),
        )
    };
    let summary = agent
        .exec_streamed(command.expect("prepared command"), &mut stdout, &mut stderr)
        .await
        .map_err(|error| no_desktop(error.to_string()))?;
    if !matches!(summary.termination, Termination::Exited(0)) {
        let detail = String::from_utf8_lossy(if stderr.is_empty() { &stdout } else { &stderr })
            .trim()
            .to_string();
        return Err(no_desktop(detail));
    }
    let processes = String::from_utf8_lossy(&stdout)
        .lines()
        .last()
        .and_then(|line| serde_json::from_str::<Value>(line).ok())
        .unwrap_or(Value::Null);
    let mut view = desktop_view(body.viewer, &processes);
    if view == STREAM_VIEW && !listening(&agent, view.0, Duration::from_secs(10)).await {
        view = VNC_VIEW;
    }
    let (port, name) = view;
    if !listening(&agent, port, Duration::from_secs(30)).await {
        return Err(no_desktop(format!(
            "the {name} live view did not start listening"
        )));
    }
    Ok(desktop_link(
        service,
        id,
        view,
        false,
        body.ttl_seconds,
        &processes,
    ))
}

async fn run(service: Arc<Service>, body: RunRequest) -> ApiResult<Response<Body>> {
    let (create, exec_request) = body.split();
    let id = service.create(create).await?.meta.id;
    let started = match service.start(&id).await {
        Ok(_) => exec(service.clone(), &id, exec_request, true).await,
        Err(error) => Err(error),
    };
    if started.is_err() {
        let _ = service.delete(&id).await;
    }
    started
}

struct EventWriter {
    sender: PollSender<Bytes>,
    kind: &'static str,
}

impl AsyncWrite for EventWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.sender.poll_reserve(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(_)) => Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into())),
            Poll::Ready(Ok(())) => {
                let mut line =
                    json!({"type": self.kind, "data": STANDARD.encode(data)}).to_string();
                line.push('\n');
                match self.sender.send_item(Bytes::from(line)) {
                    Ok(()) => Poll::Ready(Ok(data.len())),
                    Err(_) => Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into())),
                }
            }
        }
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

async fn exec(
    service: Arc<Service>,
    id: &str,
    body: ExecRequest,
    ephemeral: bool,
) -> ApiResult<Response<Body>> {
    let Some((program, args)) = body.argv.split_first() else {
        return Err(ApiError::bad_request("argv must not be empty"));
    };
    let (program, args) = if program.starts_with('/') {
        (program.clone(), args.to_vec())
    } else {
        (PATH_LOOKUP.to_string(), body.argv.clone())
    };
    let mut command = Command::new(program)
        .args(args)
        .timeout(Duration::from_millis(body.timeout_ms))
        .output_limit(kumoba_protocol::MAX_OUTPUT_LIMIT);
    for (name, value) in body.env {
        command = command.env(name, value);
    }
    if let Some(cwd) = body.cwd {
        command = command.cwd(kumoba::GuestPath::new(cwd)?);
    }
    if let Some(user) = body.user {
        command = command.user(user);
    }
    let (entry, agent, command) = service.running_agent(id, Some(command)).await?;
    let command = command.expect("prepared command");
    let (sender, receiver) = mpsc::channel::<Bytes>(32);
    let id = id.to_string();
    if ephemeral {
        let mut line = json!({"type": "sandbox", "id": id}).to_string();
        line.push('\n');
        let _ = sender.try_send(Bytes::from(line));
    }
    tokio::spawn(async move {
        let operation = entry.begin();
        let mut stdout = EventWriter {
            sender: PollSender::new(sender.clone()),
            kind: "stdout",
        };
        let mut stderr = EventWriter {
            sender: PollSender::new(sender.clone()),
            kind: "stderr",
        };
        let result = tokio::select! {
            result = agent.exec_streamed(command, &mut stdout, &mut stderr) => Some(result),
            () = sender.closed() => None,
        };
        drop(operation);
        let event = result.map(|result| match result {
            Ok(summary) => match summary.termination {
                Termination::Exited(code) => {
                    json!({"type": "exit", "exitCode": code, "timedOut": false})
                }
                Termination::Signaled(signal) => {
                    json!({"type": "exit", "exitCode": 128 + signal, "signal": signal, "timedOut": false})
                }
                Termination::TimedOut => {
                    json!({"type": "exit", "exitCode": null, "timedOut": true})
                }
            },
            Err(error) => json!({"type": "error", "message": error.to_string()}),
        });
        if ephemeral && let Err(error) = service.delete(&id).await {
            crate::service::log("run_delete_failed", &id, Some(error));
        }
        if let Some(event) = event {
            let mut line = event.to_string();
            line.push('\n');
            let _ = sender.send(Bytes::from(line)).await;
        }
    });
    Ok(stream_response("application/x-ndjson", receiver))
}

async fn download(
    service: Arc<Service>,
    id: &str,
    path: &str,
    user: Option<String>,
) -> ApiResult<Response<Body>> {
    let (entry, agent, _) = service.running_agent(id, None).await?;
    let first = agent.read_at(path, 0, MAX_DATA, user.as_deref()).await?;
    let path = path.to_string();
    let (sender, receiver) = mpsc::channel::<Bytes>(4);
    let size = first.size;
    tokio::spawn(async move {
        let _operation = entry.begin();
        let mut offset = first.data.len() as u64;
        if sender.send(Bytes::from(first.data)).await.is_err() {
            return;
        }
        while offset < size {
            match agent
                .read_at(&path, offset, MAX_DATA, user.as_deref())
                .await
            {
                Ok(chunk) if !chunk.data.is_empty() => {
                    offset += chunk.data.len() as u64;
                    if sender.send(Bytes::from(chunk.data)).await.is_err() {
                        return;
                    }
                }
                _ => return,
            }
        }
    });
    let mut response = stream_response("application/octet-stream", receiver);
    response.headers_mut().insert(
        "x-file-size",
        size.to_string().parse().expect("numeric header"),
    );
    Ok(response)
}

async fn upload(
    service: Arc<Service>,
    id: &str,
    path: &str,
    user: Option<&str>,
    mut body: Incoming,
) -> ApiResult<Response<Body>> {
    let (entry, agent, _) = service.running_agent(id, None).await?;
    let _operation = entry.begin();
    let mut buffer = BytesMut::with_capacity(MAX_DATA);
    let mut offset = 0u64;
    let mut first = true;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|error| ApiError::bad_request(error.to_string()))?;
        let Ok(data) = frame.into_data() else {
            continue;
        };
        buffer.extend_from_slice(&data);
        while buffer.len() >= MAX_DATA {
            let chunk = buffer.split_to(MAX_DATA);
            agent.write_at(path, offset, &chunk, first, user).await?;
            first = false;
            offset += chunk.len() as u64;
        }
    }
    if first || !buffer.is_empty() {
        agent.write_at(path, offset, &buffer, first, user).await?;
        offset += buffer.len() as u64;
    }
    Ok(json_response(200, json!({"written": offset})))
}

const PREVIEW_BODY_LIMIT: usize = 1024 * 1024;
const PREVIEW_REQUEST_LIMIT: usize = 10 * 1024 * 1024;
const FORWARDED_REQUEST_HEADERS: &[&str] = &[
    "accept",
    "accept-encoding",
    "accept-language",
    "connection",
    "content-type",
    "sec-websocket-extensions",
    "sec-websocket-key",
    "sec-websocket-protocol",
    "sec-websocket-version",
    "upgrade",
    "user-agent",
];
const UPGRADE_RESPONSE_HEADERS: &[&str] = &[
    "connection",
    "sec-websocket-accept",
    "sec-websocket-extensions",
    "sec-websocket-protocol",
    "upgrade",
];
const PLAIN_RESPONSE_HEADERS: &[&str] = &[];

const GUEST_ORIGIN: &str = "http://guest.invalid";

fn declared_length(head: &str) -> Option<usize> {
    head.split("\r\n").skip(1).find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse().ok())
            .flatten()
    })
}

fn preview_location(token: &str, requested: &str, location: &str) -> Option<String> {
    if location.chars().any(char::is_control) {
        return None;
    }
    let base = url::Url::parse(&format!("{GUEST_ORIGIN}{requested}")).ok()?;
    let resolved = base.join(location).ok()?;
    if resolved.scheme() != "http"
        || resolved.host_str() != Some("guest.invalid")
        || resolved.port().is_some()
        || !resolved.username().is_empty()
        || resolved.password().is_some()
    {
        return None;
    }
    let mut target = format!(
        "/v1/preview/{token}/{}",
        resolved.path().trim_start_matches('/')
    );
    if let Some(query) = resolved.query() {
        target.push('?');
        target.push_str(query);
    }
    if let Some(fragment) = resolved.fragment() {
        target.push('#');
        target.push_str(fragment);
    }
    Some(target)
}
const BROWSER_RESPONSE_HEADERS: &[&str] = &[
    "content-type",
    "content-encoding",
    "content-language",
    "location",
];

async fn read_head(stream: &mut tokio::net::UnixStream) -> ApiResult<(Vec<u8>, Vec<u8>)> {
    use tokio::io::AsyncReadExt;
    let mut buffer = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        let count = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk))
            .await
            .map_err(|_| ApiError::Status(504, "guest preview did not respond".into()))??;
        if count == 0 {
            return Err(ApiError::Status(
                502,
                "guest preview closed the connection".into(),
            ));
        }
        buffer.extend_from_slice(&chunk[..count]);
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            let rest = buffer.split_off(end + 4);
            return Ok((buffer, rest));
        }
        if buffer.len() > 64 * 1024 {
            return Err(ApiError::Status(
                502,
                "guest preview header too large".into(),
            ));
        }
    }
}

async fn forward_request<B>(
    tunnel: &mut tokio::net::UnixStream,
    request: &mut Request<B>,
    target: &str,
    port: u16,
) -> ApiResult<()>
where
    B: hyper::body::Body<Data = Bytes> + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    use tokio::io::AsyncWriteExt;
    let upgrading = request.headers().contains_key("upgrade");
    let body = if upgrading {
        Bytes::new()
    } else {
        tokio::time::timeout(
            Duration::from_secs(30),
            Limited::new(request.body_mut(), PREVIEW_REQUEST_LIMIT).collect(),
        )
        .await
        .map_err(|_| ApiError::Status(408, "request body timed out".into()))?
        .map_err(|error| {
            if error.is::<http_body_util::LengthLimitError>() {
                ApiError::Status(413, "preview request body too large".into())
            } else {
                ApiError::bad_request(error.to_string())
            }
        })?
        .to_bytes()
    };
    let framed = ["content-length", "transfer-encoding"]
        .iter()
        .any(|name| request.headers().contains_key(*name));
    let version = if upgrading { "HTTP/1.1" } else { "HTTP/1.0" };
    let mut head = format!("{} {target} {version}\r\n", request.method());
    for (name, value) in request.headers() {
        if !FORWARDED_REQUEST_HEADERS.contains(&name.as_str())
            || (!upgrading && name.as_str() == "connection")
        {
            continue;
        }
        if let Ok(value) = value.to_str() {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
    }
    if framed || !body.is_empty() {
        head.push_str(&format!("content-length: {}\r\n", body.len()));
    }
    head.push_str(&format!("host: 127.0.0.1:{port}\r\n\r\n"));
    tunnel.write_all(head.as_bytes()).await?;
    if !body.is_empty() {
        let _ = tokio::time::timeout(Duration::from_secs(30), tunnel.write_all(&body)).await;
    }
    Ok(())
}

async fn preview(
    mut request: Request<Incoming>,
    service: Arc<Service>,
    browse: bool,
) -> ApiResult<Response<Body>> {
    use tokio::io::AsyncWriteExt;
    let path = request
        .uri()
        .path()
        .trim_start_matches("/v1/preview/")
        .to_string();
    let (token, rest) = path.split_once('/').unwrap_or((path.as_str(), ""));
    let claims = service
        .preview_key
        .verify(token, crate::service::now_ms())
        .ok_or_else(|| ApiError::Status(403, "preview token is invalid or expired".into()))?;
    let (entry, agent, _) = service.running_agent(&claims.sandbox_id, None).await?;
    let mut tunnel = if claims.host_view {
        let Some((_, live)) = service.live_view(&claims.sandbox_id, false).await? else {
            return Err(ApiError::Status(409, "sandbox is not started".into()));
        };
        tokio::net::UnixStream::connect(&live).await.map_err(|_| {
            ApiError::Status(
                502,
                "the live view is not running; ask for the desktop link again".into(),
            )
        })?
    } else {
        agent.connect(claims.port).await?
    };
    let target = match request.uri().query() {
        Some(query) => format!("/{rest}?{query}"),
        None => format!("/{rest}"),
    };
    forward_request(&mut tunnel, &mut request, &target, claims.port).await?;
    let (response_head, leftover) = read_head(&mut tunnel).await?;
    let text = String::from_utf8_lossy(&response_head);
    let mut lines = text.split("\r\n");
    let status: u16 = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| ApiError::Status(502, "guest preview sent an invalid status".into()))?;
    let allowed: &[&str] = match (status, browse) {
        (101, _) => UPGRADE_RESPONSE_HEADERS,
        (_, true) => BROWSER_RESPONSE_HEADERS,
        (_, false) => PLAIN_RESPONSE_HEADERS,
    };
    let mut builder = Response::builder()
        .status(status)
        .header("cache-control", "no-store")
        .header("x-content-type-options", "nosniff");
    if status != 101 && !browse {
        builder = builder
            .header("content-type", "application/octet-stream")
            .header("content-security-policy", "sandbox")
            .header("content-disposition", "attachment");
    }
    for line in lines.filter(|line| !line.is_empty()) {
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            if name == "location" && allowed.contains(&"location") {
                if let Some(target) = preview_location(token, &target, value.trim()) {
                    builder = builder.header(name, target);
                }
            } else if allowed.contains(&name.as_str()) {
                builder = builder.header(name, value.trim());
            }
        }
    }
    if status != 101 {
        use tokio::io::AsyncReadExt;
        let mut body = leftover;
        let expected = declared_length(&text);
        let wanted = expected.map_or(PREVIEW_BODY_LIMIT + 1, |length| {
            length.min(PREVIEW_BODY_LIMIT + 1)
        });
        let remaining = wanted.saturating_sub(body.len()) as u64;
        let _ = tokio::time::timeout(
            Duration::from_secs(30),
            (&mut tunnel).take(remaining).read_to_end(&mut body),
        )
        .await;
        if expected.is_some_and(|length| length > PREVIEW_BODY_LIMIT) {
            return Err(ApiError::Status(502, "guest preview body too large".into()));
        }
        if body.len() > PREVIEW_BODY_LIMIT {
            return Err(ApiError::Status(502, "guest preview body too large".into()));
        }
        return builder
            .body(Full::new(Bytes::from(body)).boxed())
            .map_err(|error| ApiError::Status(502, error.to_string()));
    }
    let upgrade = hyper::upgrade::on(&mut request);
    tokio::spawn(async move {
        let _operation = entry.begin();
        let Ok(upgraded) = upgrade.await else {
            return;
        };
        let mut client = hyper_util::rt::TokioIo::new(upgraded);
        if !leftover.is_empty() && client.write_all(&leftover).await.is_err() {
            return;
        }
        let _ = tokio::io::copy_bidirectional(&mut client, &mut tunnel).await;
    });
    builder
        .body(Full::new(Bytes::new()).boxed())
        .map_err(|error| ApiError::Status(502, error.to_string()))
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full, StreamBody};
    use hyper::{Request, body::Frame};
    use std::convert::Infallible;

    async fn guest_receives<B>(mut request: Request<B>) -> (u16, String)
    where
        B: hyper::body::Body<Data = Bytes> + Unpin,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut tunnel, mut guest) = tokio::net::UnixStream::pair().unwrap();
        let reader = tokio::spawn(async move {
            let mut received = Vec::new();
            guest.read_to_end(&mut received).await.map(|_| received)
        });
        let status = match super::forward_request(&mut tunnel, &mut request, "/submit", 8080).await
        {
            Ok(()) => 200,
            Err(error) => error.status(),
        };
        tunnel.shutdown().await.unwrap();
        let received = reader.await.unwrap().unwrap();
        (status, String::from_utf8(received).unwrap())
    }

    fn chunks(
        parts: Vec<&'static [u8]>,
    ) -> StreamBody<impl tokio_stream::Stream<Item = Result<Frame<Bytes>, Infallible>> + Unpin>
    {
        StreamBody::new(tokio_stream::iter(
            parts
                .into_iter()
                .map(|part| Ok(Frame::data(Bytes::from_static(part)))),
        ))
    }

    #[tokio::test]
    async fn a_preview_post_reaches_the_guest_with_its_body() {
        let request = Request::post("/v1/preview/tok/submit")
            .header("content-type", "application/json")
            .header("content-length", "12")
            .body(Full::new(Bytes::from_static(b"{\"name\":\"a\"}")))
            .unwrap();
        assert_eq!(
            guest_receives(request).await,
            (
                200,
                "POST /submit HTTP/1.0\r\ncontent-type: application/json\r\ncontent-length: 12\r\nhost: 127.0.0.1:8080\r\n\r\n{\"name\":\"a\"}".into()
            )
        );
    }

    #[tokio::test]
    async fn a_chunked_preview_body_reaches_the_guest_with_a_fixed_length() {
        let request = Request::put("/v1/preview/tok/submit")
            .header("transfer-encoding", "chunked")
            .body(chunks(vec![b"one ", b"two ", b"three"]))
            .unwrap();
        assert_eq!(
            guest_receives(request).await,
            (
                200,
                "PUT /submit HTTP/1.0\r\ncontent-length: 13\r\nhost: 127.0.0.1:8080\r\n\r\none two three".into()
            )
        );
    }

    #[tokio::test]
    async fn an_empty_preview_post_still_says_it_has_no_body() {
        let request = Request::post("/v1/preview/tok/submit")
            .header("content-length", "0")
            .body(Full::new(Bytes::new()))
            .unwrap();
        assert_eq!(
            guest_receives(request).await,
            (
                200,
                "POST /submit HTTP/1.0\r\ncontent-length: 0\r\nhost: 127.0.0.1:8080\r\n\r\n".into()
            )
        );
    }

    #[tokio::test]
    async fn a_preview_get_is_relayed_without_a_length() {
        let request = Request::get("/v1/preview/tok/submit")
            .header("accept", "text/html")
            .header("connection", "keep-alive")
            .body(Full::new(Bytes::new()))
            .unwrap();
        assert_eq!(
            guest_receives(request).await,
            (
                200,
                "GET /submit HTTP/1.0\r\naccept: text/html\r\nhost: 127.0.0.1:8080\r\n\r\n".into()
            )
        );
    }

    #[tokio::test]
    async fn a_preview_upgrade_is_relayed_without_reading_a_body() {
        let request = Request::get("/v1/preview/tok/submit")
            .header("connection", "Upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
            .header("sec-websocket-version", "13")
            .body(chunks(vec![b"not a body"]))
            .unwrap();
        assert_eq!(
            guest_receives(request).await,
            (
                200,
                "GET /submit HTTP/1.1\r\nconnection: Upgrade\r\nupgrade: websocket\r\nsec-websocket-key: dGhlIHNhbXBsZSBub25jZQ==\r\nsec-websocket-version: 13\r\nhost: 127.0.0.1:8080\r\n\r\n".into()
            )
        );
    }

    #[tokio::test]
    async fn a_preview_body_over_the_cap_is_refused_before_the_guest_sees_it() {
        let limit = super::PREVIEW_REQUEST_LIMIT;
        let whole = Request::post("/v1/preview/tok/submit")
            .body(Full::new(Bytes::from(vec![b'x'; limit + 1])))
            .unwrap();
        assert_eq!(guest_receives(whole).await, (413, String::new()));
        let half: &'static [u8] = vec![b'x'; limit / 2 + 1].leak();
        let streamed = Request::post("/v1/preview/tok/submit")
            .body(chunks(vec![half, half]))
            .unwrap();
        assert_eq!(guest_receives(streamed).await, (413, String::new()));
        let exact = Request::post("/v1/preview/tok/submit")
            .body(Full::new(Bytes::from(vec![b'x'; limit])))
            .unwrap();
        let (status, received) = guest_receives(exact).await;
        assert_eq!(status, 200);
        assert!(received.ends_with(&"x".repeat(limit)));
    }

    #[tokio::test]
    async fn a_guest_that_answers_before_reading_the_body_is_still_heard() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut tunnel, mut guest) = tokio::net::UnixStream::pair().unwrap();
        tokio::spawn(async move {
            let mut head = [0u8; 64];
            guest.read_exact(&mut head).await.unwrap();
            guest
                .write_all(b"HTTP/1.0 401 Unauthorized\r\ncontent-length: 0\r\n\r\n")
                .await
                .unwrap();
        });
        let mut request = Request::post("/v1/preview/tok/submit")
            .body(Full::new(Bytes::from(vec![b'x'; 1024 * 1024])))
            .unwrap();
        super::forward_request(&mut tunnel, &mut request, "/submit", 8080)
            .await
            .unwrap();
        let (head, _) = super::read_head(&mut tunnel).await.unwrap();
        assert!(head.starts_with(b"HTTP/1.0 401 "));
    }

    #[tokio::test]
    async fn a_web_server_in_the_guest_reads_the_relayed_body() {
        use hyper_util::rt::TokioIo;
        use tokio::io::AsyncReadExt;
        let (mut tunnel, guest) = tokio::net::UnixStream::pair().unwrap();
        let server = tokio::spawn(async move {
            let echo =
                hyper::service::service_fn(|request: Request<hyper::body::Incoming>| async move {
                    let body = request.into_body().collect().await?.to_bytes();
                    Ok::<_, hyper::Error>(hyper::Response::new(Full::new(body)))
                });
            hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(guest), echo)
                .await
        });
        let mut request = Request::post("/v1/preview/tok/submit")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(chunks(vec![b"name=a", b"&note=b"]))
            .unwrap();
        super::forward_request(&mut tunnel, &mut request, "/submit", 8080)
            .await
            .unwrap();
        let mut response = Vec::new();
        tunnel.read_to_end(&mut response).await.unwrap();
        server.await.unwrap().unwrap();
        assert!(
            String::from_utf8(response)
                .unwrap()
                .ends_with("\r\n\r\nname=a&note=b"),
        );
    }

    #[test]
    fn the_desktop_link_prefers_the_stream_viewer_only_when_it_runs() {
        use super::{STREAM_VIEW, VNC_VIEW, desktop_view};
        use crate::model::DesktopViewer::{Stream, Vnc};
        use serde_json::{Value, json};
        let both = json!({"view": true, "novnc": true});
        assert_eq!(desktop_view(Stream, &both), STREAM_VIEW);
        assert_eq!(desktop_view(Vnc, &both), VNC_VIEW);
        assert_eq!(desktop_view(Stream, &json!({"novnc": true})), VNC_VIEW);
        assert_eq!(desktop_view(Stream, &json!({"view": false})), VNC_VIEW);
        assert_eq!(desktop_view(Stream, &Value::Null), VNC_VIEW);
    }

    #[test]
    fn reads_the_declared_body_length() {
        use super::declared_length;
        assert_eq!(
            declared_length("HTTP/1.0 200 OK\r\nContent-Type: text/html\r\nContent-Length: 42\r\n"),
            Some(42)
        );
        assert_eq!(
            declared_length("HTTP/1.0 200 OK\r\ncontent-length:7"),
            Some(7)
        );
        assert_eq!(
            declared_length("HTTP/1.0 200 OK\r\nContent-Type: text/html"),
            None
        );
    }

    #[test]
    fn preview_redirects_stay_inside_their_preview() {
        use super::preview_location;
        let at = |location: &str| preview_location("tok", "/app/page?x=1", location);
        assert_eq!(
            at("/login?next=1").as_deref(),
            Some("/v1/preview/tok/login?next=1")
        );
        assert_eq!(at("step2").as_deref(), Some("/v1/preview/tok/app/step2"));
        assert_eq!(at("../up#top").as_deref(), Some("/v1/preview/tok/up#top"));
        for hostile in [
            "https://evil.example/",
            "//evil.example/",
            "/\\evil.example",
            "/\t/evil.example",
            "javascript:alert(1)",
            "http://guest.invalid:81/",
            "http://user@guest.invalid/",
            "data:text/html,hi",
        ] {
            assert_eq!(at(hostile), None, "{hostile}");
        }
    }
}
