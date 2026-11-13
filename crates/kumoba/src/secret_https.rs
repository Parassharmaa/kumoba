use crate::{
    EgressEndpoint, Error, Result,
    secrets::{RequestSecrets, SecretSession},
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::UnsyncBoxBody};
use hyper::{
    Request, Response, StatusCode,
    body::{Body, Incoming},
    header::{HeaderMap, HeaderName, HeaderValue},
};
use hyper_util::rt::{TokioIo, TokioTimer};
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DnType, IsCa, KeyPair, KeyUsagePurpose,
};
use rustls::{
    ClientConfig, RootCertStore, ServerConfig,
    pki_types::{PrivatePkcs8KeyDer, ServerName},
};
use std::{
    convert::Infallible,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpStream,
    sync::Mutex,
    time::timeout,
};
use tokio_rustls::{TlsAcceptor, TlsConnector};

const HEADER_LIMIT: usize = 16 * 1024;
const HEADER_COUNT: usize = 64;
const REQUEST_LIMIT: usize = 1024 * 1024;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const SESSION_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

type HttpBody = Full<Bytes>;
type ResponseBody = UnsyncBoxBody<Bytes, io::Error>;

fn failure() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "HTTPS inspection rejected the exchange",
    )
}
fn configuration_failure() -> Error {
    Error::InvalidInput("cannot initialize network secret TLS".into())
}

pub(crate) struct Authority {
    issuer: CertifiedIssuer<'static, KeyPair>,
    upstream: Arc<ClientConfig>,
    #[cfg(test)]
    test_routes: std::collections::BTreeMap<EgressEndpoint, std::net::SocketAddr>,
}

impl Authority {
    pub(crate) fn new() -> Result<Self> {
        let mut params =
            CertificateParams::new(Vec::<String>::new()).map_err(|_| configuration_failure())?;
        params.distinguished_name.push(
            DnType::CommonName,
            format!("Kumoba Egress CA {}", uuid::Uuid::new_v4()),
        );
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let key = KeyPair::generate().map_err(|_| configuration_failure())?;
        let issuer =
            CertifiedIssuer::self_signed(params, key).map_err(|_| configuration_failure())?;
        let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        Ok(Self {
            issuer,
            upstream: Arc::new(client_config(roots)),
            #[cfg(test)]
            test_routes: Default::default(),
        })
    }

    pub(crate) fn certificate_pem(&self) -> String {
        self.issuer.pem()
    }

    fn server_config(&self, host: &str) -> io::Result<Arc<ServerConfig>> {
        let key = KeyPair::generate().map_err(|_| failure())?;
        let mut params = CertificateParams::new(vec![host.to_owned()]).map_err(|_| failure())?;
        params.distinguished_name.push(DnType::CommonName, host);
        params.use_authority_key_identifier_extension = true;
        let cert = params
            .signed_by(&key, &self.issuer)
            .map_err(|_| failure())?;
        let mut config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .map_err(|_| failure())?
                .with_no_client_auth()
                .with_single_cert(
                    vec![cert.der().clone()],
                    PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
                )
                .map_err(|_| failure())?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Arc::new(config))
    }

    #[cfg(test)]
    pub(crate) fn route_test_endpoint(
        &mut self,
        endpoint: EgressEndpoint,
        address: std::net::SocketAddr,
    ) {
        self.test_routes.insert(endpoint, address);
    }

    #[cfg(test)]
    pub(crate) fn trust_test_authority(&mut self, authority: &Authority) {
        let mut roots = RootCertStore::empty();
        roots.add(authority.issuer.der().clone()).unwrap();
        self.upstream = Arc::new(client_config(roots));
    }

    #[cfg(test)]
    pub(crate) fn test_server_config(&self, host: &str) -> Arc<ServerConfig> {
        self.server_config(host).unwrap()
    }

    #[cfg(test)]
    pub(crate) fn test_client_config(&self) -> Arc<ClientConfig> {
        let mut roots = RootCertStore::empty();
        roots.add(self.issuer.der().clone()).unwrap();
        Arc::new(client_config(roots))
    }
}

fn client_config(roots: RootCertStore) -> ClientConfig {
    let mut config =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("ring supports TLS 1.2 and TLS 1.3")
            .with_root_certificates(roots)
            .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    config
}

#[derive(Clone)]
pub(crate) struct Inspection {
    authority: Arc<Authority>,
    secrets: SecretSession,
}

impl Inspection {
    pub(crate) fn new(authority: Arc<Authority>, secrets: SecretSession) -> Self {
        Self { authority, secrets }
    }

    pub(crate) fn covers(&self, endpoint: &EgressEndpoint) -> bool {
        self.secrets
            .request(endpoint)
            .map(|secrets| secrets.authorizes_any())
            .unwrap_or(true)
    }

    #[cfg(test)]
    pub(crate) fn test_route(&self, endpoint: &EgressEndpoint) -> Option<std::net::SocketAddr> {
        self.authority.test_routes.get(endpoint).copied()
    }

    pub(crate) async fn serve<S>(
        &self,
        client: S,
        pending: Vec<u8>,
        remote: TcpStream,
        endpoint: EgressEndpoint,
    ) -> io::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        timeout(
            SESSION_TIMEOUT,
            self.exchange(client, pending, remote, endpoint),
        )
        .await
        .map_err(|_| failure())?
    }

    async fn exchange<S>(
        &self,
        client: S,
        pending: Vec<u8>,
        remote: TcpStream,
        endpoint: EgressEndpoint,
    ) -> io::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let config = self.authority.server_config(endpoint.host())?;
        let client = Prefix::new(client, pending);
        let client = timeout(HANDSHAKE_TIMEOUT, TlsAcceptor::from(config).accept(client))
            .await
            .map_err(|_| failure())?
            .map_err(|_| failure())?;
        if !client
            .get_ref()
            .1
            .server_name()
            .is_some_and(|name| name.eq_ignore_ascii_case(endpoint.host()))
        {
            return Err(failure());
        }
        let server_name =
            ServerName::try_from(endpoint.host().to_owned()).map_err(|_| failure())?;
        let remote = timeout(
            HANDSHAKE_TIMEOUT,
            TlsConnector::from(self.authority.upstream.clone()).connect(server_name, remote),
        )
        .await
        .map_err(|_| failure())?
        .map_err(|_| failure())?;
        let remote = Arc::new(Mutex::new(Some(remote)));
        let session = self.secrets.clone();
        let service = hyper::service::service_fn(move |request| {
            let remote = remote.clone();
            let session = session.clone();
            let endpoint = endpoint.clone();
            async move {
                let response = match remote.lock().await.take() {
                    Some(remote) => match timeout(
                        REQUEST_TIMEOUT,
                        process(request, &endpoint, &session, remote),
                    )
                    .await
                    {
                        Ok(Ok(response)) => response,
                        _ => empty_error(),
                    },
                    None => empty_error(),
                };
                Ok::<_, Infallible>(response)
            }
        });
        hyper::server::conn::http1::Builder::new()
            .timer(TokioTimer::new())
            .header_read_timeout(HANDSHAKE_TIMEOUT)
            .keep_alive(false)
            .max_headers(HEADER_COUNT)
            .max_buf_size(HEADER_LIMIT)
            .serve_connection(TokioIo::new(HeaderGuard::new(client)), service)
            .await
            .map_err(|_| failure())
    }
}

async fn process(
    request: Request<Incoming>,
    endpoint: &EgressEndpoint,
    session: &SecretSession,
    remote: tokio_rustls::client::TlsStream<TcpStream>,
) -> io::Result<Response<ResponseBody>> {
    validate_request(&request, endpoint)?;
    let secrets = Arc::new(session.request(endpoint).map_err(|_| failure())?);
    let (mut parts, body) = request.into_parts();
    let body = body_bytes(body, REQUEST_LIMIT).await?;
    remove_hop_headers(&mut parts.headers)?;
    for (name, value) in parts.headers.iter_mut() {
        if ordinary_header(name) {
            let replaced = secrets
                .substitute(value.as_bytes(), HEADER_LIMIT)
                .map_err(|_| failure())?;
            *value = HeaderValue::from_bytes(&replaced).map_err(|_| failure())?;
        }
    }
    parts
        .headers
        .insert("accept-encoding", HeaderValue::from_static("identity"));
    set_framing(&mut parts.headers, body.len())?;
    validate_header_limits(&parts.headers)?;
    let request = Request::from_parts(parts, Full::new(body));
    let (mut sender, connection) = hyper::client::conn::http1::Builder::new()
        .max_headers(HEADER_COUNT)
        .max_buf_size(HEADER_LIMIT)
        .handshake(TokioIo::new(HeaderGuard::response(remote, secrets.clone())))
        .await
        .map_err(|_| failure())?;
    let mut connection: crate::secret_stream::Driver = Box::pin(connection);
    let response = {
        let sending = sender.send_request(request);
        tokio::pin!(sending);
        tokio::select! {
            response = &mut sending => response.map_err(|_| failure())?,
            result = &mut connection => {
                result.map_err(|_| failure())?;
                connection = Box::pin(std::future::pending());
                sending.await.map_err(|_| failure())?
            }
        }
    };
    transform_response(response, secrets, connection, sender)
}

fn validate_request(request: &Request<Incoming>, endpoint: &EgressEndpoint) -> io::Result<()> {
    let uri = request.uri();
    if uri.scheme().is_some()
        || uri.authority().is_some()
        || !uri.path().starts_with('/')
        || matches!(
            *request.method(),
            hyper::Method::CONNECT | hyper::Method::TRACE
        )
        || request.version() != hyper::Version::HTTP_11
    {
        return Err(failure());
    }
    let headers = request.headers();
    let mut hosts = headers.get_all("host").iter();
    let host = hosts
        .next()
        .ok_or_else(failure)?
        .to_str()
        .map_err(|_| failure())?;
    if hosts.next().is_some() {
        return Err(failure());
    }
    let explicit = format!("{}:{}", endpoint.host(), endpoint.port());
    if !host.eq_ignore_ascii_case(&explicit)
        && !(endpoint.port() == 443 && host.eq_ignore_ascii_case(endpoint.host()))
    {
        return Err(failure());
    }
    for name in ["upgrade", "expect", "trailer", "range", "if-range"] {
        if headers.contains_key(name) {
            return Err(failure());
        }
    }
    if !identity_encoding(headers)? {
        return Err(failure());
    }
    validate_header_limits(headers)
}

fn ordinary_header(name: &HeaderName) -> bool {
    !matches!(
        name.as_str(),
        "host"
            | "content-length"
            | "transfer-encoding"
            | "content-encoding"
            | "accept-encoding"
            | "connection"
            | "proxy-connection"
            | "keep-alive"
            | "te"
            | "trailer"
            | "upgrade"
            | "expect"
            | "range"
            | "if-range"
            | "proxy-authorization"
            | "proxy-authenticate"
    )
}

fn identity_encoding(headers: &HeaderMap) -> io::Result<bool> {
    for value in headers.get_all("content-encoding") {
        if !value
            .to_str()
            .map_err(|_| failure())?
            .trim()
            .eq_ignore_ascii_case("identity")
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn remove_hop_headers(headers: &mut HeaderMap) -> io::Result<()> {
    let mut named = Vec::new();
    for value in headers.get_all("connection") {
        for name in value.to_str().map_err(|_| failure())?.split(',') {
            let name = HeaderName::from_bytes(name.trim().as_bytes()).map_err(|_| failure())?;
            if matches!(
                name.as_str(),
                "host" | "content-length" | "transfer-encoding" | "content-encoding"
            ) {
                return Err(failure());
            }
            named.push(name);
        }
    }
    for name in named {
        headers.remove(name);
    }
    for name in [
        "connection",
        "proxy-connection",
        "keep-alive",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        "proxy-authorization",
        "proxy-authenticate",
    ] {
        headers.remove(name);
    }
    Ok(())
}

fn set_framing(headers: &mut HeaderMap, length: usize) -> io::Result<()> {
    headers.remove("transfer-encoding");
    headers.insert(
        "content-length",
        HeaderValue::from_str(&length.to_string()).map_err(|_| failure())?,
    );
    headers.insert("connection", HeaderValue::from_static("close"));
    Ok(())
}

fn validate_header_limits(headers: &HeaderMap) -> io::Result<()> {
    let mut bytes = 0usize;
    if headers.len() > HEADER_COUNT {
        return Err(failure());
    }
    for (name, value) in headers {
        bytes = bytes
            .checked_add(name.as_str().len() + value.as_bytes().len() + 4)
            .ok_or_else(failure)?;
        if bytes > HEADER_LIMIT {
            return Err(failure());
        }
    }
    Ok(())
}

async fn body_bytes(mut body: Incoming, limit: usize) -> io::Result<Bytes> {
    let mut result = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| failure())?;
        if frame.is_trailers() {
            return Err(failure());
        }
        if let Ok(data) = frame.into_data() {
            if data.len() > limit.saturating_sub(result.len()) {
                return Err(failure());
            }
            result.extend_from_slice(&data);
        }
    }
    Ok(Bytes::from(result))
}

fn transform_response(
    response: Response<Incoming>,
    secrets: Arc<RequestSecrets>,
    connection: crate::secret_stream::Driver,
    sender: hyper::client::conn::http1::SendRequest<HttpBody>,
) -> io::Result<Response<ResponseBody>> {
    if response.status().is_informational()
        || response.status() == StatusCode::PARTIAL_CONTENT
        || response.headers().contains_key("content-range")
        || response.headers().contains_key("upgrade")
        || response.headers().contains_key("trailer")
        || !identity_encoding(response.headers())?
    {
        return Err(failure());
    }
    let (parts, body) = response.into_parts();
    if body.size_hint().lower() > crate::secret_stream::RESPONSE_LIMIT as u64 {
        return Err(failure());
    }
    let mut response = Response::new(
        crate::secret_stream::RedactedBody::new(body, connection, sender, secrets.clone())
            .boxed_unsync(),
    );
    *response.status_mut() = parts.status;
    let mut headers = parts.headers;
    remove_hop_headers(&mut headers)?;
    for (name, value) in &headers {
        let scrubbed_name = secrets
            .scrub(name.as_str().as_bytes(), HEADER_LIMIT)
            .map_err(|_| failure())?;
        if scrubbed_name != name.as_str().as_bytes()
            || matches!(
                name.as_str(),
                "content-length"
                    | "etag"
                    | "digest"
                    | "content-digest"
                    | "repr-digest"
                    | "content-md5"
            )
        {
            continue;
        }
        let scrubbed_value = secrets
            .scrub(value.as_bytes(), HEADER_LIMIT)
            .map_err(|_| failure())?;
        response.headers_mut().append(
            name.clone(),
            HeaderValue::from_bytes(&scrubbed_value).map_err(|_| failure())?,
        );
    }
    response
        .headers_mut()
        .insert("connection", HeaderValue::from_static("close"));
    validate_header_limits(response.headers())?;
    Ok(response)
}

fn empty_error() -> Response<ResponseBody> {
    let body = Full::new(Bytes::new())
        .map_err(|never| match never {})
        .boxed_unsync();
    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::BAD_GATEWAY;
    response
        .headers_mut()
        .insert("content-length", HeaderValue::from_static("0"));
    response
        .headers_mut()
        .insert("connection", HeaderValue::from_static("close"));
    response
}

pub(crate) struct Prefix<T> {
    inner: T,
    bytes: Vec<u8>,
    offset: usize,
}
impl<T> Prefix<T> {
    pub(crate) fn new(inner: T, bytes: Vec<u8>) -> Self {
        Self {
            inner,
            bytes,
            offset: 0,
        }
    }
}
impl<T: AsyncRead + Unpin> AsyncRead for Prefix<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.offset < this.bytes.len() {
            let count = output.remaining().min(this.bytes.len() - this.offset);
            output.put_slice(&this.bytes[this.offset..this.offset + count]);
            this.offset += count;
            Poll::Ready(Ok(()))
        } else {
            Pin::new(&mut this.inner).poll_read(cx, output)
        }
    }
}
impl<T: AsyncWrite + Unpin> AsyncWrite for Prefix<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, data)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

struct HeaderGuard<T> {
    io: Prefix<T>,
    checked: bool,
    secrets: Option<Arc<RequestSecrets>>,
}
impl<T> HeaderGuard<T> {
    fn new(inner: T) -> Self {
        Self {
            io: Prefix::new(inner, Vec::new()),
            checked: false,
            secrets: None,
        }
    }
    fn response(inner: T, secrets: Arc<RequestSecrets>) -> Self {
        Self {
            io: Prefix::new(inner, Vec::new()),
            checked: false,
            secrets: Some(secrets),
        }
    }
}
impl<T: AsyncRead + Unpin> AsyncRead for HeaderGuard<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        while !this.checked {
            if let Some(end) = this
                .io
                .bytes
                .windows(4)
                .position(|bytes| bytes == b"\r\n\r\n")
            {
                check_wire_headers(&this.io.bytes[..end + 4])?;
                if let Some(secrets) = &this.secrets {
                    let head = scrub_wire_header_names(&this.io.bytes[..end + 4], secrets)?;
                    let body = this.io.bytes.split_off(end + 4);
                    this.io.bytes = head;
                    this.io.bytes.extend_from_slice(&body);
                }
                this.checked = true;
                break;
            }
            if this.io.bytes.len() >= HEADER_LIMIT {
                return Poll::Ready(Err(failure()));
            }
            let mut buffer = [0u8; 1024];
            let available = (HEADER_LIMIT - this.io.bytes.len()).min(buffer.len());
            let mut buffer = ReadBuf::new(&mut buffer[..available]);
            match Pin::new(&mut this.io.inner).poll_read(cx, &mut buffer) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(_)) => return Poll::Ready(Err(failure())),
                Poll::Ready(Ok(())) if buffer.filled().is_empty() => {
                    return Poll::Ready(Err(failure()));
                }
                Poll::Ready(Ok(())) => this.io.bytes.extend_from_slice(buffer.filled()),
            }
        }
        Pin::new(&mut this.io).poll_read(cx, output)
    }
}
impl<T: AsyncWrite + Unpin> AsyncWrite for HeaderGuard<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().io).poll_write(cx, data)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_shutdown(cx)
    }
}

pub(crate) fn check_wire_headers(bytes: &[u8]) -> io::Result<()> {
    let mut length = false;
    let mut transfer = false;
    let mut host = false;
    let mut count = 0;
    let text = std::str::from_utf8(bytes).map_err(|_| failure())?;
    for line in text.split("\r\n").skip(1).filter(|line| !line.is_empty()) {
        count += 1;
        if count > HEADER_COUNT || line.starts_with([' ', '\t']) {
            return Err(failure());
        }
        let (name, value) = line.split_once(':').ok_or_else(failure)?;
        if name.eq_ignore_ascii_case("content-length") {
            if length
                || value.trim().is_empty()
                || !value.trim().bytes().all(|b| b.is_ascii_digit())
            {
                return Err(failure());
            }
            length = true;
        }
        if name.eq_ignore_ascii_case("transfer-encoding") {
            if transfer || !value.trim().eq_ignore_ascii_case("chunked") {
                return Err(failure());
            }
            transfer = true;
        }
        if name.eq_ignore_ascii_case("host") {
            if host {
                return Err(failure());
            }
            host = true;
        }
    }
    if length && transfer {
        return Err(failure());
    }
    Ok(())
}

fn scrub_wire_header_names(bytes: &[u8], secrets: &RequestSecrets) -> io::Result<Vec<u8>> {
    let text = std::str::from_utf8(bytes).map_err(|_| failure())?;
    let mut lines = text.split("\r\n");
    let status = lines.next().ok_or_else(failure)?;
    let code = status.split(' ').nth(1).ok_or_else(failure)?;
    if code.len() != 3 || !code.bytes().all(|b| b.is_ascii_digit()) || code.starts_with('1') {
        return Err(failure());
    }
    let mut output = Vec::with_capacity(bytes.len());
    output.extend_from_slice(status.as_bytes());
    output.extend_from_slice(b"\r\n");
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, _) = line.split_once(':').ok_or_else(failure)?;
        if secrets
            .scrub(name.as_bytes(), HEADER_LIMIT)
            .map_err(|_| failure())?
            != name.as_bytes()
        {
            let parsed_name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| failure())?;
            if !ordinary_header(&parsed_name) {
                return Err(failure());
            }
            continue;
        }
        output.extend_from_slice(line.as_bytes());
        output.extend_from_slice(b"\r\n");
    }
    output.extend_from_slice(b"\r\n");
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HttpConnectPolicy, SecretStore, SecretValue, secrets::LiveBindings};
    use std::collections::BTreeMap;
    use tokio::net::UnixStream;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    const VALUE: &str = "SyntheticSecretAbcd1234";

    struct Exchange {
        guest: Vec<u8>,
        body: Vec<u8>,
        complete: bool,
        upstream: Vec<u8>,
        placeholder: String,
    }

    fn decode_test_response(wire: &[u8]) -> (Vec<u8>, bool) {
        let Some(end) = wire.windows(4).position(|part| part == b"\r\n\r\n") else {
            return (Vec::new(), false);
        };
        let headers = String::from_utf8_lossy(&wire[..end]).to_ascii_lowercase();
        let mut rest = &wire[end + 4..];
        if !headers.contains("transfer-encoding: chunked") {
            let length = headers.lines().find_map(|line| {
                line.strip_prefix("content-length: ")
                    .and_then(|n| n.trim().parse::<usize>().ok())
            });
            return (rest.to_vec(), length == Some(rest.len()));
        }
        let mut body = Vec::new();
        loop {
            let Some(end) = rest.windows(2).position(|part| part == b"\r\n") else {
                return (body, false);
            };
            let Some(length) = std::str::from_utf8(&rest[..end])
                .ok()
                .and_then(|s| usize::from_str_radix(s, 16).ok())
            else {
                return (body, false);
            };
            rest = &rest[end + 2..];
            if length == 0 {
                return (body, rest.starts_with(b"\r\n"));
            }
            if rest.len() < length + 2 {
                body.extend_from_slice(&rest[..rest.len().min(length)]);
                return (body, false);
            }
            body.extend_from_slice(&rest[..length]);
            if &rest[length..length + 2] != b"\r\n" {
                return (body, false);
            }
            rest = &rest[length + 2..];
        }
    }

    async fn run(
        request: impl FnOnce(&str) -> String,
        reply: Vec<u8>,
        trusted: bool,
        rotate: bool,
    ) -> Exchange {
        let endpoint = EgressEndpoint::new("api.example.com", 443).unwrap();
        let store = SecretStore::new();
        let metadata = store
            .create(
                "test",
                SecretValue::new(VALUE).unwrap(),
                HttpConnectPolicy::new([endpoint.clone()]).unwrap(),
            )
            .unwrap();
        let bindings = LiveBindings::new(BTreeMap::from([("TOKEN".into(), metadata.id)]));
        let session = SecretSession::new(store.clone(), bindings);
        let upstream_authority = Authority::new().unwrap();
        let mut authority = Authority::new().unwrap();
        if trusted {
            authority.trust_test_authority(&upstream_authority);
        }
        let authority = Arc::new(authority);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let placeholder = metadata.placeholder.clone();
        let upstream_config = upstream_authority.test_server_config(endpoint.host());
        let upstream = tokio::spawn(async move {
            let (remote, _) = listener.accept().await.unwrap();
            let Ok(mut remote) = TlsAcceptor::from(upstream_config).accept(remote).await else {
                return Vec::new();
            };
            let mut request = Vec::new();
            let mut block = [0; 1024];
            loop {
                let count = remote.read(&mut block).await.unwrap_or(0);
                if count == 0 {
                    return request;
                }
                request.extend_from_slice(&block[..count]);
                if let Some(end) = request.windows(4).position(|b| b == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&request[..end]);
                    let length = header
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            if rotate {
                store
                    .rotate(
                        metadata.id,
                        SecretValue::new("RotatedSyntheticValue5678").unwrap(),
                    )
                    .unwrap();
            }
            let _ = remote.write_all(&reply).await;
            let _ = remote.shutdown().await;
            request
        });
        let remote = TcpStream::connect(address).await.unwrap();
        let (client, server) = UnixStream::pair().unwrap();
        let inspection = Inspection::new(authority.clone(), session);
        let inspection =
            tokio::spawn(
                async move { inspection.serve(server, Vec::new(), remote, endpoint).await },
            );
        let mut client = TlsConnector::from(authority.test_client_config())
            .connect(ServerName::try_from("api.example.com").unwrap(), client)
            .await
            .unwrap();
        client
            .write_all(request(&placeholder).as_bytes())
            .await
            .unwrap();
        let mut guest = Vec::new();
        let _ = timeout(Duration::from_secs(3), client.read_to_end(&mut guest))
            .await
            .unwrap();
        let _ = inspection.await.unwrap();
        let (body, complete) = decode_test_response(&guest);
        Exchange {
            guest,
            body,
            complete,
            upstream: upstream.await.unwrap(),
            placeholder,
        }
    }

    fn get(token: &str) -> String {
        format!(
            "GET /v1?echo={token} HTTP/1.1\r\nHost: api.example.com\r\nAuthorization: Bearer {token}\r\nAccept-Encoding: gzip\r\nConnection: close\r\n\r\n"
        )
    }

    #[tokio::test]
    async fn replaces_only_header_values_and_scrubs_complete_response() {
        let body = format!("echo:{VALUE}");
        let reply = format!(
            "HTTP/1.1 200 {VALUE}\r\nX-{VALUE}: ignored\r\nX-Echo: {VALUE}\r\nContent-Length: {}\r\nETag: stale\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let exchange = run(get, reply.into_bytes(), true, false).await;
        let upstream = String::from_utf8(exchange.upstream).unwrap();
        let guest = String::from_utf8(exchange.guest).unwrap();
        assert!(upstream.contains(&format!("authorization: Bearer {VALUE}")));
        assert!(upstream.contains(&format!("/v1?echo={}", exchange.placeholder)));
        assert!(upstream.contains("accept-encoding: identity"));
        assert!(!guest.contains(VALUE));
        assert!(!guest.contains("x-syntheticsecretabcd1234"));
        assert!(!guest.contains("etag:"));
        assert!(guest.contains(&format!("x-echo: {}", exchange.placeholder)));
        assert!(exchange.complete);
        assert_eq!(
            exchange.body,
            format!("echo:{}", exchange.placeholder).as_bytes()
        );
        assert!(guest.starts_with("HTTP/1.1 200 OK"));
    }

    #[tokio::test]
    async fn keeps_old_value_snapshot_when_rotated_during_request() {
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{VALUE}",
            VALUE.len()
        );
        let exchange = run(get, reply.into_bytes(), true, true).await;
        let guest = String::from_utf8(exchange.guest).unwrap();
        assert!(!guest.contains(VALUE));
        assert!(exchange.complete);
        assert_eq!(exchange.body, exchange.placeholder.as_bytes());
    }

    #[tokio::test]
    async fn scrubs_values_split_across_response_chunks() {
        let (a, b) = VALUE.split_at(8);
        let reply = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{a}\r\n{:x}\r\n{b}\r\n0\r\n\r\n",
            a.len(),
            b.len()
        );
        let exchange = run(get, reply.into_bytes(), true, false).await;
        let guest = String::from_utf8(exchange.guest).unwrap();
        assert!(exchange.complete);
        assert_eq!(exchange.body, exchange.placeholder.as_bytes());
        assert!(guest.contains("transfer-encoding: chunked"));
    }

    #[tokio::test]
    async fn rejects_encoded_truncated_trailered_and_ambiguous_responses() {
        for reply in [
            b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 3\r\n\r\nraw".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort".to_vec(),
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nraw\r\n0\r\nX-Trailer: raw\r\n\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 3\r\n\r\n3\r\nraw\r\n0\r\n\r\n".to_vec(),
            b"HTTP/1.1 103 Early Hints\r\nX-Hint: raw\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec(),
        ] {
            let exchange = run(get, reply, true, false).await;
            let guest = String::from_utf8(exchange.guest).unwrap();
            assert!(guest.starts_with("HTTP/1.1 502 Bad Gateway") || !exchange.complete, "{guest}");
            assert!(!guest.contains(VALUE));
        }
    }

    #[tokio::test]
    async fn upstream_certificate_validation_is_required() {
        let exchange = run(get, Vec::new(), false, false).await;
        assert!(exchange.upstream.is_empty());
        assert!(exchange.guest.is_empty());
    }

    #[tokio::test]
    async fn mismatched_host_is_not_forwarded() {
        for bad_host in [
            "attacker.example.com",
            "user@api.example.com",
            "api.example.com:999999",
            "api.example.com:444",
        ] {
            let exchange = run(
                |token| get(token).replace("Host: api.example.com", &format!("Host: {bad_host}")),
                Vec::new(),
                true,
                false,
            )
            .await;
            assert!(exchange.upstream.is_empty());
            assert!(
                String::from_utf8(exchange.guest)
                    .unwrap()
                    .starts_with("HTTP/1.1 502")
            );
        }
    }

    #[tokio::test]
    async fn request_body_keeps_literal_placeholder() {
        let exchange = run(|token| format!("POST / HTTP/1.1\r\nHost: api.example.com:443\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{token}", token.len()),
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec(), true, false).await;
        let upstream = String::from_utf8(exchange.upstream).unwrap();
        assert!(upstream.contains(&format!("authorization: Bearer {VALUE}")));
        assert!(upstream.ends_with(&exchange.placeholder));
        assert!(
            String::from_utf8(exchange.guest)
                .unwrap()
                .starts_with("HTTP/1.1 200")
        );
    }

    #[tokio::test]
    async fn requires_incoming_sni_even_when_client_trusts_the_leaf() {
        let endpoint = EgressEndpoint::new("api.example.com", 443).unwrap();
        let authority = Arc::new(Authority::new().unwrap());
        let session = SecretSession::new(SecretStore::new(), LiveBindings::new(BTreeMap::new()));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let remote = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (client, server) = UnixStream::pair().unwrap();
        let inspection = Inspection::new(authority.clone(), session);
        let task =
            tokio::spawn(
                async move { inspection.serve(server, Vec::new(), remote, endpoint).await },
            );
        let mut config = authority.test_client_config();
        Arc::get_mut(&mut config).unwrap().enable_sni = false;
        let mut client = TlsConnector::from(config)
            .connect(ServerName::try_from("api.example.com").unwrap(), client)
            .await
            .unwrap();
        let _ = client.write_all(get("opaque").as_bytes()).await;
        let mut response = Vec::new();
        let _ = client.read_to_end(&mut response).await;
        assert!(response.is_empty());
        assert!(task.await.unwrap().is_err());
    }

    #[test]
    fn header_name_scrubbing_cannot_hide_content_encoding() {
        let endpoint = EgressEndpoint::new("api.example.com", 443).unwrap();
        let store = SecretStore::new();
        let metadata = store
            .create(
                "encoding",
                SecretValue::new("content-encoding").unwrap(),
                HttpConnectPolicy::new([endpoint.clone()]).unwrap(),
            )
            .unwrap();
        let session = SecretSession::new(
            store,
            LiveBindings::new(BTreeMap::from([("TOKEN".into(), metadata.id)])),
        );
        let snapshot = session.request(&endpoint).unwrap();
        assert!(
            scrub_wire_header_names(
                b"HTTP/1.1 200 OK\r\ncontent-encoding: gzip\r\n\r\n",
                &snapshot
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_ambiguous_framing_and_routing_before_hyper_normalization() {
        for headers in [
            "GET / HTTP/1.1\r\nHost: a\r\nHost: b\r\n\r\n",
            "GET / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\n",
            "GET / HTTP/1.1\r\nContent-Length: 1\r\nTransfer-Encoding: chunked\r\n\r\n",
            "GET / HTTP/1.1\r\nTransfer-Encoding: gzip, chunked\r\n\r\n",
            "GET / HTTP/1.1\r\nX-Test: yes\r\n folded\r\n\r\n",
        ] {
            assert!(check_wire_headers(headers.as_bytes()).is_err());
        }
    }

    #[tokio::test]
    async fn prefix_replays_overread_bytes_before_transport() {
        let (mut writer, reader) = tokio::io::duplex(16);
        writer.write_all(b"tail").await.unwrap();
        writer.shutdown().await.unwrap();
        let mut prefix = Prefix::new(reader, b"prefix".to_vec());
        let mut output = Vec::new();
        prefix.read_to_end(&mut output).await.unwrap();
        assert_eq!(output, b"prefixtail");
    }
    #[tokio::test]
    async fn cancelled_inspection_closes_stalled_upstream() {
        let endpoint = EgressEndpoint::new("api.example.com", 443).unwrap();
        let store = SecretStore::new();
        let secret = store
            .create(
                "test",
                SecretValue::new(VALUE).unwrap(),
                HttpConnectPolicy::new([endpoint.clone()]).unwrap(),
            )
            .unwrap();
        let session = SecretSession::new(
            store,
            LiveBindings::new(BTreeMap::from([("TOKEN".into(), secret.id)])),
        );
        let upstream_authority = Authority::new().unwrap();
        let mut authority = Authority::new().unwrap();
        authority.trust_test_authority(&upstream_authority);
        let authority = Arc::new(authority);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = upstream_authority.test_server_config(endpoint.host());
        let (ready, admitted) = tokio::sync::oneshot::channel();
        let upstream = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = TlsAcceptor::from(server).accept(socket).await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut buffer = [0; 1024];
                let count = socket.read(&mut buffer).await.unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|part| part == b"\r\n\r\n") {
                    break;
                }
            }
            ready.send(()).unwrap();
            let mut byte = [0];
            let result = timeout(Duration::from_secs(2), socket.read(&mut byte))
                .await
                .unwrap();
            assert!(
                matches!(result, Ok(0) | Err(_)),
                "upstream survived cancellation"
            );
        });
        let remote = TcpStream::connect(address).await.unwrap();
        let (client, server) = UnixStream::pair().unwrap();
        let inspection = Inspection::new(authority.clone(), session);
        let running =
            tokio::spawn(
                async move { inspection.serve(server, Vec::new(), remote, endpoint).await },
            );
        let mut client = TlsConnector::from(authority.test_client_config())
            .connect(ServerName::try_from("api.example.com").unwrap(), client)
            .await
            .unwrap();
        client
            .write_all(get(&secret.placeholder).as_bytes())
            .await
            .unwrap();
        timeout(Duration::from_secs(2), admitted)
            .await
            .unwrap()
            .unwrap();
        running.abort();
        assert!(running.await.unwrap_err().is_cancelled());
        upstream.await.unwrap();
    }
    async fn streaming_exchange(delay: Duration, cancel_after_first: bool) {
        let endpoint = EgressEndpoint::new("api.example.com", 443).unwrap();
        let store = SecretStore::new();
        let secret = store
            .create(
                "stream",
                SecretValue::new(VALUE).unwrap(),
                HttpConnectPolicy::new([endpoint.clone()]).unwrap(),
            )
            .unwrap();
        let session = SecretSession::new(
            store.clone(),
            LiveBindings::new(BTreeMap::from([("TOKEN".into(), secret.id)])),
        );
        let upstream_authority = Authority::new().unwrap();
        let mut authority = Authority::new().unwrap();
        authority.trust_test_authority(&upstream_authority);
        let authority = Arc::new(authority);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let config = upstream_authority.test_server_config(endpoint.host());
        let (continue_tx, continue_rx) = tokio::sync::oneshot::channel::<bool>();
        let upstream = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = TlsAcceptor::from(config).accept(socket).await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut bytes = [0; 1024];
                let n = socket.read(&mut bytes).await.unwrap();
                assert_ne!(n, 0);
                request.extend_from_slice(&bytes[..n]);
                if request.windows(4).any(|b| b == b"\r\n\r\n") {
                    break;
                }
            }
            assert!(String::from_utf8_lossy(&request).contains(VALUE));
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
            socket.write_all(b"d\r\ndata: first\n\n\r\n").await.unwrap();
            socket.flush().await.unwrap();
            if !continue_rx.await.unwrap() {
                let mut byte = [0];
                let result = timeout(Duration::from_secs(2), socket.read(&mut byte))
                    .await
                    .expect("upstream remained open after downstream cancellation");
                assert!(matches!(result, Ok(0) | Err(_)));
                return;
            }
            tokio::time::sleep(delay).await;
            for part in [
                format!("data: {}", &VALUE[..8]),
                format!("{}\n\n", &VALUE[8..]),
            ] {
                socket
                    .write_all(format!("{:x}\r\n{part}\r\n", part.len()).as_bytes())
                    .await
                    .unwrap();
                socket.flush().await.unwrap();
                tokio::task::yield_now().await;
            }
            socket.write_all(b"0\r\n\r\n").await.unwrap();
            socket.shutdown().await.unwrap();
        });
        let remote = TcpStream::connect(address).await.unwrap();
        let (client, server) = UnixStream::pair().unwrap();
        let inspect = Inspection::new(authority.clone(), session);
        let inspecting =
            tokio::spawn(async move { inspect.serve(server, Vec::new(), remote, endpoint).await });
        let mut client = TlsConnector::from(authority.test_client_config())
            .connect(ServerName::try_from("api.example.com").unwrap(), client)
            .await
            .unwrap();
        client
            .write_all(get(&secret.placeholder).as_bytes())
            .await
            .unwrap();
        let mut wire = Vec::new();
        timeout(Duration::from_secs(2), async {
            loop {
                let mut buffer = [0; 1024];
                let n = client.read(&mut buffer).await.unwrap();
                assert_ne!(n, 0, "stream ended before its first event");
                wire.extend_from_slice(&buffer[..n]);
                if wire.windows(13).any(|b| b == b"data: first\n\n") {
                    break;
                }
            }
        })
        .await
        .expect("first event was buffered until the end");
        if cancel_after_first {
            drop(client);
            continue_tx.send(false).unwrap();
            upstream.await.unwrap();
            let _ = inspecting.await.unwrap();
            return;
        }
        store
            .rotate(
                secret.id,
                SecretValue::new("new-secret-after-admission-1234").unwrap(),
            )
            .unwrap();
        continue_tx.send(true).unwrap();
        timeout(
            delay + Duration::from_secs(5),
            client.read_to_end(&mut wire),
        )
        .await
        .unwrap()
        .unwrap();
        upstream.await.unwrap();
        inspecting.await.unwrap().unwrap();
        let (body, complete) = decode_test_response(&wire);
        assert!(complete);
        assert_eq!(
            body,
            format!("data: first\n\ndata: {}\n\n", secret.placeholder).as_bytes()
        );
        assert!(!String::from_utf8_lossy(&wire).contains(VALUE));
    }

    #[tokio::test]
    async fn sse_is_delivered_before_completion_and_redacts_across_chunks_after_rotation() {
        streaming_exchange(Duration::ZERO, false).await;
    }

    #[tokio::test]
    #[ignore = "31-second regression for long LLM responses; run scripts/test-local.sh"]
    async fn llm_stream_survives_the_old_thirty_second_deadline() {
        streaming_exchange(Duration::from_secs(31), false).await;
    }

    #[tokio::test]
    async fn truncated_stream_never_flushes_an_undecided_secret_prefix() {
        let partial = &VALUE[..12];
        let reply = format!("HTTP/1.1 200 OK\r\nContent-Length: 999\r\n\r\ndata: {partial}");
        let exchange = run(get, reply.into_bytes(), true, false).await;
        assert!(!exchange.complete);
        assert!(!String::from_utf8_lossy(&exchange.body).contains(partial));
    }
    #[tokio::test]
    async fn cancelling_after_stream_headers_closes_upstream() {
        streaming_exchange(Duration::ZERO, true).await;
    }
}
