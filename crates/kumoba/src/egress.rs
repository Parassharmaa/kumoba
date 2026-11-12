use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    ffi::CString,
    io,
    net::{Ipv4Addr, SocketAddrV4},
    path::Path,
    pin::Pin,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, SystemTime},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::{TcpStream, UnixListener, UnixStream},
    sync::{Semaphore, oneshot, watch},
    task::{JoinHandle, JoinSet},
    time::{Instant, timeout},
};

const MAX_ENDPOINTS: usize = 128;
const MAX_HEADERS: usize = 8 * 1024;
const MAX_ADDRESSES: usize = 64;
const MAX_CONNECTIONS: usize = 16;
const MAX_WEB_CONNECTIONS: usize = 32;
const SETUP_TIMEOUT: Duration = Duration::from_secs(5);
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const TUNNEL_TIMEOUT: Duration = Duration::from_secs(3600);
static DNS_SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "EndpointData")]
pub struct EgressEndpoint {
    host: String,
    port: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EndpointData {
    host: String,
    port: u16,
}

impl TryFrom<EndpointData> for EgressEndpoint {
    type Error = Error;
    fn try_from(value: EndpointData) -> Result<Self> {
        Self::new(value.host, value.port)
    }
}

impl EgressEndpoint {
    pub(crate) fn host(&self) -> &str {
        &self.host
    }
    pub(crate) fn port(&self) -> u16 {
        self.port
    }
    pub fn new(host: impl Into<String>, port: u16) -> Result<Self> {
        let host = host.into();
        let host = host.strip_suffix('.').unwrap_or(&host);
        if port == 0
            || host.is_empty()
            || host.len() > 253
            || !host.is_ascii()
            || host.parse::<std::net::IpAddr>().is_ok()
            || numeric_ipv4_name(host)
            || host.split('.').any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || !label.as_bytes()[0].is_ascii_alphanumeric()
                    || !label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                    || !label
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            })
        {
            return Err(Error::InvalidInput(
                "egress requires an ASCII DNS hostname and a nonzero port".into(),
            ));
        }
        Ok(Self {
            host: host.to_ascii_lowercase(),
            port,
        })
    }
}

fn numeric_ipv4_name(host: &str) -> bool {
    let mut labels = host.split('.');
    labels.clone().count() <= 4
        && labels.all(|label| {
            if let Some(hex) = label
                .strip_prefix("0x")
                .or_else(|| label.strip_prefix("0X"))
            {
                !hex.is_empty() && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
            } else {
                !label.is_empty() && label.bytes().all(|byte| byte.is_ascii_digit())
            }
        })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "PolicyData")]
pub struct HttpConnectPolicy {
    endpoints: Vec<EgressEndpoint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    domains: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    public_web: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyData {
    endpoints: Vec<EgressEndpoint>,
    #[serde(default)]
    domains: Vec<String>,
    #[serde(default)]
    public_web: bool,
}

impl TryFrom<PolicyData> for HttpConnectPolicy {
    type Error = Error;
    fn try_from(value: PolicyData) -> Result<Self> {
        if value.public_web {
            if !value.endpoints.is_empty() || !value.domains.is_empty() {
                return Err(Error::InvalidInput(
                    "public-web policy cannot contain endpoint grants".into(),
                ));
            }
            Ok(Self::public_web())
        } else {
            Self::with_domains(value.endpoints, value.domains)
        }
    }
}

impl HttpConnectPolicy {
    pub fn new(endpoints: impl IntoIterator<Item = EgressEndpoint>) -> Result<Self> {
        Self::with_domains(endpoints, Vec::new())
    }

    pub fn with_domains(
        endpoints: impl IntoIterator<Item = EgressEndpoint>,
        domains: impl IntoIterator<Item = String>,
    ) -> Result<Self> {
        let mut unique = BTreeSet::new();
        for (index, endpoint) in endpoints.into_iter().enumerate() {
            if index >= MAX_ENDPOINTS {
                return Err(Error::InvalidInput(
                    "egress allows at most 128 endpoint entries".into(),
                ));
            }
            unique.insert(endpoint);
        }
        let mut unique_domains = BTreeSet::new();
        for (index, domain) in domains.into_iter().enumerate() {
            if index >= MAX_ENDPOINTS {
                return Err(Error::InvalidInput(
                    "egress allows at most 128 domain entries".into(),
                ));
            }
            unique_domains.insert(EgressEndpoint::new(domain, 443)?.host);
        }
        if unique.is_empty() && unique_domains.is_empty() {
            return Err(Error::InvalidInput(
                "egress endpoint policy is empty".into(),
            ));
        }
        Ok(Self {
            endpoints: unique.into_iter().collect(),
            domains: unique_domains.into_iter().collect(),
            public_web: false,
        })
    }

    pub fn public_web() -> Self {
        Self {
            endpoints: Vec::new(),
            domains: Vec::new(),
            public_web: true,
        }
    }
    pub(crate) fn is_public_web(&self) -> bool {
        self.public_web
    }
    pub(crate) fn endpoints(&self) -> &[EgressEndpoint] {
        &self.endpoints
    }

    pub(crate) fn allows(&self, endpoint: &EgressEndpoint) -> bool {
        if self.public_web {
            return matches!(endpoint.port, 80 | 443);
        }
        self.endpoints.binary_search(endpoint).is_ok()
            || (matches!(endpoint.port, 80 | 443)
                && self.domains.iter().any(|domain| {
                    endpoint.host.len() > domain.len() + 1
                        && endpoint.host.ends_with(domain)
                        && endpoint.host.as_bytes()[endpoint.host.len() - domain.len() - 1]
                            == b'.'
                }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrivateAllow {
    Host(String),
    Cidr(Ipv4Addr, u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamProxy {
    host: String,
    port: u16,
    authorization: Option<String>,
    by_name: bool,
}

impl UpstreamProxy {
    pub fn parse(url: &str) -> Result<Self> {
        let rest = url
            .strip_prefix("http://")
            .ok_or_else(|| Error::InvalidInput("upstream proxy must be an http:// URL".into()))?;
        let rest = rest.trim_end_matches('/');
        let (credentials, authority) = match rest.rsplit_once('@') {
            Some((credentials, authority)) => (Some(credentials), authority),
            None => (None, rest),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (
                host,
                port.parse::<u16>()
                    .map_err(|_| Error::InvalidInput("invalid upstream proxy port".into()))?,
            ),
            None => (authority, 3128),
        };
        if host.is_empty() || host.contains(['/', '?', '#']) {
            return Err(Error::InvalidInput("invalid upstream proxy host".into()));
        }
        let authorization = credentials.map(|credentials| {
            use base64::Engine;
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(credentials)
            )
        });
        Ok(Self {
            host: host.to_string(),
            port,
            authorization,
            by_name: false,
        })
    }

    pub fn connect_by_name(mut self, by_name: bool) -> Self {
        self.by_name = by_name;
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EgressConfig {
    pub private_allow: Vec<PrivateAllow>,
    pub upstream_proxy: Option<UpstreamProxy>,
    pub no_proxy: Vec<String>,
}

fn host_matches(pattern: &str, host: &str) -> bool {
    let pattern = pattern.trim_start_matches('.').to_ascii_lowercase();
    let host = host.to_ascii_lowercase();
    !pattern.is_empty()
        && (host == pattern || host.ends_with(&format!(".{pattern}")))
}

fn cidr_contains(network: Ipv4Addr, prefix: u8, ip: Ipv4Addr) -> bool {
    let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - u32::from(prefix)) };
    u32::from(ip) & mask == u32::from(network) & mask
}

impl EgressConfig {
    pub fn parse(
        private_allow: &str,
        upstream_proxy: Option<&str>,
        no_proxy: &str,
    ) -> Result<Self> {
        let mut allow = Vec::new();
        for item in private_allow.split(',').map(str::trim).filter(|item| !item.is_empty()) {
            if let Some((network, prefix)) = item.split_once('/') {
                let network: Ipv4Addr = network
                    .parse()
                    .map_err(|_| Error::InvalidInput(format!("invalid CIDR {item}")))?;
                let prefix: u8 = prefix
                    .parse()
                    .ok()
                    .filter(|prefix| *prefix <= 32)
                    .ok_or_else(|| Error::InvalidInput(format!("invalid CIDR {item}")))?;
                allow.push(PrivateAllow::Cidr(network, prefix));
            } else if let Ok(ip) = item.parse::<Ipv4Addr>() {
                allow.push(PrivateAllow::Cidr(ip, 32));
            } else {
                EgressEndpoint::new(item, 443)?;
                allow.push(PrivateAllow::Host(item.to_ascii_lowercase()));
            }
        }
        Ok(Self {
            private_allow: allow,
            upstream_proxy: upstream_proxy
                .map(str::trim)
                .filter(|url| !url.is_empty())
                .map(UpstreamProxy::parse)
                .transpose()?,
            no_proxy: no_proxy
                .split(',')
                .map(str::trim)
                .map(|item| item.trim_start_matches('.').to_ascii_lowercase())
                .filter(|item| !item.is_empty())
                .collect(),
        })
    }

    fn proxy_for(&self, host: &str) -> Option<&UpstreamProxy> {
        let proxy = self.upstream_proxy.as_ref()?;
        let bypass = self.no_proxy.iter().any(|pattern| host_matches(pattern, host))
            || self.private_allow.iter().any(|allow| match allow {
                PrivateAllow::Host(pattern) => host_matches(pattern, host),
                PrivateAllow::Cidr(..) => false,
            });
        (!bypass).then_some(proxy)
    }

    pub(crate) fn private_allowed(&self, host: &str, ip: Ipv4Addr) -> bool {
        self.private_allow.iter().any(|allow| match allow {
            PrivateAllow::Host(pattern) => host_matches(pattern, host),
            PrivateAllow::Cidr(network, prefix) => cidr_contains(*network, *prefix, ip),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessRecord {
    pub host: String,
    pub port: u16,
    pub allowed: bool,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub started: SystemTime,
    pub completed: SystemTime,
}

pub(crate) type AccessSink = Arc<dyn Fn(AccessRecord) + Send + Sync>;

#[derive(Default)]
pub(crate) struct Counters {
    pub(crate) from_guest: AtomicU64,
    pub(crate) to_guest: AtomicU64,
}

pub(crate) struct Counted<S> {
    inner: S,
    counters: Arc<Counters>,
}

impl<S> Counted<S> {
    pub(crate) fn new(inner: S, counters: Arc<Counters>) -> Self {
        Self { inner, counters }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Counted<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &result {
            let read = (buf.filled().len() - before) as u64;
            self.counters.from_guest.fetch_add(read, Ordering::Relaxed);
        }
        result
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Counted<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write(cx, data);
        if let Poll::Ready(Ok(written)) = &result {
            self.counters
                .to_guest
                .fetch_add(*written as u64, Ordering::Relaxed);
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

pub(crate) struct AccessScope {
    sink: Option<AccessSink>,
    counters: Arc<Counters>,
    started: SystemTime,
}

impl AccessScope {
    pub(crate) fn new(sink: Option<AccessSink>) -> Self {
        Self {
            sink,
            counters: Arc::new(Counters::default()),
            started: SystemTime::now(),
        }
    }
    pub(crate) fn counters(&self) -> Arc<Counters> {
        self.counters.clone()
    }
    pub(crate) fn record(&self, host: &str, port: u16, allowed: bool) {
        let Some(sink) = &self.sink else { return };
        sink(AccessRecord {
            host: host.chars().take(253).collect(),
            port,
            allowed,
            bytes_in: self.counters.to_guest.load(Ordering::Relaxed),
            bytes_out: self.counters.from_guest.load(Ordering::Relaxed),
            started: self.started,
            completed: SystemTime::now(),
        });
    }
}

pub(crate) struct Broker {
    task: Option<JoinHandle<io::Result<()>>>,
    stop: Option<oneshot::Sender<()>>,
}

impl Broker {
    pub(crate) async fn bind(
        path: &Path,
        policy: HttpConnectPolicy,
        inspection: Option<crate::secret_https::Inspection>,
        sink: Option<AccessSink>,
        egress: Arc<EgressConfig>,
    ) -> io::Result<Self> {
        let listener = UnixListener::bind(path)?;
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(serve(
            listener,
            Arc::new(policy),
            inspection,
            sink,
            egress,
            stopped,
        ));
        Ok(Self {
            task: Some(task),
            stop: Some(stop),
        })
    }

    pub(crate) async fn wait(&mut self) -> io::Result<()> {
        let Some(task) = self.task.as_mut() else {
            return Ok(());
        };
        let result = task.await.map_err(io::Error::other);
        self.task = None;
        result?
    }

    pub(crate) async fn shutdown(&mut self) -> io::Result<()> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.wait().await
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

async fn serve(
    listener: UnixListener,
    policy: Arc<HttpConnectPolicy>,
    inspection: Option<crate::secret_https::Inspection>,
    sink: Option<AccessSink>,
    egress: Arc<EgressConfig>,
    mut stopped: oneshot::Receiver<()>,
) -> io::Result<()> {
    let mut relays = JoinSet::new();
    let limit = if policy.is_public_web() {
        MAX_WEB_CONNECTIONS
    } else {
        MAX_CONNECTIONS
    };
    let result = loop {
        tokio::select! {
            biased;
            _ = &mut stopped => break Ok(()),
            completed = relays.join_next(), if !relays.is_empty() => {
                if let Some(Err(error)) = completed {
                    break Err(io::Error::other(error));
                }
            }
            accepted = listener.accept(), if relays.len() < limit => {
                let (stream, _) = match accepted {
                    Ok(value) => value,
                    Err(error) => break Err(error),
                };
                let policy = policy.clone();
                let inspection = inspection.clone();
                let scope = AccessScope::new(sink.clone());
                let egress = egress.clone();
                relays.spawn(async move { let _ = connection(stream, &policy, inspection, scope, &egress).await; });
            }
        }
    };
    drop(listener);
    relays.shutdown().await;
    result
}

pub(crate) fn raw_target(authority: &str) -> Option<(String, u16)> {
    let (host, port) = authority.rsplit_once(':')?;
    let host = host.trim_matches(['[', ']']).trim_end_matches('.').to_ascii_lowercase();
    let valid = !host.is_empty()
        && host.len() <= 253
        && host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-.:".contains(&byte));
    valid.then(|| port.parse().ok().map(|port| (host, port)))?
}

fn raw_connect_target(head: &[u8]) -> Option<(String, u16)> {
    let line = head.split(|byte| *byte == b'\n').next()?;
    let line = std::str::from_utf8(line).ok()?;
    let mut parts = line.split_ascii_whitespace();
    (parts.next()? == "CONNECT").then(|| raw_target(parts.next()?))?
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn authority(value: &str) -> io::Result<EgressEndpoint> {
    let (host, port) = value
        .split_once(':')
        .ok_or_else(|| invalid("missing CONNECT port"))?;
    if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid("invalid CONNECT port"));
    }
    let port = port.parse().map_err(|_| invalid("invalid CONNECT port"))?;
    EgressEndpoint::new(host, port).map_err(|_| invalid("invalid CONNECT authority"))
}

fn parse_headers(bytes: &[u8]) -> io::Result<EgressEndpoint> {
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("invalid CONNECT headers"))?;
    if !text.is_ascii() || !text.ends_with("\r\n\r\n") {
        return Err(invalid("invalid CONNECT headers"));
    }
    let mut lines = text[..text.len() - 4].split("\r\n");
    let mut request = lines.next().unwrap_or("").split(' ');
    if request.next() != Some("CONNECT") {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "only CONNECT is supported",
        ));
    }
    let endpoint = authority(request.next().unwrap_or(""))?;
    if request.next() != Some("HTTP/1.1") || request.next().is_some() {
        return Err(invalid("invalid CONNECT request line"));
    }
    let mut host_seen = false;
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| invalid("invalid CONNECT header"))?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
            || value
                .bytes()
                .any(|byte| byte < 0x20 && byte != b'\t' || byte == 0x7f)
        {
            return Err(invalid("invalid CONNECT header"));
        }
        if name.eq_ignore_ascii_case("content-length")
            || name.eq_ignore_ascii_case("transfer-encoding")
        {
            return Err(invalid("CONNECT request bodies are not supported"));
        }
        if name.eq_ignore_ascii_case("host") {
            if host_seen || authority(value.trim_matches([' ', '\t']))? != endpoint {
                return Err(invalid("conflicting CONNECT Host header"));
            }
            host_seen = true;
        }
    }
    Ok(endpoint)
}

async fn read_head(stream: &mut UnixStream) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let mut bytes = Vec::with_capacity(MAX_HEADERS);
    let mut buffer = [0u8; 1024];
    loop {
        let available = (MAX_HEADERS - bytes.len()).min(buffer.len());
        if available == 0 {
            return Err(invalid("CONNECT headers exceed 8 KiB"));
        }
        let count = stream.read(&mut buffer[..available]).await?;
        if count == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let end = end + 4;
            let pending = bytes.split_off(end);
            return Ok((bytes, pending));
        }
    }
}

#[cfg(test)]
async fn read_request(stream: &mut UnixStream) -> io::Result<(EgressEndpoint, Vec<u8>)> {
    let (head, pending) = read_head(stream).await?;
    Ok((parse_headers(&head)?, pending))
}

async fn response(stream: &mut UnixStream, status: &'static [u8]) -> io::Result<()> {
    timeout(SETUP_TIMEOUT, stream.write_all(status)).await?
}

async fn connection(
    mut client: UnixStream,
    policy: &HttpConnectPolicy,
    inspection: Option<crate::secret_https::Inspection>,
    scope: AccessScope,
    egress: &EgressConfig,
) -> io::Result<()> {
    let (endpoint, pending) = match timeout(SETUP_TIMEOUT, read_head(&mut client)).await {
        Ok(Ok((head, pending))) => match parse_headers(&head) {
            Ok(endpoint) => (endpoint, pending),
            Err(error) if error.kind() == io::ErrorKind::Unsupported && policy.public_web => {
                let client = Counted::new(client, scope.counters());
                return crate::public_http::serve(client, head, pending, scope, egress).await;
            }
            Err(error) => {
                if let Some((host, port)) = raw_connect_target(&head) {
                    scope.record(&host, port, false);
                }
                let status: &[u8] = if error.kind() == io::ErrorKind::Unsupported {
                    b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                } else {
                    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                };
                response(&mut client, status).await?;
                return Ok(());
            }
        },
        result => {
            let status: &[u8] = match result {
                Ok(Err(error)) if error.kind() == io::ErrorKind::Unsupported => b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                _ => b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            };
            response(&mut client, status).await?;
            return Ok(());
        }
    };
    if !policy.allows(&endpoint) || (policy.public_web && endpoint.port != 443) {
        scope.record(endpoint.host(), endpoint.port(), false);
        response(
            &mut client,
            b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )
        .await?;
        return Ok(());
    }
    let remote = match remote_for(&endpoint, &inspection, egress).await {
        Ok(remote) => remote,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            scope.record(endpoint.host(), endpoint.port(), false);
            response(
                &mut client,
                b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await?;
            return Ok(());
        }
        Err(_) => {
            scope.record(endpoint.host(), endpoint.port(), true);
            response(
                &mut client,
                b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await?;
            return Ok(());
        }
    };
    response(&mut client, b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;
    scope
        .counters()
        .from_guest
        .fetch_add(pending.len() as u64, Ordering::Relaxed);
    let client = Counted::new(client, scope.counters());
    let result = tunnel(client, pending, remote, policy, inspection, &endpoint).await;
    scope.record(endpoint.host(), endpoint.port(), true);
    result
}

fn remote_for(
    endpoint: &EgressEndpoint,
    inspection: &Option<crate::secret_https::Inspection>,
    egress: &EgressConfig,
) -> impl Future<Output = io::Result<TcpStream>> + use<> {
    let endpoint = endpoint.clone();
    let egress = egress.clone();
    #[cfg(test)]
    let test_route = inspection
        .as_ref()
        .and_then(|inspection| inspection.test_route(&endpoint));
    #[cfg(not(test))]
    let test_route: Option<std::net::SocketAddr> = {
        let _ = inspection;
        None
    };
    async move {
        if let Some(address) = test_route {
            return TcpStream::connect(address).await;
        }
        connect(&endpoint, &egress).await
    }
}

async fn tunnel<S>(
    client: S,
    pending: Vec<u8>,
    mut remote: TcpStream,
    policy: &HttpConnectPolicy,
    inspection: Option<crate::secret_https::Inspection>,
    endpoint: &EgressEndpoint,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    if let Some(inspection) = inspection.filter(|inspection| inspection.covers(endpoint)) {
        return inspection
            .serve(client, pending, remote, endpoint.clone())
            .await;
    }
    timeout(SETUP_TIMEOUT, remote.write_all(&pending)).await??;
    relay(
        client,
        remote,
        if policy.is_public_web() {
            ClientLifetime::Web
        } else {
            ClientLifetime::HalfClose
        },
    )
    .await
}

pub(crate) async fn connect(
    endpoint: &EgressEndpoint,
    egress: &EgressConfig,
) -> io::Result<TcpStream> {
    let proxy = egress.proxy_for(&endpoint.host);
    if proxy.is_some() && looks_internal(&endpoint.host) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "internal host names are not tunnelled through the upstream proxy",
        ));
    }
    let permit = DNS_SLOTS
        .get_or_init(|| Arc::new(Semaphore::new(8)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "DNS capacity exhausted"))?;
    let target = endpoint.clone();
    let config = egress.clone();
    let lookup = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        resolve(&target, &config)
    });
    let addresses = timeout(SETUP_TIMEOUT, lookup)
        .await?
        .map_err(io::Error::other)??;
    if let Some(proxy) = proxy {
        let checked = *addresses
            .first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no destination addresses"))?;
        return connect_via_proxy(proxy, endpoint, checked).await;
    }
    timeout(SETUP_TIMEOUT, async {
        let mut last = io::Error::new(io::ErrorKind::NotFound, "no destination addresses");
        for address in addresses {
            match TcpStream::connect(address).await {
                Ok(stream) => return Ok(stream),
                Err(error) => last = error,
            }
        }
        Err(last)
    })
    .await?
}

const INTERNAL_NAME_SUFFIXES: &[&str] = &[
    ".localhost",
    ".local",
    ".internal",
    ".intranet",
    ".lan",
    ".home",
    ".corp",
    ".private",
];

fn looks_internal(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    host == "localhost"
        || !host.contains('.')
        || INTERNAL_NAME_SUFFIXES
            .iter()
            .any(|suffix| host.ends_with(suffix))
}

async fn connect_via_proxy(
    proxy: &UpstreamProxy,
    endpoint: &EgressEndpoint,
    checked: SocketAddrV4,
) -> io::Result<TcpStream> {
    let mut stream = timeout(
        SETUP_TIMEOUT,
        TcpStream::connect((proxy.host.as_str(), proxy.port)),
    )
    .await??;
    let target = if proxy.by_name {
        format!("{}:{}", endpoint.host, endpoint.port)
    } else {
        checked.to_string()
    };
    let mut request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
    if let Some(authorization) = &proxy.authorization {
        request.push_str(&format!("Proxy-Authorization: {authorization}\r\n"));
    }
    request.push_str("\r\n");
    timeout(SETUP_TIMEOUT, stream.write_all(request.as_bytes())).await??;
    let mut head = Vec::new();
    timeout(SETUP_TIMEOUT, async {
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if head.len() >= 8192 || stream.read(&mut byte).await? == 0 {
                return Err(invalid("upstream proxy closed before answering CONNECT"));
            }
            head.push(byte[0]);
        }
        Ok(())
    })
    .await??;
    let status = std::str::from_utf8(&head)
        .ok()
        .and_then(|head| head.split_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok());
    match status {
        Some(200) => Ok(stream),
        Some(403 | 407) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "upstream proxy refused the destination",
        )),
        _ => Err(io::Error::other("upstream proxy rejected CONNECT")),
    }
}

#[cfg(test)]
pub(crate) static TEST_DNS: std::sync::Mutex<Option<std::collections::HashMap<String, Vec<Ipv4Addr>>>> =
    std::sync::Mutex::new(None);

fn resolve(endpoint: &EgressEndpoint, egress: &EgressConfig) -> io::Result<Vec<SocketAddrV4>> {
    #[cfg(test)]
    if let Some(candidates) = TEST_DNS
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|table| table.get(&endpoint.host).cloned())
    {
        return checked_addresses(
            candidates,
            &local_addresses()?,
            endpoint.port,
            &endpoint.host,
            egress,
        );
    }
    let host = CString::new(endpoint.host.as_str()).map_err(|_| invalid("invalid DNS hostname"))?;
    let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
    hints.ai_family = libc::AF_INET;
    hints.ai_socktype = libc::SOCK_STREAM;
    hints.ai_protocol = libc::IPPROTO_TCP;
    let mut addresses = std::ptr::null_mut();
    let code =
        unsafe { libc::getaddrinfo(host.as_ptr(), std::ptr::null(), &hints, &mut addresses) };
    if code != 0 {
        return Err(io::Error::other(format!("host DNS failed ({code})")));
    }
    struct Addresses(*mut libc::addrinfo);
    impl Drop for Addresses {
        fn drop(&mut self) {
            unsafe { libc::freeaddrinfo(self.0) };
        }
    }
    let _addresses = Addresses(addresses);
    let mut candidates = Vec::new();
    let mut current = addresses;
    while !current.is_null() {
        if candidates.len() >= MAX_ADDRESSES {
            return Err(invalid("too many DNS addresses"));
        }
        let entry = unsafe { &*current };
        if entry.ai_family != libc::AF_INET
            || entry.ai_addr.is_null()
            || (entry.ai_addrlen as usize) < std::mem::size_of::<libc::sockaddr_in>()
        {
            return Err(invalid("unexpected DNS address family"));
        }
        let address = unsafe { &*(entry.ai_addr.cast::<libc::sockaddr_in>()) };
        candidates.push(Ipv4Addr::from(u32::from_be(address.sin_addr.s_addr)));
        current = entry.ai_next;
    }
    checked_addresses(
        candidates,
        &local_addresses()?,
        endpoint.port,
        &endpoint.host,
        egress,
    )
}

fn local_addresses() -> io::Result<BTreeSet<Ipv4Addr>> {
    let mut interfaces = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut interfaces) } != 0 {
        return Err(io::Error::last_os_error());
    }
    struct Interfaces(*mut libc::ifaddrs);
    impl Drop for Interfaces {
        fn drop(&mut self) {
            unsafe { libc::freeifaddrs(self.0) };
        }
    }
    let _interfaces = Interfaces(interfaces);
    let mut result = BTreeSet::new();
    let mut current = interfaces;
    while !current.is_null() {
        let interface = unsafe { &*current };
        if !interface.ifa_addr.is_null()
            && unsafe { (*interface.ifa_addr).sa_family as i32 } == libc::AF_INET
        {
            let address = unsafe { &*(interface.ifa_addr.cast::<libc::sockaddr_in>()) };
            result.insert(Ipv4Addr::from(u32::from_be(address.sin_addr.s_addr)));
        }
        current = interface.ifa_next;
    }
    Ok(result)
}

fn never_allowed_ipv4(ip: Ipv4Addr) -> bool {
    const DENIED: &[(u32, u32)] = &[
        (0x00000000, 8),
        (0x7f000000, 8),
        (0xa9fe0000, 16),
        (0xc0000000, 24),
        (0xe0000000, 4),
        (0xf0000000, 4),
    ];
    let ip = u32::from(ip);
    DENIED
        .iter()
        .any(|(network, prefix)| ip & (u32::MAX << (32 - prefix)) == *network)
}

fn checked_addresses(
    candidates: Vec<Ipv4Addr>,
    local: &BTreeSet<Ipv4Addr>,
    port: u16,
    host: &str,
    egress: &EgressConfig,
) -> io::Result<Vec<SocketAddrV4>> {
    let permitted = |ip: Ipv4Addr| {
        !local.contains(&ip)
            && !never_allowed_ipv4(ip)
            && (public_ipv4(ip) || egress.private_allowed(host, ip))
    };
    if candidates.is_empty()
        || candidates.len() > MAX_ADDRESSES
        || candidates.iter().any(|ip| !permitted(*ip))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "DNS destination is not public IPv4",
        ));
    }
    Ok(candidates
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|ip| SocketAddrV4::new(ip, port))
        .collect())
}

fn public_ipv4(ip: Ipv4Addr) -> bool {
    const DENIED: &[(u32, u32)] = &[
        (0x00000000, 8),
        (0x0a000000, 8),
        (0x64400000, 10),
        (0x7f000000, 8),
        (0xa9fe0000, 16),
        (0xac100000, 12),
        (0xc0000000, 24),
        (0xc0000200, 24),
        (0xc01fc400, 24),
        (0xc034c100, 24),
        (0xc0586300, 24),
        (0xc0a80000, 16),
        (0xc0af3000, 24),
        (0xc6120000, 15),
        (0xc6336400, 24),
        (0xcb007100, 24),
        (0xe0000000, 4),
        (0xf0000000, 4),
    ];
    let ip = u32::from(ip);
    !DENIED
        .iter()
        .any(|(network, prefix)| ip & (u32::MAX << (32 - prefix)) == *network)
}

async fn copy_active<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    mut reader: R,
    mut writer: W,
    activity: watch::Sender<Instant>,
) -> io::Result<()> {
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let length = reader.read(&mut buffer).await?;
        if length == 0 {
            return writer.shutdown().await;
        }
        activity.send_replace(Instant::now());
        let mut offset = 0;
        while offset < length {
            let written = writer.write(&buffer[offset..length]).await?;
            if written == 0 {
                return Err(io::Error::from(io::ErrorKind::WriteZero));
            }
            offset += written;
            activity.send_replace(Instant::now());
        }
    }
}

#[derive(Clone, Copy)]
enum ClientLifetime {
    HalfClose,
    Web,
}
async fn relay<S>(client: S, remote: TcpStream, lifetime: ClientLifetime) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (client_read, client_write) = tokio::io::split(client);
    let (remote_read, remote_write) = remote.into_split();
    let (activity, mut changes) = watch::channel(Instant::now());
    timeout(TUNNEL_TIMEOUT, async {
        let copying = async {
            let upload = copy_active(client_read, remote_write, activity.clone());
            let download = copy_active(remote_read, client_write, activity);
            match lifetime {
                ClientLifetime::HalfClose => {
                    tokio::try_join!(upload, download)?;
                    Ok(())
                }
                ClientLifetime::Web => tokio::select! {
                    result = upload => result,
                    result = download => result,
                },
            }
        };
        let idle = async {
            loop {
                let expires = *changes.borrow_and_update() + IDLE_TIMEOUT;
                tokio::select! {
                    _ = tokio::time::sleep_until(expires) => return Err(io::Error::new(io::ErrorKind::TimedOut, "egress tunnel idle")),
                    changed = changes.changed() => { if changed.is_err() { return Ok(()); } }
                }
            }
        };
        tokio::select! { result = copying => result, result = idle => result }
    }).await?
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn sockets() -> (UnixStream, UnixStream, TcpStream, TcpStream) {
        let (client, proxy) = UnixStream::pair().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let remote = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server, _) = listener.accept().await.unwrap();
        (client, proxy, remote, server)
    }

    #[tokio::test]
    async fn aborted_web_client_releases_tunnel_without_waiting_for_remote_fin() {
        let (mut client, proxy, remote, mut server) = sockets().await;
        let task = tokio::spawn(relay(proxy, remote, ClientLifetime::Web));
        client.write_all(b"request").await.unwrap();
        let mut request = [0; 7];
        server.read_exact(&mut request).await.unwrap();
        client.shutdown().await.unwrap();
        timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(server.read(&mut [0]).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn allowlist_tunnels_preserve_half_close_responses() {
        let (mut client, proxy, remote, mut server) = sockets().await;
        let task = tokio::spawn(relay(proxy, remote, ClientLifetime::HalfClose));
        client.write_all(b"request").await.unwrap();
        client.shutdown().await.unwrap();
        let mut request = Vec::new();
        server.read_to_end(&mut request).await.unwrap();
        assert_eq!(request, b"request");
        server
            .write_all(b"response after half-close")
            .await
            .unwrap();
        server.shutdown().await.unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        assert_eq!(response, b"response after half-close");
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn web_remote_eof_drains_all_response_bytes() {
        let (mut client, proxy, remote, mut server) = sockets().await;
        let task = tokio::spawn(relay(proxy, remote, ClientLifetime::Web));
        let payload = vec![42; 256 * 1024];
        let expected = payload.clone();
        let writer = tokio::spawn(async move {
            server.write_all(&payload).await.unwrap();
            server.shutdown().await.unwrap();
        });
        let mut response = Vec::new();
        timeout(Duration::from_secs(2), client.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response, expected);
        writer.await.unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn busy_broker_queues_instead_of_dropping_new_connections() {
        use crate::secret_https::{Authority, Inspection};
        use crate::secrets::{LiveBindings, SecretSession};
        let endpoint = EgressEndpoint::new("test.example", 443).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut authority = Authority::new().unwrap();
        authority.route_test_endpoint(endpoint.clone(), listener.local_addr().unwrap());
        let servers = tokio::spawn(async move {
            let mut clients = Vec::new();
            loop {
                clients.push(listener.accept().await.unwrap().0);
            }
        });
        let store = crate::SecretStore::new();
        let secret = store
            .create(
                "broker-secret",
                crate::SecretValue::new("broker-secret-value").unwrap(),
                HttpConnectPolicy::new([endpoint.clone()]).unwrap(),
            )
            .unwrap();
        let inspection = Inspection::new(
            Arc::new(authority),
            SecretSession::new(
                store,
                LiveBindings::new([("BROKER_TOKEN".to_string(), secret.id)].into()),
            ),
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("broker.sock");
        let mut broker = Broker::bind(
            &path,
            HttpConnectPolicy::new([endpoint]).unwrap(),
            Some(inspection),
            None,
            Arc::default(),
        )
        .await
        .unwrap();
        let mut admitted = Vec::new();
        for _ in 0..MAX_CONNECTIONS {
            let mut client = UnixStream::connect(&path).await.unwrap();
            client
                .write_all(b"CONNECT test.example:443 HTTP/1.1\r\n\r\n")
                .await
                .unwrap();
            let mut response = [0; 39];
            timeout(Duration::from_secs(1), client.read_exact(&mut response))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(&response, b"HTTP/1.1 200 Connection Established\r\n\r\n");
            admitted.push(client);
        }
        let mut queued = UnixStream::connect(&path).await.unwrap();
        queued
            .write_all(b"CONNECT denied.example:443 HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        assert!(
            timeout(Duration::from_millis(50), queued.read(&mut [0]))
                .await
                .is_err()
        );
        drop(admitted.pop());
        let mut reply = String::new();
        timeout(Duration::from_secs(1), queued.read_to_string(&mut reply))
            .await
            .unwrap()
            .unwrap();
        assert!(reply.starts_with("HTTP/1.1 403"));
        broker.shutdown().await.unwrap();
        servers.abort();
    }

    #[test]
    fn policy_is_canonical_and_validated_on_load() {
        let endpoint = EgressEndpoint::new("Registry.NPMJS.org.", 443).unwrap();
        assert_eq!(endpoint.host, "registry.npmjs.org");
        let policy = HttpConnectPolicy::new([endpoint.clone(), endpoint]).unwrap();
        assert_eq!(policy.endpoints.len(), 1);
        assert!(policy.allows(&EgressEndpoint::new("registry.npmjs.org", 443).unwrap()));
        assert!(!policy.allows(&EgressEndpoint::new("evil.registry.npmjs.org", 443).unwrap()));
        assert!(!policy.allows(&EgressEndpoint::new("registry.npmjs.org", 80).unwrap()));
        for host in [
            "",
            "*",
            "*.example.org",
            "127.0.0.1",
            "2130706433",
            "0x08080808",
            "0177.1",
            "0x7f.0.0.1",
            "[::1]",
            "a..b",
            "-a.com",
            "a-.com",
            "a_b.com",
            "é.com",
            "a.com..",
            "a.com/path",
        ] {
            assert!(EgressEndpoint::new(host, 443).is_err(), "{host}");
        }
        assert!(EgressEndpoint::new("example.org", 0).is_err());
        assert!(
            serde_json::from_str::<HttpConnectPolicy>(
                r#"{"endpoints":[{"host":"127.0.0.1","port":443}]}"#
            )
            .is_err()
        );
        assert!(serde_json::from_str::<HttpConnectPolicy>(r#"{"endpoints":[]}"#).is_err());
        let restored: HttpConnectPolicy =
            serde_json::from_str(r#"{"endpoints":[{"host":"Registry.NPMJS.org.","port":443}]}"#)
                .unwrap();
        assert_eq!(restored, policy);

        let wildcard = HttpConnectPolicy::with_domains(
            [EgressEndpoint::new("pypi.org", 443).unwrap()],
            ["GitHub.com.".to_string()],
        )
        .unwrap();
        assert!(wildcard.allows(&EgressEndpoint::new("api.github.com", 443).unwrap()));
        assert!(wildcard.allows(&EgressEndpoint::new("raw.githubusercontent.github.com", 80).unwrap()));
        assert!(!wildcard.allows(&EgressEndpoint::new("github.com", 443).unwrap()));
        assert!(!wildcard.allows(&EgressEndpoint::new("evilgithub.com", 443).unwrap()));
        assert!(!wildcard.allows(&EgressEndpoint::new("api.github.com", 8443).unwrap()));
        assert!(wildcard.allows(&EgressEndpoint::new("pypi.org", 443).unwrap()));
        let serialized = serde_json::to_string(&wildcard).unwrap();
        assert!(serialized.contains(r#""domains":["github.com"]"#));
        assert_eq!(serde_json::from_str::<HttpConnectPolicy>(&serialized).unwrap(), wildcard);
        assert!(HttpConnectPolicy::with_domains([], ["*.github.com".to_string()]).is_err());
    }

    #[test]
    fn connect_parser_rejects_ambiguous_and_body_requests() {
        assert_eq!(
            parse_headers(b"CONNECT Example.org:443 HTTP/1.1\r\nHost: example.org:443\r\n\r\n")
                .unwrap(),
            EgressEndpoint::new("example.org", 443).unwrap()
        );
        for request in [
            "GET https://example.org HTTP/1.1\r\n\r\n",
            "CONNECT example.org:443 HTTP/1.0\r\n\r\n",
            "CONNECT example.org:443 HTTP/1.1\r\nHost: other.org:443\r\n\r\n",
            "CONNECT example.org:443 HTTP/1.1\r\nHost: example.org:443\r\nHost: example.org:443\r\n\r\n",
            "CONNECT example.org:443 HTTP/1.1\r\nContent-Length: 0\r\n\r\n",
            "CONNECT example.org:443 HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n",
            "CONNECT example.org:443 HTTP/1.1\r\n Fold: value\r\n\r\n",
            "CONNECT example.org:443 HTTP/1.1\r\nBad : value\r\n\r\n",
            "CONNECT 127.0.0.1:443 HTTP/1.1\r\n\r\n",
            "CONNECT example.org:443@other.org HTTP/1.1\r\n\r\n",
            "CONNECT example.org:443 HTTP/1.1\nX: y\r\n\r\n",
        ] {
            assert!(parse_headers(request.as_bytes()).is_err(), "{request}");
        }
    }

    #[test]
    fn special_and_local_addresses_fail_before_any_dial() {
        for address in [
            "0.0.0.0",
            "10.255.255.255",
            "100.64.0.0",
            "100.127.255.255",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.0",
            "172.31.255.255",
            "192.0.0.9",
            "192.0.2.1",
            "192.31.196.1",
            "192.52.193.1",
            "192.88.99.1",
            "192.168.1.1",
            "192.175.48.1",
            "198.18.0.0",
            "198.19.255.255",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "255.255.255.255",
        ] {
            assert!(!public_ipv4(address.parse().unwrap()), "{address}");
        }
        let public: Ipv4Addr = "93.184.216.34".parse().unwrap();
        assert!(public_ipv4(public));
        assert!(
            checked_addresses(
                vec![public, Ipv4Addr::LOCALHOST],
                &BTreeSet::new(),
                443,
                "example.com",
                &EgressConfig::default()
            )
            .is_err()
        );
        let direct = EgressConfig::default();
        let check = |candidates: Vec<Ipv4Addr>, local: BTreeSet<Ipv4Addr>| {
            checked_addresses(candidates, &local, 443, "example.com", &direct)
        };
        assert!(check(vec![public], BTreeSet::from([public])).is_err());
        assert_eq!(
            check(vec![public, public], BTreeSet::new()).unwrap(),
            vec![SocketAddrV4::new(public, 443)]
        );
        assert!(check(vec![public; 65], BTreeSet::new()).is_err());
        assert!(check(vec![], BTreeSet::new()).is_err());
    }

    #[tokio::test]
    async fn broker_denies_unlisted_and_private_destinations() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("egress.sock");
        let policy =
            HttpConnectPolicy::new([EgressEndpoint::new("localhost", 443).unwrap()]).unwrap();
        let mut broker = Broker::bind(&path, policy, None, None, Arc::default()).await.unwrap();
        for (request, expected) in [
            (
                "CONNECT denied.invalid:443 HTTP/1.1\r\n\r\n",
                "HTTP/1.1 403",
            ),
            ("CONNECT localhost:443 HTTP/1.1\r\n\r\n", "HTTP/1.1 403"),
            ("GET / HTTP/1.1\r\n\r\n", "HTTP/1.1 405"),
        ] {
            let mut client = UnixStream::connect(&path).await.unwrap();
            client.write_all(request.as_bytes()).await.unwrap();
            let mut reply = String::new();
            timeout(Duration::from_secs(6), client.read_to_string(&mut reply))
                .await
                .unwrap()
                .unwrap();
            assert!(reply.starts_with(expected), "{reply}");
        }
        broker.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn broker_records_denied_and_tunnelled_connections_with_byte_counts() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 5];
            socket.read_exact(&mut request).await.unwrap();
            assert_eq!(&request, b"hello");
            socket.write_all(b"world!!").await.unwrap();
        });
        let records = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink: AccessSink = {
            let records = records.clone();
            Arc::new(move |record| records.lock().unwrap().push(record))
        };
        let endpoint = EgressEndpoint::new("tunnel.example", address.port()).unwrap();
        let other = EgressEndpoint::new("other.example", 443).unwrap();
        let mut authority = crate::secret_https::Authority::new().unwrap();
        authority.route_test_endpoint(endpoint.clone(), address);
        let store = crate::SecretStore::new();
        let secret = store
            .create(
                "other-secret",
                crate::SecretValue::new("other-secret-value").unwrap(),
                HttpConnectPolicy::new([other.clone()]).unwrap(),
            )
            .unwrap();
        let inspection = crate::secret_https::Inspection::new(
            Arc::new(authority),
            crate::secrets::SecretSession::new(
                store,
                crate::secrets::LiveBindings::new([("OTHER".to_string(), secret.id)].into()),
            ),
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("egress.sock");
        let policy = HttpConnectPolicy::new([endpoint, other]).unwrap();
        let mut broker = Broker::bind(&path, policy, Some(inspection), Some(sink), Arc::default())
            .await
            .unwrap();

        let mut denied = UnixStream::connect(&path).await.unwrap();
        denied
            .write_all(b"CONNECT denied.invalid:443 HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let mut reply = String::new();
        denied.read_to_string(&mut reply).await.unwrap();
        assert!(reply.starts_with("HTTP/1.1 403"));

        let mut client = UnixStream::connect(&path).await.unwrap();
        client
            .write_all(format!("CONNECT tunnel.example:{} HTTP/1.1\r\n\r\nhello", address.port()).as_bytes())
            .await
            .unwrap();
        let mut established = [0u8; 39];
        client.read_exact(&mut established).await.unwrap();
        assert!(established.starts_with(b"HTTP/1.1 200"));
        let mut body = Vec::new();
        client.read_to_end(&mut body).await.unwrap();
        assert_eq!(body, b"world!!");
        drop(client);
        upstream.await.unwrap();
        timeout(Duration::from_secs(5), async {
            while records.lock().unwrap().len() < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        broker.shutdown().await.unwrap();

        let records = records.lock().unwrap();
        assert_eq!(records[0].host, "denied.invalid");
        assert!(!records[0].allowed);
        assert_eq!((records[0].bytes_in, records[0].bytes_out), (0, 0));
        assert_eq!(records[1].host, "tunnel.example");
        assert!(records[1].allowed);
        assert_eq!(records[1].bytes_out, 5);
        assert_eq!(records[1].bytes_in, 7);
        assert!(records[1].completed >= records[1].started);
    }

    #[test]
    fn egress_config_parses_allowlists_and_proxy_urls() {
        let config = EgressConfig::parse(
            "gitlab.corp.example, 10.20.0.0/16,192.168.7.9",
            Some("http://user:p%40ss@proxy.corp.example:8080"),
            ".corp.example, intranet.local",
        )
        .unwrap();
        assert_eq!(
            config.private_allow,
            [
                PrivateAllow::Host("gitlab.corp.example".into()),
                PrivateAllow::Cidr("10.20.0.0".parse().unwrap(), 16),
                PrivateAllow::Cidr("192.168.7.9".parse().unwrap(), 32),
            ]
        );
        let proxy = config.upstream_proxy.as_ref().unwrap();
        assert_eq!((proxy.host.as_str(), proxy.port), ("proxy.corp.example", 8080));
        assert_eq!(
            proxy.authorization.as_deref(),
            Some("Basic dXNlcjpwJTQwc3M=")
        );
        assert_eq!(config.no_proxy, ["corp.example", "intranet.local"]);
        assert!(EgressConfig::parse("10.0.0.0/33", None, "").is_err());
        assert!(EgressConfig::parse("", Some("https://proxy"), "").is_err());
        assert_eq!(
            UpstreamProxy::parse("http://proxy").unwrap().port,
            3128
        );

        assert!(config.proxy_for("pypi.org").is_some());
        assert!(config.proxy_for("jira.corp.example").is_none());
        assert!(config.proxy_for("GITLAB.corp.example").is_none());
        assert!(config.proxy_for("intranet.local").is_none());
        assert!(config.proxy_for("notintranet.local").is_some());
        assert!(EgressConfig::default().proxy_for("pypi.org").is_none());
    }

    #[test]
    fn private_allowlist_opens_intranet_ranges_but_never_the_host_itself() {
        let config = EgressConfig::parse("gitlab.corp.example,10.20.0.0/16", None, "").unwrap();
        let check = |host: &str, ip: &str| {
            checked_addresses(
                vec![ip.parse().unwrap()],
                &BTreeSet::from(["10.20.0.1".parse().unwrap()]),
                443,
                host,
                &config,
            )
            .is_ok()
        };
        assert!(check("gitlab.corp.example", "192.168.5.5"));
        assert!(check("anything.example", "10.20.9.9"));
        assert!(!check("anything.example", "10.21.0.1"));
        assert!(!check("anything.example", "192.168.5.5"));
        assert!(!check("gitlab.corp.example", "127.0.0.1"));
        assert!(!check("gitlab.corp.example", "169.254.169.254"));
        assert!(!check("anything.example", "10.20.0.1"));
        assert!(check("anything.example", "93.184.216.34"));
        assert!(!check("anything.example", "0.0.0.0"));
    }

    #[tokio::test]
    async fn upstream_proxy_receives_connect_with_credentials_and_relays() {
        let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_address = target.local_addr().unwrap();
        let upstream = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut socket, _) = target.accept().await.unwrap();
                let mut hello = [0u8; 5];
                socket.read_exact(&mut hello).await.unwrap();
                assert_eq!(&hello, b"hello");
                socket.write_all(b"world").await.unwrap();
            }
        });
        let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_address = proxy.local_addr().unwrap();
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let proxy_task = {
            let seen = seen.clone();
            tokio::spawn(async move {
                loop {
                    let (mut client, _) = proxy.accept().await.unwrap();
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        client.read_exact(&mut byte).await.unwrap();
                        head.push(byte[0]);
                    }
                    seen.lock().unwrap().push(String::from_utf8(head).unwrap());
                    let remote = TcpStream::connect(target_address).await.unwrap();
                    client
                        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                        .await
                        .unwrap();
                    tokio::spawn(async move {
                        let (mut client_read, mut client_write) = client.into_split();
                        let (mut remote_read, mut remote_write) = remote.into_split();
                        let _ = tokio::join!(
                            tokio::io::copy(&mut client_read, &mut remote_write),
                            tokio::io::copy(&mut remote_read, &mut client_write)
                        );
                    });
                }
            })
        };
        let config = EgressConfig::parse(
            "",
            Some(&format!("http://agent:secret@127.0.0.1:{}", proxy_address.port())),
            "direct.example",
        )
        .unwrap();
        let public: Ipv4Addr = "93.184.216.34".parse().unwrap();
        *TEST_DNS.lock().unwrap() = Some(std::collections::HashMap::from([
            ("intranet.example".to_string(), vec![public]),
            ("rebound.example".to_string(), vec![Ipv4Addr::new(10, 9, 8, 7)]),
        ]));
        let rebound = connect(&EgressEndpoint::new("rebound.example", 443).unwrap(), &config)
            .await
            .unwrap_err();
        assert_eq!(rebound.kind(), io::ErrorKind::PermissionDenied);
        let unresolvable = connect(
            &EgressEndpoint::new("does-not-resolve.invalid", 443).unwrap(),
            &config,
        )
        .await
        .unwrap_err();
        assert_ne!(unresolvable.kind(), io::ErrorKind::PermissionDenied);
        for internal in ["localhost", "git", "db.internal", "printer.local"] {
            let error = connect(&EgressEndpoint::new(internal, 443).unwrap(), &config)
                .await
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{internal}");
        }
        let endpoint = EgressEndpoint::new("intranet.example", 8443).unwrap();
        let mut stream = connect(&endpoint, &config).await.unwrap();
        stream.write_all(b"hello").await.unwrap();
        let mut reply = [0u8; 5];
        stream.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply, b"world");
        drop(stream);
        let mut by_name = config.clone();
        by_name.upstream_proxy = by_name.upstream_proxy.map(|proxy| proxy.connect_by_name(true));
        let mut named = connect(&endpoint, &by_name).await.unwrap();
        named.write_all(b"hello").await.unwrap();
        named.read_exact(&mut reply).await.unwrap();
        drop(named);
        upstream.await.unwrap();
        proxy_task.abort();
        let heads = seen.lock().unwrap().clone();
        assert!(heads[0].starts_with("CONNECT 93.184.216.34:8443 HTTP/1.1\r\n"), "{}", heads[0]);
        assert!(heads[0].contains("Host: 93.184.216.34:8443\r\n"));
        assert!(heads[0].contains("Proxy-Authorization: Basic YWdlbnQ6c2VjcmV0\r\n"));
        assert!(heads[1].starts_with("CONNECT intranet.example:8443 HTTP/1.1\r\n"), "{}", heads[1]);

        let refused = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let refused_address = refused.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut client, _) = refused.accept().await.unwrap();
            let mut buffer = [0u8; 1024];
            let _ = client.read(&mut buffer).await;
            let _ = client
                .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
                .await;
        });
        let refusing = EgressConfig::parse(
            "",
            Some(&format!("http://127.0.0.1:{}", refused_address.port())),
            "",
        )
        .unwrap();
        let error = connect(&endpoint, &refusing).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        *TEST_DNS.lock().unwrap() = None;
    }

    #[tokio::test]
    async fn parser_preserves_tunnel_prefix_and_broker_joins_pending_clients() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        client
            .write_all(b"CONNECT example.org:443 HTTP/1.1\r\n\r\nprefix")
            .await
            .unwrap();
        let (_, pending) = read_request(&mut server).await.unwrap();
        assert_eq!(pending, b"prefix");
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("egress.sock");
        let policy =
            HttpConnectPolicy::new([EgressEndpoint::new("example.org", 443).unwrap()]).unwrap();
        let mut broker = Broker::bind(&path, policy, None, None, Arc::default()).await.unwrap();
        let mut client = UnixStream::connect(&path).await.unwrap();
        client.write_all(b"CONNECT ").await.unwrap();
        timeout(Duration::from_secs(1), broker.shutdown())
            .await
            .unwrap()
            .unwrap();
        broker.shutdown().await.unwrap();
        let mut byte = [0];
        assert!(matches!(client.read(&mut byte).await, Ok(0) | Err(_)));
    }
}
