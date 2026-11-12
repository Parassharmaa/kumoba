mod http;
mod model;
mod preview;
mod service;

use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use kumoba::{Runtime, RuntimeConfig};
use service::{Service, ServiceConfig};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};
use tokio::net::{TcpListener, UnixListener};

struct Options {
    state_dir: PathBuf,
    worker: PathBuf,
    images: BTreeMap<String, PathBuf>,
    socket: Option<PathBuf>,
    listen: Option<String>,
    token: Vec<u8>,
    max_running: usize,
}

fn env_path(name: &str) -> Result<PathBuf, String> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| format!("{name} is required"))
}

fn options() -> Result<Options, String> {
    let images = std::env::var("KUMOBAD_IMAGES")
        .map_err(|_| "KUMOBAD_IMAGES is required (name=/path.raw,...)".to_string())?
        .split(',')
        .filter(|pair| !pair.trim().is_empty())
        .map(|pair| {
            pair.split_once('=')
                .map(|(name, path)| (name.trim().to_string(), PathBuf::from(path.trim())))
                .ok_or_else(|| format!("invalid image mapping {pair}"))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let token = std::fs::read_to_string(env_path("KUMOBAD_TOKEN_FILE")?)
        .map_err(|error| format!("cannot read KUMOBAD_TOKEN_FILE: {error}"))?
        .trim()
        .as_bytes()
        .to_vec();
    if token.len() < 32 {
        return Err("kumobad token must be at least 32 bytes".into());
    }
    let listen = std::env::var("KUMOBAD_LISTEN").ok();
    if let Some(address) = &listen {
        let parsed: std::net::SocketAddr = address
            .parse()
            .map_err(|_| format!("invalid KUMOBAD_LISTEN {address}"))?;
        if !parsed.ip().is_loopback() {
            return Err("KUMOBAD_LISTEN must be a loopback address".into());
        }
    }
    Ok(Options {
        state_dir: env_path("KUMOBAD_STATE_DIR")?,
        worker: env_path("KUMOBAD_WORKER")?,
        images,
        socket: std::env::var_os("KUMOBAD_SOCKET").map(PathBuf::from),
        listen,
        token,
        max_running: std::env::var("KUMOBAD_MAX_RUNNING")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(4),
    })
}

fn serve<S>(stream: S, service: Arc<Service>, token: Arc<Vec<u8>>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let handler =
            service_fn(move |request| http::handle(request, service.clone(), token.clone()));
        let _ = hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(stream), handler)
            .with_upgrades()
            .await;
    });
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!(
            "{}",
            serde_json::json!({"type": "kumobad", "event": "fatal", "error": error})
        );
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let options = options()?;
    if options.socket.is_none() && options.listen.is_none() {
        return Err("set KUMOBAD_SOCKET or KUMOBAD_LISTEN".into());
    }
    std::fs::create_dir_all(&options.state_dir).map_err(|error| error.to_string())?;
    let mut runtime_config =
        RuntimeConfig::for_host(&options.worker, options.state_dir.join("runtime"))
            .map_err(|error| error.to_string())?;
    runtime_config.max_running = options.max_running;
    runtime_config.boot_timeout = Duration::from_secs(120);
    let store = kumoba::SecretStore::new();
    runtime_config.secret_store = Some(store.clone());
    let runtime = Runtime::open(runtime_config)
        .await
        .map_err(|error| error.to_string())?;
    let service = Service::open(
        runtime,
        store,
        ServiceConfig {
            state_dir: options.state_dir.clone(),
            images: options.images,
        },
    )
    .await
    .map_err(|error| error.to_string())?;
    let token = Arc::new(options.token);

    let unix = match &options.socket {
        Some(path) => {
            let _ = std::fs::remove_file(path);
            let listener = UnixListener::bind(path).map_err(|error| error.to_string())?;
            std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
                .map_err(|error| error.to_string())?;
            Some(listener)
        }
        None => None,
    };
    let tcp = match &options.listen {
        Some(address) => Some(
            TcpListener::bind(address)
                .await
                .map_err(|error| error.to_string())?,
        ),
        None => None,
    };
    eprintln!(
        "{}",
        serde_json::json!({"type": "kumobad", "event": "ready", "socket": options.socket, "listen": options.listen})
    );

    let reaper = service.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            reaper.reap().await;
        }
    });

    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|error| error.to_string())?;
    loop {
        tokio::select! {
            accepted = async { unix.as_ref().unwrap().accept().await }, if unix.is_some() => {
                if let Ok((stream, _)) = accepted { serve(stream, service.clone(), token.clone()); }
            }
            accepted = async { tcp.as_ref().unwrap().accept().await }, if tcp.is_some() => {
                if let Ok((stream, _)) = accepted { serve(stream, service.clone(), token.clone()); }
            }
            _ = tokio::signal::ctrl_c() => break,
            _ = terminate.recv() => break,
        }
    }
    service.stop_all().await;
    if let Some(path) = options.socket {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}
