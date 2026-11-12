use crate::secret_https::Authority;
use crate::{
    Command, EgressEndpoint, GuestPath, HttpConnectPolicy, Network, Runtime, RuntimeConfig,
    Sandbox, SandboxConfig, SecretStore, SecretValue, StopOutcome,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    Request, Response,
    body::{Body, Frame, Incoming},
};
use hyper_util::rt::TokioIo;
use std::{
    collections::BTreeMap,
    convert::Infallible,
    io,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, task::JoinSet};
use tokio_rustls::TlsAcceptor;

fn required(name: &str) -> PathBuf {
    std::env::var_os(name)
        .unwrap_or_else(|| panic!("set {name} using scripts/test-local.sh"))
        .into()
}
fn endpoint(host: &str) -> EgressEndpoint {
    EgressEndpoint::new(host, 443).unwrap()
}
fn policy(hosts: &[&str]) -> HttpConnectPolicy {
    HttpConnectPolicy::new(hosts.iter().map(|host| endpoint(host))).unwrap()
}
fn guest_path(path: &str) -> GuestPath {
    GuestPath::new(path).unwrap()
}

struct LlmEvents {
    stage: usize,
    credential: String,
    wait: std::pin::Pin<Box<tokio::time::Sleep>>,
}
impl Body for LlmEvents {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        use std::{future::Future, task::Poll};
        let bytes = match self.stage {
            0 => Bytes::from_static(b"data: ready\n\n"),
            1 | 2 => {
                if self.wait.as_mut().poll(cx).is_pending() {
                    return Poll::Pending;
                }
                let split = self.credential.len() / 2;
                if self.stage == 1 {
                    self.wait
                        .as_mut()
                        .reset(tokio::time::Instant::now() + Duration::from_secs(1));
                    Bytes::from(format!("data: {}", &self.credential[..split]))
                } else {
                    Bytes::from(format!("{}\n\n", &self.credential[split..]))
                }
            }
            _ => return Poll::Ready(None),
        };
        self.stage += 1;
        Poll::Ready(Some(Ok(Frame::data(bytes))))
    }
}
#[derive(Default)]
struct StreamOutput {
    first: Option<std::time::Instant>,
    bytes: Vec<u8>,
}
impl tokio::io::AsyncWrite for StreamOutput {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        self.first.get_or_insert_with(std::time::Instant::now);
        self.bytes.extend_from_slice(bytes);
        std::task::Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

struct TestUpstream {
    address: std::net::SocketAddr,
    records: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for TestUpstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl TestUpstream {
    async fn start(authority: &Authority, host: &str) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let acceptor = TlsAcceptor::from(authority.test_server_config(host));
        let records = Arc::new(Mutex::new(Vec::new()));
        let output = records.clone();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((stream,_)) = accepted else { break; };
                        let acceptor = acceptor.clone();
                        let records = output.clone();
                        connections.spawn(async move {
                            let Ok(tls) = acceptor.accept(stream).await else { return; };
                            let service = hyper::service::service_fn(move |request:Request<Incoming>| {
                                let records = records.clone();
                                async move {
                                    let path = request.uri().path().to_owned();
                                    let authorization = request.headers().get("authorization").and_then(|value| value.to_str().ok()).unwrap_or("").to_owned();
                                    records.lock().unwrap().push(authorization.clone());
                                    let uri = request.uri().to_string();
                                    let body = request.into_body().collect().await.unwrap().to_bytes();
                                    let response_body = if path == "/stream" {
                                        LlmEvents { stage: 0, credential: authorization.clone(), wait: Box::pin(tokio::time::sleep(Duration::from_secs(1))) }.boxed_unsync()
                                    } else { Full::new(Bytes::from(format!("auth={authorization}\nuri={uri}\nbody={}",String::from_utf8_lossy(&body)))).boxed_unsync() };
                                    let mut response = Response::new(response_body);
                                    if path == "/stream" { response.headers_mut().insert("content-type", "text/event-stream".parse().unwrap()); }
                                    if !authorization.is_empty() {
                                        response.headers_mut().insert("x-reflected-credential", authorization.parse().unwrap());
                                    }
                                    if path == "/gzip" { response.headers_mut().insert("content-encoding", "gzip".parse().unwrap()); }
                                    if path == "/redirect" { *response.status_mut()=hyper::StatusCode::FOUND; response.headers_mut().insert("location","https://denied.kumoba.test/".parse().unwrap()); }
                                    Ok::<_,Infallible>(response)
                                }
                            });
                            let _ = hyper::server::conn::http1::Builder::new().keep_alive(false).serve_connection(TokioIo::new(tls),service).await;
                        });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        Ok(Self {
            address,
            records,
            task,
        })
    }
}
fn assert_no_plaintext(
    path: &std::path::Path,
    values: &[&str],
) -> Result<(), Box<dyn std::error::Error>> {
    let matcher = aho_corasick::AhoCorasick::new(values)?;
    let file = std::fs::File::open(path)?;
    assert!(
        matcher.stream_find_iter(file).next().transpose()?.is_none(),
        "credential found in persisted artifact {}",
        path.display()
    );
    Ok(())
}

async fn curl(
    sandbox: &mut Sandbox,
    url: &str,
    placeholder: &str,
) -> crate::Result<crate::ExecOutput> {
    sandbox
        .exec(
            Command::new("/usr/bin/curl")
                .args([
                    "-fsSi",
                    "--max-time",
                    "10",
                    "-H",
                    &format!("Authorization: Bearer {placeholder}"),
                    url,
                ])
                .timeout(Duration::from_secs(15)),
        )
        .await
}

#[tokio::test]
#[ignore = "requires signed worker and dev image; scripts/test-local.sh"]
async fn real_guest_network_secrets_are_host_only_and_scoped()
-> Result<(), Box<dyn std::error::Error>> {
    let state = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()?
        .keep();
    eprintln!("secret VM state: {}", state.display());
    let upstream_authority = Authority::new()?;
    let api = TestUpstream::start(&upstream_authority, "api.kumoba.test").await?;
    let other = TestUpstream::start(&upstream_authority, "other.kumoba.test").await?;
    let store = SecretStore::new();
    let original = format!("synthetic-original-{}", uuid::Uuid::new_v4());
    let rotated = format!("synthetic-rotated-{}", uuid::Uuid::new_v4());
    let credential = store.create(
        "service",
        SecretValue::new(original.clone())?,
        policy(&["api.kumoba.test"]),
    )?;
    let mut config = RuntimeConfig::new(
        required("KUMOBA_TEST_WORKER"),
        required("KUMOBA_TEST_LIBRARY"),
        required("KUMOBA_TEST_FIRMWARE"),
        &state,
    );
    config.secret_store = Some(store.clone());
    let mut runtime = Runtime::open(config).await?;
    let authority = Arc::get_mut(
        Arc::get_mut(&mut runtime.inner)
            .unwrap()
            .authority
            .as_mut()
            .unwrap(),
    )
    .unwrap();
    authority.trust_test_authority(&upstream_authority);
    authority.route_test_endpoint(endpoint("api.kumoba.test"), api.address);
    authority.route_test_endpoint(endpoint("other.kumoba.test"), other.address);
    let mut config = SandboxConfig::new(required("KUMOBA_TEST_DEV_IMAGE"));
    config.memory_mib = 1024;
    config.network = Network::HttpConnect(policy(&["api.kumoba.test", "other.kumoba.test"]));
    config.secrets = BTreeMap::from([("SERVICE_TOKEN".into(), credential.id)]);
    let mut sandbox = runtime.create(config.clone()).await?;
    sandbox.start().await?;
    let env = sandbox.exec(Command::new("/usr/bin/env")).await?;
    assert!(env.success());
    let env = String::from_utf8(env.stdout)?;
    assert!(env.contains(&format!("SERVICE_TOKEN={}", credential.placeholder)));
    assert!(!env.contains(&original));
    assert!(env.contains("CURL_CA_BUNDLE=/tmp/kumoba-ca-bundle.pem"));
    let guest_ca = sandbox
        .read_file(&guest_path("/tmp/kumoba-ca.pem"), 65536)
        .await?;
    assert_eq!(
        guest_ca,
        runtime
            .inner
            .authority
            .as_ref()
            .unwrap()
            .certificate_pem()
            .as_bytes()
    );
    let bundle = sandbox
        .read_file(&guest_path("/tmp/kumoba-ca-bundle.pem"), 1024 * 1024)
        .await?;
    assert!(
        String::from_utf8(bundle)?
            .contains(&runtime.inner.authority.as_ref().unwrap().certificate_pem())
    );
    let echoed = curl(
        &mut sandbox,
        "https://api.kumoba.test/echo",
        &credential.placeholder,
    )
    .await?;
    assert!(echoed.success(), "echo request failed: {echoed:?}");
    let echoed = String::from_utf8(echoed.stdout)?;
    assert!(echoed.contains(&credential.placeholder));
    assert!(!echoed.contains(&original));
    assert_eq!(
        api.records.lock().unwrap().last().unwrap(),
        &format!("Bearer {original}")
    );
    assert!(
        curl(
            &mut sandbox,
            "https://API.KUMOBA.TEST/case",
            &credential.placeholder
        )
        .await?
        .success()
    );
    let mut streamed = StreamOutput::default();
    let mut stream_errors = Vec::new();
    let streamed_result = sandbox
        .exec_streamed(
            Command::new("/usr/bin/curl").args([
                "-NfsS",
                "--max-time",
                "10",
                "-H",
                &format!("Authorization: Bearer {}", credential.placeholder),
                "https://api.kumoba.test/stream",
            ]),
            &mut streamed,
            &mut stream_errors,
        )
        .await?;
    assert!(
        streamed_result.success(),
        "stream request failed: {}",
        String::from_utf8_lossy(&stream_errors)
    );
    assert!(
        streamed.first.unwrap().elapsed() >= Duration::from_millis(1500),
        "guest output was buffered until completion"
    );
    assert_eq!(
        streamed.bytes,
        format!("data: ready\n\ndata: Bearer {}\n\n", credential.placeholder).as_bytes()
    );
    assert!(!String::from_utf8_lossy(&streamed.bytes).contains(&original));
    let scoped = curl(
        &mut sandbox,
        "https://other.kumoba.test/echo",
        &credential.placeholder,
    )
    .await?;
    assert!(scoped.success());
    assert_eq!(
        other.records.lock().unwrap().last().unwrap(),
        &format!("Bearer {}", credential.placeholder)
    );
    let body = sandbox
        .exec(Command::new("/usr/bin/curl").args([
            "-fsS",
            "--max-time",
            "10",
            "-d",
            &credential.placeholder,
            &format!(
                "https://api.kumoba.test/body?token={}",
                credential.placeholder
            ),
        ]))
        .await?;
    assert!(body.success());
    assert!(String::from_utf8_lossy(&body.stdout).contains(&credential.placeholder));
    assert_eq!(api.records.lock().unwrap().last().unwrap(), "");
    let denied = curl(
        &mut sandbox,
        "https://denied.kumoba.test/",
        &credential.placeholder,
    )
    .await?;
    assert!(!denied.success());
    let encoded = curl(
        &mut sandbox,
        "https://api.kumoba.test/gzip",
        &credential.placeholder,
    )
    .await?;
    assert!(!encoded.success());
    assert!(!String::from_utf8_lossy(&encoded.stdout).contains(&original));
    let following = sandbox
        .exec(Command::new("/usr/bin/curl").args([
            "-fsSL",
            "--max-time",
            "10",
            "-H",
            &format!("Authorization: Bearer {}", credential.placeholder),
            "https://api.kumoba.test/redirect",
        ]))
        .await?;
    assert!(!following.success());
    let next = store.rotate(credential.id, SecretValue::new(rotated.clone())?)?;
    assert_eq!(next.placeholder, credential.placeholder);
    let rotated_response = curl(
        &mut sandbox,
        "https://api.kumoba.test/rotated",
        &credential.placeholder,
    )
    .await?;
    assert!(rotated_response.success());
    assert!(!String::from_utf8_lossy(&rotated_response.stdout).contains(&rotated));
    assert_eq!(
        api.records.lock().unwrap().last().unwrap(),
        &format!("Bearer {rotated}")
    );
    config.secrets.clear();
    let mut unbound = runtime.create(config).await?;
    unbound.start().await?;
    assert!(
        curl(
            &mut unbound,
            "https://api.kumoba.test/unbound",
            &credential.placeholder
        )
        .await?
        .success()
    );
    assert_eq!(
        api.records.lock().unwrap().last().unwrap(),
        &format!("Bearer {}", credential.placeholder)
    );
    unbound.delete().await?;
    sandbox.update_secret_bindings(BTreeMap::new()).await?;
    assert!(
        curl(
            &mut sandbox,
            "https://api.kumoba.test/detached",
            &credential.placeholder
        )
        .await?
        .success()
    );
    assert_eq!(
        api.records.lock().unwrap().last().unwrap(),
        &format!("Bearer {}", credential.placeholder)
    );
    sandbox
        .update_secret_bindings(BTreeMap::from([("SERVICE_TOKEN".into(), credential.id)]))
        .await?;
    assert!(
        curl(
            &mut sandbox,
            "https://api.kumoba.test/reattached",
            &credential.placeholder
        )
        .await?
        .success()
    );
    assert_eq!(
        api.records.lock().unwrap().last().unwrap(),
        &format!("Bearer {rotated}")
    );
    sandbox
        .write_file(
            &guest_path("/workspace/saved-token"),
            credential.placeholder.as_bytes(),
        )
        .await?;
    assert_eq!(sandbox.stop().await?, StopOutcome::Graceful);
    let snapshot = sandbox.snapshot().await?;
    assert_no_plaintext(&snapshot.path().join("root.raw"), &[&original, &rotated])?;
    assert_no_plaintext(
        &state
            .join("sandboxes")
            .join(sandbox.id().to_string())
            .join("worker.log"),
        &[&original, &rotated],
    )?;
    let manifest = std::fs::read_to_string(snapshot.path().join("snapshot.json"))?;
    assert!(!manifest.contains(&original) && !manifest.contains(&rotated));
    let mut restored = runtime.restore(&snapshot).await?;
    assert!(restored.config().secrets.is_empty());
    restored.start().await?;
    assert!(
        curl(
            &mut restored,
            "https://api.kumoba.test/restored",
            &credential.placeholder
        )
        .await?
        .success()
    );
    assert_eq!(
        api.records.lock().unwrap().last().unwrap(),
        &format!("Bearer {}", credential.placeholder)
    );
    restored.delete().await?;
    sandbox.start().await?;
    assert!(
        curl(
            &mut sandbox,
            "https://api.kumoba.test/restarted",
            &credential.placeholder
        )
        .await?
        .success()
    );
    assert_eq!(
        api.records.lock().unwrap().last().unwrap(),
        &format!("Bearer {rotated}")
    );
    store.delete(credential.id)?;
    assert!(
        curl(
            &mut sandbox,
            "https://api.kumoba.test/deleted",
            &credential.placeholder
        )
        .await?
        .success()
    );
    assert_eq!(
        api.records.lock().unwrap().last().unwrap(),
        &format!("Bearer {}", credential.placeholder)
    );
    sandbox.update_secret_bindings(BTreeMap::new()).await?;
    sandbox.delete().await?;
    runtime.delete_snapshot(&snapshot).await?;
    drop(restored);
    drop(unbound);
    drop(sandbox);
    drop(runtime);
    std::fs::remove_dir_all(&state)?;
    eprintln!(
        "real guest secret injection, reflection scrubbing, destination and binding scope, rotation, revoke, restore and restart passed"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires signed worker and browser image; scripts/test-local.sh"]
async fn real_browser_uses_scoped_network_secrets() -> Result<(), Box<dyn std::error::Error>> {
    let state = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()?;
    let upstream_authority = Authority::new()?;
    let api = TestUpstream::start(&upstream_authority, "api.kumoba.test").await?;
    let store = SecretStore::new();
    let original = format!("synthetic-browser-{}", uuid::Uuid::new_v4());
    let credential = store.create(
        "browser",
        SecretValue::new(original.clone())?,
        policy(&["api.kumoba.test"]),
    )?;
    let mut config = RuntimeConfig::new(
        required("KUMOBA_TEST_WORKER"),
        required("KUMOBA_TEST_LIBRARY"),
        required("KUMOBA_TEST_FIRMWARE"),
        state.path(),
    );
    config.secret_store = Some(store);
    let mut runtime = Runtime::open(config).await?;
    let authority = Arc::get_mut(
        Arc::get_mut(&mut runtime.inner)
            .unwrap()
            .authority
            .as_mut()
            .unwrap(),
    )
    .unwrap();
    authority.trust_test_authority(&upstream_authority);
    authority.route_test_endpoint(endpoint("api.kumoba.test"), api.address);
    let mut config = SandboxConfig::new(required("KUMOBA_TEST_BROWSER_IMAGE"));
    config.vcpus = 2;
    config.memory_mib = 2048;
    config.network = Network::HttpConnect(policy(&["api.kumoba.test"]));
    config.secrets = BTreeMap::from([("SERVICE_TOKEN".into(), credential.id)]);
    let mut sandbox = runtime.create(config).await?;
    sandbox.start().await?;
    sandbox
        .desktop_start(crate::DesktopConfig::default())
        .await?;
    let script = r#"
const assert=require('node:assert/strict');
const {chromium}=require('playwright-core');
(async()=>{
 const browser=await chromium.connectOverCDP('http://127.0.0.1:9222');
 const context=browser.contexts()[0];
 await context.setExtraHTTPHeaders({Authorization:`Bearer ${process.env.SERVICE_TOKEN}`});
 const page=context.pages()[0];
 const response=await page.goto('https://api.kumoba.test/echo',{timeout:15000});
 assert.equal(response.status(),200);
 assert.ok((await response.text()).includes(process.env.SERVICE_TOKEN));
 assert.ok((await response.headerValue('x-reflected-credential')).includes(process.env.SERVICE_TOKEN));
 let denied=false;
 try{await page.goto('https://denied.kumoba.test/',{timeout:5000})}catch{denied=true}
 assert.ok(denied,'non-allowlisted endpoint reached');
 console.log('browser TLS trust, header injection, response redaction and deny rule verified');
 await browser.close();
})().catch(e=>{console.error(e);process.exitCode=1});
"#;
    sandbox
        .write_file(
            &guest_path("/workspace/browser-secrets.cjs"),
            script.as_bytes(),
        )
        .await?;
    let result = sandbox
        .exec(
            Command::new("/usr/bin/node")
                .args(["/workspace/browser-secrets.cjs"])
                .timeout(Duration::from_secs(30)),
        )
        .await?;
    assert!(result.success(), "{result:?}");
    assert!(!String::from_utf8_lossy(&result.stdout).contains(&original));
    assert!(
        api.records
            .lock()
            .unwrap()
            .iter()
            .any(|value| value == &format!("Bearer {original}"))
    );
    sandbox.desktop_stop().await?;
    assert_eq!(sandbox.stop().await?, StopOutcome::Graceful);
    let snapshot = sandbox.snapshot().await?;
    assert_no_plaintext(&snapshot.path().join("root.raw"), &[&original])?;
    sandbox.delete().await?;
    runtime.delete_snapshot(&snapshot).await?;
    Ok(())
}
