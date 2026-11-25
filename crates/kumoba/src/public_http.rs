use crate::{EgressEndpoint, secret_https::Prefix};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::UnsyncBoxBody};
use hyper::{
    Request, Response, StatusCode,
    body::{Body, Incoming},
    header::{HeaderMap, HeaderValue},
};
use hyper_util::rt::TokioIo;
use std::{
    convert::Infallible,
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::{oneshot, watch},
    task::JoinHandle,
    time::{Instant, timeout},
};

type ResponseBody = UnsyncBoxBody<Bytes, io::Error>;
#[derive(Clone, Copy)]
struct Limits {
    idle: Duration,
    headers: Duration,
    lifetime: Duration,
}
const LIMITS: Limits = Limits {
    idle: crate::egress::IDLE_TIMEOUT,
    headers: Duration::from_secs(30),
    lifetime: crate::egress::TUNNEL_TIMEOUT,
};

pub(crate) async fn serve<S>(
    client: S,
    head: Vec<u8>,
    pending: Vec<u8>,
    scope: crate::egress::AccessScope,
    egress: &crate::egress::Egress,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    serve_with(client, head, pending, scope, egress, LIMITS).await
}
async fn serve_with<S>(
    client: S,
    mut head: Vec<u8>,
    pending: Vec<u8>,
    scope: crate::egress::AccessScope,
    egress: &crate::egress::Egress,
    limits: Limits,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    crate::secret_https::check_wire_headers(&head)?;
    head.extend(pending);
    let scope = &scope;
    let service = hyper::service::service_fn(|request| async move {
        let outcome = forward(request, scope, egress, limits).await;
        Ok::<_, Infallible>(match outcome {
            Ok(response) => response,
            Err(status) => failure(status),
        })
    });
    let (activity, changes) = watch::channel(Instant::now());
    let client = Prefix::new(
        Active {
            inner: client,
            activity,
        },
        head,
    );
    let connection = hyper::server::conn::http1::Builder::new()
        .keep_alive(false)
        .max_buf_size(8192)
        .serve_connection(TokioIo::new(client), service);
    timeout(limits.lifetime, async {
        tokio::select! {
            result = connection => result.map_err(io::Error::other),
            result = crate::egress::idle(changes, limits.idle) => result,
        }
    })
    .await?
}
fn destination(uri: &hyper::Uri, headers: &HeaderMap) -> Result<EgressEndpoint, StatusCode> {
    if uri.scheme_str() != Some("http") || uri.port_u16().unwrap_or(80) != 80 {
        return Err(StatusCode::FORBIDDEN);
    }
    let endpoint = EgressEndpoint::new(uri.host().ok_or(StatusCode::BAD_REQUEST)?, 80)
        .map_err(|_| StatusCode::FORBIDDEN)?;
    let host = headers
        .get("host")
        .ok_or(StatusCode::BAD_REQUEST)?
        .to_str()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let authority = uri.authority().ok_or(StatusCode::BAD_REQUEST)?;
    if !host.eq_ignore_ascii_case(authority.as_str()) {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(endpoint)
}
fn strip_hop_headers(headers: &mut HeaderMap) -> Result<(), StatusCode> {
    let names = headers
        .get_all("connection")
        .iter()
        .map(|value| {
            value
                .to_str()
                .map(str::to_owned)
                .map_err(|_| StatusCode::BAD_REQUEST)
        })
        .collect::<Result<Vec<_>, _>>()?;
    for value in names {
        for name in value.split(',').map(str::trim) {
            if name.eq_ignore_ascii_case("host")
                || name.eq_ignore_ascii_case("content-length")
                || name.eq_ignore_ascii_case("transfer-encoding")
            {
                return Err(StatusCode::BAD_REQUEST);
            }
            headers.remove(name);
        }
    }
    for name in [
        "connection",
        "proxy-connection",
        "proxy-authorization",
        "proxy-authenticate",
        "keep-alive",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }
    Ok(())
}
async fn forward(
    request: Request<Incoming>,
    scope: &crate::egress::AccessScope,
    egress: &crate::egress::Egress,
    limits: Limits,
) -> Result<Response<ResponseBody>, StatusCode> {
    if request.method() == hyper::Method::CONNECT || request.headers().contains_key("upgrade") {
        return Err(StatusCode::BAD_REQUEST);
    }
    let endpoint = match destination(request.uri(), request.headers()) {
        Ok(endpoint) => endpoint,
        Err(status) => {
            if status == StatusCode::FORBIDDEN
                && let Some((host, port)) = request.uri().authority().and_then(|authority| {
                    crate::egress::raw_target(authority.as_str())
                        .or_else(|| crate::egress::raw_target(&format!("{}:80", authority.host())))
                })
            {
                scope.record(&host, port, crate::egress::AccessDecision::Denied);
            }
            return Err(status);
        }
    };
    let (mut parts, body) = request.into_parts();
    parts.uri = parts
        .uri
        .path_and_query()
        .map(hyper::http::uri::PathAndQuery::as_str)
        .unwrap_or("/")
        .parse()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    strip_hop_headers(&mut parts.headers)?;
    parts.headers.remove("content-length");
    parts
        .headers
        .insert("connection", HeaderValue::from_static("close"));
    let remote = match crate::egress::connect(&endpoint, egress).await {
        Ok(remote) => remote,
        Err(error) => {
            let (decision, status) = if error.kind() == io::ErrorKind::PermissionDenied {
                (crate::egress::AccessDecision::Denied, StatusCode::FORBIDDEN)
            } else {
                (
                    crate::egress::AccessDecision::Failed,
                    StatusCode::BAD_GATEWAY,
                )
            };
            scope.record(endpoint.host(), endpoint.port(), decision);
            return Err(status);
        }
    };
    scope.record(
        endpoint.host(),
        endpoint.port(),
        crate::egress::AccessDecision::Allowed,
    );
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(remote))
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let guard = AbortOnDrop(tokio::spawn(async move {
        let _ = connection.await;
    }));
    let (sent, uploaded) = oneshot::channel::<Infallible>();
    let body = RequestBody {
        sent: (!body.is_end_stream()).then_some(sent),
        body,
    };
    let response = sender.send_request(Request::from_parts(parts, body));
    let headers = async {
        let _ = uploaded.await;
        tokio::time::sleep(limits.headers).await;
    };
    let response = tokio::select! {
        biased;
        response = response => response.map_err(|_| StatusCode::BAD_GATEWAY)?,
        () = headers => return Err(StatusCode::GATEWAY_TIMEOUT),
    };
    let (mut parts, body) = response.into_parts();
    strip_hop_headers(&mut parts.headers)?;
    Ok(Response::from_parts(
        parts,
        UpstreamBody {
            body,
            _guard: guard,
        }
        .boxed_unsync(),
    ))
}
struct Active<S> {
    inner: S,
    activity: watch::Sender<Instant>,
}
impl<S: AsyncRead + Unpin> AsyncRead for Active<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        if matches!(result, Poll::Ready(Ok(()))) && buf.filled().len() > before {
            self.activity.send_replace(Instant::now());
        }
        result
    }
}
impl<S: AsyncWrite + Unpin> AsyncWrite for Active<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write(cx, data);
        if matches!(result, Poll::Ready(Ok(written)) if written > 0) {
            self.activity.send_replace(Instant::now());
        }
        result
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
struct RequestBody {
    body: Incoming,
    sent: Option<oneshot::Sender<Infallible>>,
}
impl Body for RequestBody {
    type Data = Bytes;
    type Error = hyper::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, hyper::Error>>> {
        let this = self.get_mut();
        let frame = Pin::new(&mut this.body).poll_frame(cx);
        if matches!(frame, Poll::Ready(None | Some(Err(_)))) || this.body.is_end_stream() {
            this.sent = None;
        }
        frame
    }
    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }
    fn size_hint(&self) -> hyper::body::SizeHint {
        self.body.size_hint()
    }
}
struct AbortOnDrop(JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct UpstreamBody {
    body: Incoming,
    _guard: AbortOnDrop,
}
impl Body for UpstreamBody {
    type Data = Bytes;
    type Error = io::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, io::Error>>> {
        Pin::new(&mut self.get_mut().body)
            .poll_frame(cx)
            .map(|frame| frame.map(|frame| frame.map_err(io::Error::other)))
    }
    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }
    fn size_hint(&self) -> hyper::body::SizeHint {
        self.body.size_hint()
    }
}
fn failure(status: StatusCode) -> Response<ResponseBody> {
    let mut response = Response::new(
        Full::new(Bytes::from_static(
            b"Sandbox public-web proxy rejected this request.",
        ))
        .map_err(|never: Infallible| match never {})
        .boxed_unsync(),
    );
    *response.status_mut() = status;
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::egress::{
        AccessDecision, AccessRecord, AccessScope, Egress, EgressConfig, TEST_DNS, TEST_DNS_CHANGED,
    };
    use std::{
        future::Future,
        sync::{Arc, Mutex},
        time::Instant,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream, UnixStream},
        time::sleep,
    };

    fn short(idle: u64, headers: u64) -> Limits {
        Limits {
            idle: Duration::from_millis(idle),
            headers: Duration::from_millis(headers),
            lifetime: Duration::from_secs(30),
        }
    }

    struct Origin {
        host: &'static str,
        egress: &'static Egress,
        task: JoinHandle<()>,
        records: Arc<Mutex<Vec<AccessRecord>>>,
    }

    impl Origin {
        fn request(&self, head: &str, limits: Limits) -> (UnixStream, JoinHandle<io::Result<()>>) {
            let (server, client) = UnixStream::pair().unwrap();
            let head = head.replace("{host}", self.host).into_bytes();
            let records = self.records.clone();
            let task = tokio::spawn(serve_with(
                server,
                head,
                Vec::new(),
                AccessScope::new(Some(Arc::new(move |record| {
                    records.lock().unwrap().push(record);
                }))),
                self.egress,
                limits,
            ));
            (client, task)
        }
        async fn finish(&mut self) {
            (&mut self.task).await.unwrap();
        }
        fn decisions(&self) -> Vec<(String, u16, AccessDecision)> {
            self.records
                .lock()
                .unwrap()
                .iter()
                .map(|record| (record.host.clone(), record.port, record.decision))
                .collect()
        }
    }

    impl Drop for Origin {
        fn drop(&mut self) {
            self.task.abort();
            TEST_DNS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(self.host);
            TEST_DNS_CHANGED.notify_all();
        }
    }

    async fn origin<F, R>(host: &'static str, handle: F) -> Origin
    where
        F: FnOnce(TcpStream) -> R + Send + 'static,
        R: Future<Output = ()> + Send + 'static,
    {
        let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://127.0.0.1:{}", proxy.local_addr().unwrap().port());
        let config = EgressConfig::parse("", Some(&url), "").unwrap();
        let egress = Box::leak(Box::new(Egress::new(Arc::new(config))));
        TEST_DNS
            .lock()
            .unwrap()
            .insert(host.into(), Some(vec!["93.184.216.34".parse().unwrap()]));
        TEST_DNS_CHANGED.notify_all();
        let task = tokio::spawn(async move {
            let (mut stream, _) = proxy.accept().await.unwrap();
            read_head(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            handle(stream).await;
        });
        Origin {
            host,
            egress,
            task,
            records: Arc::default(),
        }
    }

    async fn read_head(stream: &mut TcpStream) -> String {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).await.unwrap();
            head.push(byte[0]);
        }
        String::from_utf8(head).unwrap()
    }

    async fn echo_origin(stream: TcpStream) {
        let service = hyper::service::service_fn(|request: Request<Incoming>| async move {
            let length = request
                .headers()
                .get("content-length")
                .map(|value| value.to_str().unwrap().to_owned());
            let chunked = request.headers().contains_key("transfer-encoding");
            let body = request.into_body().collect().await?.to_bytes();
            let intact = body
                .iter()
                .enumerate()
                .all(|(index, byte)| usize::from(*byte) == index % 251);
            Ok::<_, hyper::Error>(Response::new(Full::new(Bytes::from(format!(
                "{length:?} {chunked} {} {intact}",
                body.len()
            )))))
        });
        let _ = hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(stream), service)
            .await;
    }

    fn pattern(length: usize) -> Vec<u8> {
        (0..length)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect()
    }

    async fn read_response(client: &mut UnixStream, within: Duration) -> (String, Vec<u8>) {
        let mut response = Vec::new();
        timeout(within, client.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        let end = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap()
            + 4;
        let body = response.split_off(end);
        (String::from_utf8(response).unwrap(), body)
    }

    #[tokio::test]
    async fn steady_download_outlives_the_idle_limit() {
        let mut origin = origin("steady.public-http.example", |mut stream| async move {
            assert!(
                read_head(&mut stream)
                    .await
                    .starts_with("GET /big HTTP/1.1\r\n")
            );
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 20480\r\n\r\n")
                .await
                .unwrap();
            for _ in 0..20 {
                stream.write_all(&[7; 1024]).await.unwrap();
                sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        let (mut client, task) = origin.request(
            "GET http://{host}/big HTTP/1.1\r\nHost: {host}\r\n\r\n",
            short(300, 300),
        );
        let (head, body) = read_response(&mut client, Duration::from_secs(5)).await;
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert_eq!(body.len(), 20480);
        assert!(body.iter().all(|byte| *byte == 7));
        task.await.unwrap().unwrap();
        origin.finish().await;
        assert_eq!(
            origin.decisions(),
            [(origin.host.to_owned(), 80, AccessDecision::Allowed)]
        );
    }

    #[tokio::test]
    async fn upload_over_four_mib_reaches_the_origin_intact() {
        let mut origin = origin("upload.public-http.example", echo_origin).await;
        let length = 5 * 1024 * 1024;
        let (mut client, task) = origin.request(
            &format!(
                "POST http://{{host}}/upload HTTP/1.1\r\nHost: {{host}}\r\nContent-Length: {length}\r\n\r\n"
            ),
            short(2000, 2000),
        );
        let _ = client.write_all(&pattern(length)).await;
        let (head, body) = read_response(&mut client, Duration::from_secs(10)).await;
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert_eq!(
            String::from_utf8(body).unwrap(),
            format!("Some(\"{length}\") false {length} true")
        );
        task.await.unwrap().unwrap();
        origin.finish().await;
        assert_eq!(
            origin.decisions(),
            [(origin.host.to_owned(), 80, AccessDecision::Allowed)]
        );
    }

    #[tokio::test]
    async fn slow_chunked_upload_outlasts_the_header_wait() {
        let mut origin = origin("slow-upload.public-http.example", echo_origin).await;
        let (mut client, task) = origin.request(
            "POST http://{host}/upload HTTP/1.1\r\nHost: {host}\r\nTransfer-Encoding: chunked\r\n\r\n",
            short(2000, 300),
        );
        for chunk in pattern(10_000).chunks(1000) {
            client
                .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                .await
                .unwrap();
            client.write_all(chunk).await.unwrap();
            client.write_all(b"\r\n").await.unwrap();
            sleep(Duration::from_millis(100)).await;
        }
        client.write_all(b"0\r\n\r\n").await.unwrap();
        let (head, body) = read_response(&mut client, Duration::from_secs(5)).await;
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert_eq!(String::from_utf8(body).unwrap(), "None true 10000 true");
        task.await.unwrap().unwrap();
        origin.finish().await;
    }

    #[tokio::test]
    async fn silent_origin_gets_gateway_timeout_after_the_header_wait() {
        for (host, request) in [
            (
                "silent-get.public-http.example",
                "GET http://{host}/ HTTP/1.1\r\nHost: {host}\r\n\r\n",
            ),
            (
                "silent-post.public-http.example",
                "POST http://{host}/ HTTP/1.1\r\nHost: {host}\r\nContent-Length: 5\r\n\r\n",
            ),
        ] {
            let origin = origin(host, |mut stream| async move {
                read_head(&mut stream).await;
                sleep(Duration::from_secs(5)).await;
            })
            .await;
            let started = Instant::now();
            let (mut client, task) = origin.request(request, short(3000, 300));
            if request.starts_with("POST") {
                client.write_all(b"hello").await.unwrap();
            }
            let (head, _) = read_response(&mut client, Duration::from_secs(5)).await;
            assert!(head.starts_with("HTTP/1.1 504"), "{host}: {head}");
            assert!(started.elapsed() < Duration::from_secs(2), "{host}");
            task.await.unwrap().unwrap();
        }
    }

    #[tokio::test]
    async fn stalled_download_is_cut_after_the_idle_limit() {
        let origin = origin("stalled.public-http.example", |mut stream| async move {
            read_head(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10000\r\n\r\n")
                .await
                .unwrap();
            stream.write_all(&[1; 1000]).await.unwrap();
            sleep(Duration::from_secs(5)).await;
        })
        .await;
        let started = Instant::now();
        let (mut client, task) = origin.request(
            "GET http://{host}/ HTTP/1.1\r\nHost: {host}\r\n\r\n",
            short(300, 300),
        );
        let (head, body) = read_response(&mut client, Duration::from_secs(5)).await;
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert_eq!(body.len(), 1000);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(task.await.unwrap().is_err());
        origin.task.abort();
    }

    #[tokio::test]
    async fn trickling_download_still_ends_at_the_lifetime_limit() {
        let origin = origin("trickle.public-http.example", |mut stream| async move {
            read_head(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\n")
                .await
                .unwrap();
            loop {
                if stream.write_all(&[1]).await.is_err() {
                    return;
                }
                sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        let started = Instant::now();
        let (mut client, task) = origin.request(
            "GET http://{host}/ HTTP/1.1\r\nHost: {host}\r\n\r\n",
            Limits {
                lifetime: Duration::from_millis(800),
                ..short(300, 300)
            },
        );
        let (head, body) = read_response(&mut client, Duration::from_secs(5)).await;
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert!(body.len() < 1000000);
        assert!(started.elapsed() >= Duration::from_millis(800));
        assert!(task.await.unwrap().is_err());
        origin.task.abort();
    }
    #[test]
    fn destination_requires_public_web_scheme_port_and_matching_authority() {
        for (uri, host, allowed) in [
            ("http://example.com/path?q=1", "example.com", true),
            ("http://example.com:80/", "example.com:80", true),
            ("http://example.com/", "other.org", false),
            ("http://127.0.0.1/", "127.0.0.1", false),
            ("http://0x7f000001/", "0x7f000001", false),
            ("http://[::1]/", "[::1]", false),
            ("http://example.com:8080/", "example.com:8080", false),
            ("https://example.com/", "example.com", false),
            ("/path", "example.com", false),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("host", host.parse().unwrap());
            assert_eq!(
                destination(&uri.parse().unwrap(), &headers).is_ok(),
                allowed,
                "{uri}"
            );
        }
    }
    #[test]
    fn drops_proxy_credentials_and_connection_named_headers() {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("host", "example.com"),
            ("proxy-authorization", "synthetic"),
            ("connection", "x-hop, keep-alive"),
            ("x-hop", "private"),
            ("accept", "text/html"),
        ] {
            headers.insert(
                hyper::header::HeaderName::from_static(name),
                value.parse().unwrap(),
            );
        }
        strip_hop_headers(&mut headers).unwrap();
        assert!(!headers.contains_key("proxy-authorization"));
        assert!(!headers.contains_key("x-hop"));
        assert!(headers.contains_key("host"));
        assert!(headers.contains_key("accept"));
        headers.insert("connection", "content-length".parse().unwrap());
        assert!(strip_hop_headers(&mut headers).is_err());
    }
    #[tokio::test]
    async fn rejects_mismatched_authority_before_connecting() {
        let (server, mut client) = UnixStream::pair().unwrap();
        let task = tokio::spawn(serve(
            server,
            b"GET http://example.com/ HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec(),
            Vec::new(),
            AccessScope::new(None),
            Box::leak(Box::default()),
        ));
        let mut response = Vec::new();
        timeout(Duration::from_secs(2), client.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(response.starts_with(b"HTTP/1.1 400"));
        task.await.unwrap().unwrap();
    }
    #[test]
    fn public_policy_cannot_expand_secret_scope() {
        let policy = crate::HttpConnectPolicy::public_web();
        assert!(policy.allows(&EgressEndpoint::new("example.com", 443).unwrap()));
        assert!(!policy.allows(&EgressEndpoint::new("example.com", 22).unwrap()));
        let serialized = serde_json::to_string(&policy).unwrap();
        assert!(
            serde_json::from_str::<crate::HttpConnectPolicy>(&serialized)
                .unwrap()
                .is_public_web()
        );
        assert!(
            serde_json::from_str::<crate::HttpConnectPolicy>(
                r#"{"endpoints":[{"host":"example.com","port":22}],"public_web":true}"#
            )
            .is_err()
        );
        let store = crate::SecretStore::new();
        assert!(
            store
                .create(
                    "test",
                    crate::SecretValue::new("SyntheticOnly123").unwrap(),
                    policy
                )
                .is_err()
        );
    }
}
