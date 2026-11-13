use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, body::Incoming};
use hyper_util::rt::TokioIo;
use std::path::PathBuf;

pub enum Failure {
    Usage(String),
    Unreachable(String),
    Api(String),
}

impl Failure {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Api(_) => 1,
            Self::Usage(_) => 2,
            Self::Unreachable(_) => 3,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Usage(message) | Self::Unreachable(message) | Self::Api(message) => message,
        }
    }
}

enum Endpoint {
    Unix(PathBuf),
    Tcp(String),
}

pub struct Connection {
    endpoint: Endpoint,
    token: String,
    pub display: String,
}

fn home() -> PathBuf {
    if let Some(home) = std::env::var_os("KUMOBA_HOME") {
        return PathBuf::from(home);
    }
    std::env::var_os("HOME").map_or_else(
        || PathBuf::from(".kumoba"),
        |home| PathBuf::from(home).join(".kumoba"),
    )
}

impl Connection {
    pub fn from_env() -> Result<Self, Failure> {
        let home = home();
        let url = std::env::var("KUMOBA_URL")
            .unwrap_or_else(|_| format!("unix://{}", home.join("kumobad.sock").display()));
        let endpoint = if let Some(path) = url.strip_prefix("unix://") {
            Endpoint::Unix(PathBuf::from(path))
        } else if let Some(address) = url.strip_prefix("http://") {
            Endpoint::Tcp(address.trim_end_matches('/').to_string())
        } else {
            return Err(Failure::Usage(format!(
                "KUMOBA_URL must be unix:///path/to/kumobad.sock or http://127.0.0.1:port, got {url}"
            )));
        };
        let token_file =
            std::env::var_os("KUMOBA_TOKEN_FILE").map_or_else(|| home.join("token"), PathBuf::from);
        let token = match std::env::var("KUMOBA_TOKEN") {
            Ok(token) if !token.is_empty() => token,
            _ => std::fs::read_to_string(&token_file)
                .map(|token| token.trim().to_string())
                .map_err(|_| {
                    Failure::Unreachable(format!(
                        "No kumobad token at {}: start kumobad once so it writes one, or set KUMOBA_TOKEN or KUMOBA_TOKEN_FILE",
                        token_file.display()
                    ))
                })?,
        };
        Ok(Self {
            display: url,
            endpoint,
            token,
        })
    }

    pub async fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<(&'static str, Vec<u8>)>,
    ) -> Result<Response<Incoming>, Failure> {
        let unreachable = |error: std::io::Error| {
            Failure::Unreachable(format!(
                "Cannot reach kumobad at {}: {error}. Start it with `kumobad`, or point KUMOBA_URL at a running daemon",
                self.display
            ))
        };
        let (host, mut sender) = match &self.endpoint {
            Endpoint::Unix(path) => {
                let stream = tokio::net::UnixStream::connect(path)
                    .await
                    .map_err(unreachable)?;
                ("kumobad".to_string(), handshake(stream).await?)
            }
            Endpoint::Tcp(address) => {
                let stream = tokio::net::TcpStream::connect(address)
                    .await
                    .map_err(unreachable)?;
                (address.clone(), handshake(stream).await?)
            }
        };
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", host)
            .header("authorization", format!("Bearer {}", self.token));
        let payload = match body {
            Some((content_type, bytes)) => {
                request = request.header("content-type", content_type);
                Bytes::from(bytes)
            }
            None => Bytes::new(),
        };
        let request = request
            .body(Full::new(payload))
            .map_err(|error| Failure::Usage(error.to_string()))?;
        let response = sender
            .send_request(request)
            .await
            .map_err(|error| Failure::Api(format!("kumobad closed the connection: {error}")))?;
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status().as_u16();
        let bytes = response
            .into_body()
            .collect()
            .await
            .map(http_body_util::Collected::to_bytes)
            .unwrap_or_default();
        let message = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|value| {
                value
                    .get("message")
                    .or_else(|| value.get("error"))
                    .and_then(|message| message.as_str().map(str::to_string))
            })
            .unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned());
        Err(Failure::Api(format!("{message} (HTTP {status})")))
    }
}

async fn handshake<S>(
    stream: S,
) -> Result<hyper::client::conn::http1::SendRequest<Full<Bytes>>, Failure>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|error| Failure::Unreachable(format!("kumobad handshake failed: {error}")))?;
    tokio::spawn(connection.with_upgrades());
    Ok(sender)
}

impl Connection {
    pub async fn upgrade(
        &self,
        path: &str,
        body: Vec<u8>,
        protocol: &str,
    ) -> Result<hyper::upgrade::Upgraded, Failure> {
        let unreachable = |error: std::io::Error| {
            Failure::Unreachable(format!(
                "Cannot reach kumobad at {}: {error}. Start it with `kumobad`, or point KUMOBA_URL at a running daemon",
                self.display
            ))
        };
        let (host, mut sender) = match &self.endpoint {
            Endpoint::Unix(socket) => {
                let stream = tokio::net::UnixStream::connect(socket)
                    .await
                    .map_err(unreachable)?;
                ("kumobad".to_string(), handshake(stream).await?)
            }
            Endpoint::Tcp(address) => {
                let stream = tokio::net::TcpStream::connect(address)
                    .await
                    .map_err(unreachable)?;
                (address.clone(), handshake(stream).await?)
            }
        };
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header("host", host)
            .header("authorization", format!("Bearer {}", self.token))
            .header("content-type", "application/json")
            .header("connection", "upgrade")
            .header("upgrade", protocol)
            .body(Full::new(Bytes::from(body)))
            .map_err(|error| Failure::Usage(error.to_string()))?;
        let response = sender
            .send_request(request)
            .await
            .map_err(|error| Failure::Api(format!("kumobad closed the connection: {error}")))?;
        if response.status() == hyper::StatusCode::SWITCHING_PROTOCOLS {
            return hyper::upgrade::on(response)
                .await
                .map_err(|error| Failure::Api(format!("the shell connection failed: {error}")));
        }
        let status = response.status().as_u16();
        let bytes = read_all(response).await.unwrap_or_default();
        let message = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|value| {
                value
                    .get("message")
                    .and_then(|m| m.as_str().map(str::to_string))
            })
            .unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned());
        Err(Failure::Api(format!("{message} (HTTP {status})")))
    }
}

pub async fn read_all(response: Response<Incoming>) -> Result<Bytes, Failure> {
    response
        .into_body()
        .collect()
        .await
        .map(http_body_util::Collected::to_bytes)
        .map_err(|error| Failure::Api(format!("reading the kumobad response failed: {error}")))
}
