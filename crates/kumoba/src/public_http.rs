use crate::{EgressEndpoint, secret_https::Prefix};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited, combinators::UnsyncBoxBody};
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
    io::{AsyncRead, AsyncWrite},
    task::JoinHandle,
    time::timeout,
};

type ResponseBody = UnsyncBoxBody<Bytes, io::Error>;
const SESSION_TIMEOUT: Duration = Duration::from_secs(120);

pub(crate) async fn serve<S>(
    client: S,
    mut head: Vec<u8>,
    pending: Vec<u8>,
    scope: crate::egress::AccessScope,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    crate::secret_https::check_wire_headers(&head)?;
    head.extend(pending);
    let scope = &scope;
    let service = hyper::service::service_fn(|request| async move {
        let target = request.uri().authority().and_then(|authority| {
            crate::egress::raw_target(authority.as_str())
                .or_else(|| crate::egress::raw_target(&format!("{}:80", authority.host())))
        });
        let outcome = forward(request).await;
        let decision = match outcome.as_ref().err() {
            None => Some(true),
            Some(&StatusCode::BAD_GATEWAY) => Some(true),
            Some(&StatusCode::FORBIDDEN) => Some(false),
            Some(_) => None,
        };
        if let (Some((host, port)), Some(allowed)) = (target, decision) {
            scope.record(&host, port, allowed);
        }
        Ok::<_, Infallible>(match outcome {
            Ok(response) => response,
            Err(status) => failure(status),
        })
    });
    timeout(
        SESSION_TIMEOUT,
        hyper::server::conn::http1::Builder::new()
            .keep_alive(false)
            .max_buf_size(8192)
            .serve_connection(TokioIo::new(Prefix::new(client, head)), service),
    )
    .await?
    .map_err(io::Error::other)
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
async fn forward(request: Request<Incoming>) -> Result<Response<ResponseBody>, StatusCode> {
    if request.method() == hyper::Method::CONNECT || request.headers().contains_key("upgrade") {
        return Err(StatusCode::BAD_REQUEST);
    }
    let endpoint = destination(request.uri(), request.headers())?;
    let (mut parts, body) = request.into_parts();
    parts.uri = parts
        .uri
        .path_and_query()
        .map(|path| path.as_str())
        .unwrap_or("/")
        .parse()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    strip_hop_headers(&mut parts.headers)?;
    let bytes = timeout(
        Duration::from_secs(30),
        Limited::new(body, 4 * 1024 * 1024).collect(),
    )
    .await
    .map_err(|_| StatusCode::REQUEST_TIMEOUT)?
    .map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?
    .to_bytes();
    parts.headers.remove("content-length");
    parts
        .headers
        .insert("connection", HeaderValue::from_static("close"));
    let remote = crate::egress::connect(&endpoint).await.map_err(|error| {
        if error.kind() == io::ErrorKind::PermissionDenied {
            StatusCode::FORBIDDEN
        } else {
            StatusCode::BAD_GATEWAY
        }
    })?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(remote))
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let task = tokio::spawn(async move {
        let _ = timeout(SESSION_TIMEOUT, connection).await;
    });
    let guard = AbortOnDrop(task);
    let response = timeout(
        Duration::from_secs(30),
        sender.send_request(Request::from_parts(parts, Full::new(bytes))),
    )
    .await
    .map_err(|_| StatusCode::GATEWAY_TIMEOUT)?
    .map_err(|_| StatusCode::BAD_GATEWAY)?;
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
    use tokio::net::UnixStream;
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
        use tokio::io::AsyncReadExt;
        let (server, mut client) = UnixStream::pair().unwrap();
        let task = tokio::spawn(serve(
            server,
            b"GET http://example.com/ HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec(),
            Vec::new(),
            crate::egress::AccessScope::new(None),
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
