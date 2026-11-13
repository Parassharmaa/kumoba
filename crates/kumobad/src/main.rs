mod access_log;
mod catalog;
mod http;
mod model;
mod preview;
mod service;
mod terminal;

use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use kumoba::{AccessSink, Runtime, RuntimeConfig};
use service::{Service, ServiceConfig};
use std::{
    collections::BTreeMap,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::net::{TcpListener, UnixListener};

const DEFAULT_LISTEN: &str = "127.0.0.1:7788";
const DEFAULT_PREVIEW_LISTEN: &str = "127.0.0.1:7789";

struct Options {
    egress: kumoba::EgressConfig,
    state_dir: PathBuf,
    worker: PathBuf,
    images: BTreeMap<String, PathBuf>,
    socket: Option<PathBuf>,
    listen: Option<String>,
    token: Vec<u8>,
    token_file: PathBuf,
    listen_is_default: bool,
    preview_listen: Option<String>,
    preview_url: Option<String>,
    max_running: usize,
}

fn home() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("KUMOBA_HOME") {
        return Ok(PathBuf::from(path));
    }
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".kumoba"))
        .ok_or_else(|| "set KUMOBA_HOME or HOME".to_string())
}

fn env_path_or(name: &str, default: impl FnOnce() -> PathBuf) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(default)
}

fn parse_images(value: &str) -> Result<BTreeMap<String, PathBuf>, String> {
    value
        .split(',')
        .filter(|pair| !pair.trim().is_empty())
        .map(|pair| {
            pair.split_once('=')
                .map(|(name, path)| (name.trim().to_string(), PathBuf::from(path.trim())))
                .ok_or_else(|| format!("invalid image mapping {pair}"))
        })
        .collect()
}

fn discover_images(dir: &Path) -> Result<BTreeMap<String, PathBuf>, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(format!("cannot read {}: {error}", dir.display())),
    };
    let mut images = BTreeMap::new();
    for entry in entries {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path.extension().is_some_and(|extension| extension == "raw")
            && let Some(name) = path.file_stem().and_then(|stem| stem.to_str())
        {
            images.insert(name.to_string(), path);
        }
    }
    Ok(images)
}

fn read_token(path: &Path, create: bool) -> Result<Vec<u8>, String> {
    if create && !path.exists() {
        let mut random = [0u8; 32];
        std::io::Read::read_exact(
            &mut std::fs::File::open("/dev/urandom").map_err(|error| error.to_string())?,
            &mut random,
        )
        .map_err(|error| error.to_string())?;
        let token: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
        std::io::Write::write_all(&mut file, token.as_bytes())
            .map_err(|error| error.to_string())?;
    }
    let token = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read token file {}: {error}", path.display()))?
        .trim()
        .as_bytes()
        .to_vec();
    if token.len() < 32 {
        return Err("kumobad token must be at least 32 bytes".into());
    }
    Ok(token)
}

fn default_worker() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("kumoba-worker")))
        .unwrap_or_else(|| PathBuf::from("kumoba-worker"))
}

fn loopback(address: &str, variable: &str) -> Result<String, String> {
    let parsed: std::net::SocketAddr = address
        .parse()
        .map_err(|_| format!("invalid {variable} {address}"))?;
    if !parsed.ip().is_loopback() {
        return Err(format!("{variable} must be a loopback address"));
    }
    Ok(address.to_string())
}

fn options() -> Result<Options, String> {
    let home = home()?;
    let images = match std::env::var("KUMOBAD_IMAGES") {
        Ok(value) => parse_images(&value)?,
        Err(_) => discover_images(&home.join("images"))?,
    };
    if images.is_empty() {
        return Err(format!(
            "no images: put a disk image at {}/<name>.raw (scripts/install.sh downloads one) or set KUMOBAD_IMAGES=name=/path.raw",
            home.join("images").display()
        ));
    }
    let token_file = std::env::var_os("KUMOBAD_TOKEN_FILE").map(PathBuf::from);
    let token = match &token_file {
        Some(path) => read_token(path, false)?,
        None => {
            std::fs::create_dir_all(&home)
                .map_err(|error| format!("cannot create {}: {error}", home.display()))?;
            read_token(&home.join("token"), true)?
        }
    };
    let worker = env_path_or("KUMOBAD_WORKER", default_worker);
    if !worker.is_file() {
        return Err(format!(
            "kumoba-worker not found at {}; install it next to kumobad or set KUMOBAD_WORKER",
            worker.display()
        ));
    }
    let listen = std::env::var("KUMOBAD_LISTEN")
        .ok()
        .map(|address| loopback(&address, "KUMOBAD_LISTEN"))
        .transpose()?;
    let mut egress = kumoba::EgressConfig::parse(
        &std::env::var("KUMOBAD_ALLOW_PRIVATE_HOSTS").unwrap_or_default(),
        std::env::var("KUMOBAD_UPSTREAM_PROXY").ok().as_deref(),
        &std::env::var("KUMOBAD_NO_PROXY").unwrap_or_default(),
    )
    .map_err(|error| format!("invalid egress configuration: {error}"))?;
    let connect_by = std::env::var("KUMOBAD_UPSTREAM_PROXY_CONNECT").unwrap_or_default();
    egress.upstream_proxy = match (egress.upstream_proxy, connect_by.as_str()) {
        (proxy, "" | "address") => proxy,
        (proxy, "host") => proxy.map(|proxy| proxy.connect_by_name(true)),
        _ => return Err("KUMOBAD_UPSTREAM_PROXY_CONNECT must be address or host".into()),
    };
    let explicit_socket = std::env::var_os("KUMOBAD_SOCKET").map(PathBuf::from);
    let listen_is_default = listen.is_none() && explicit_socket.is_none();
    let socket = explicit_socket.or_else(|| listen.is_none().then(|| home.join("kumobad.sock")));
    let listen = if listen_is_default {
        Some(DEFAULT_LISTEN.to_string())
    } else {
        listen
    };
    let preview_listen = match std::env::var("KUMOBAD_PREVIEW_LISTEN") {
        Ok(address) => Some(loopback(&address, "KUMOBAD_PREVIEW_LISTEN")?),
        Err(_) => listen_is_default.then(|| DEFAULT_PREVIEW_LISTEN.to_string()),
    };
    Ok(Options {
        egress,
        state_dir: env_path_or("KUMOBAD_STATE_DIR", || home.join("state")),
        worker,
        images,
        socket,
        listen,
        token,
        token_file: token_file.unwrap_or_else(|| home.join("token")),
        listen_is_default,
        preview_listen,
        preview_url: std::env::var("KUMOBAD_PREVIEW_URL").ok(),
        max_running: std::env::var("KUMOBAD_MAX_RUNNING")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(4),
    })
}

fn serve<S>(stream: S, service: Arc<Service>, token: Arc<Vec<u8>>, listener: http::Listener)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let handler = service_fn(move |request| {
            http::handle(request, service.clone(), token.clone(), listener)
        });
        let _ = hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(stream), handler)
            .with_upgrades()
            .await;
    });
}

const USAGE: &str = "\
kumobad: serves microVM sandboxes over HTTP

With no configuration it uses ~/.kumoba (or $KUMOBA_HOME): boots every
images/<name>.raw, keeps sandboxes in state/, creates a bearer token in token
and listens on kumobad.sock. The SDKs find that socket and token by themselves.

Environment, all optional:
  KUMOBA_HOME           base directory for the defaults below (default ~/.kumoba)
  KUMOBAD_IMAGES        name=/path/to/image.raw[,name2=...] (default: $KUMOBA_HOME/images/*.raw)
  KUMOBAD_STATE_DIR     sandbox disks, metadata and keys (default $KUMOBA_HOME/state)
  KUMOBAD_TOKEN_FILE    bearer token, 32 bytes or more (default $KUMOBA_HOME/token, created)
  KUMOBAD_WORKER        kumoba-worker binary (default: next to kumobad)
  KUMOBAD_SOCKET        Unix socket to listen on, mode 0600
                        (default $KUMOBA_HOME/kumobad.sock unless KUMOBAD_LISTEN is set)
  KUMOBAD_LISTEN        loopback host:port to listen on (default 127.0.0.1:7788 when neither
                        it nor KUMOBAD_SOCKET is set; skipped if that port is taken)
  KUMOBAD_PREVIEW_LISTEN  loopback host:port that serves previews to browsers, on an origin
                        apart from the API (default 127.0.0.1:7789 when nothing else is set)
  KUMOBAD_PREVIEW_URL   base URL browsers reach that listener at, for expose and desktop
                        links (default http://<preview listen address>)
  KUMOBAD_MAX_RUNNING   concurrently running sandboxes (default 4)
  KUMOBAD_ALLOW_PRIVATE_HOSTS  intranet destinations sandboxes may reach: host names (with
                        subdomains) and IPv4 CIDRs, comma separated. Loopback, link-local
                        and this host's own addresses stay blocked
  KUMOBAD_UPSTREAM_PROXY  http://[user:pass@]host[:port] corporate proxy to tunnel through
  KUMOBAD_NO_PROXY      host names reached directly instead of through the upstream proxy
  KUMOBAD_UPSTREAM_PROXY_CONNECT  what the proxy CONNECT names: address (the checked IP,
                        default) or host (the name, for proxies with domain rules)
  KUMOBAD_LIBKRUN       libkrun to load (default: ../lib next to kumobad, then the system)
  KUMOBAD_LIBKRUNFW     libkrunfw to load (default: ../lib next to kumobad, then the system)
";

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{USAGE}");
        return;
    }
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("kumobad {}", env!("CARGO_PKG_VERSION"));
        return;
    }
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
    std::fs::create_dir_all(&options.state_dir).map_err(|error| error.to_string())?;
    let image_names: Vec<String> = options.images.keys().cloned().collect();
    let mut runtime_config =
        RuntimeConfig::for_host(&options.worker, options.state_dir.join("runtime"))
            .map_err(|error| error.to_string())?;
    runtime_config.max_running = options.max_running;
    runtime_config.boot_timeout = Duration::from_secs(120);
    let store = kumoba::SecretStore::new();
    runtime_config.secret_store = Some(store.clone());
    runtime_config.egress = Arc::new(options.egress.clone());
    let (access_tx, access_rx) = tokio::sync::mpsc::channel(65_536);
    let dropped = service::DroppedRecords::default();
    let overflow = dropped.clone();
    runtime_config.access_sink = Some(AccessSink::new(move |id, record| {
        if access_tx.try_send((id, record)).is_err() {
            overflow.add(&id.to_string(), 1);
        }
    }));
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
        access_rx,
        dropped,
    )
    .await
    .map_err(|error| error.to_string())?;
    let token = Arc::new(options.token);

    let unix = match &options.socket {
        Some(path) => {
            let _ = std::fs::remove_file(path);
            let listener = UnixListener::bind(path)
                .map_err(|error| format!("cannot listen on {}: {error}", path.display()))?;
            std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
                .map_err(|error| error.to_string())?;
            Some(listener)
        }
        None => None,
    };
    let tcp = match &options.listen {
        Some(address) => match TcpListener::bind(address).await {
            Ok(listener) => Some(listener),
            Err(error) if options.listen_is_default => {
                eprintln!(
                    "{}",
                    serde_json::json!({"type": "kumobad", "event": "listen_skipped", "listen": address, "error": error.to_string()})
                );
                None
            }
            Err(error) => return Err(format!("cannot listen on {address}: {error}")),
        },
        None => None,
    };
    let listening = tcp.as_ref().and_then(|_| options.listen.clone());
    let browser = match &options.preview_listen {
        Some(address) => match TcpListener::bind(address).await {
            Ok(listener) => Some(listener),
            Err(error) if options.listen_is_default => {
                eprintln!(
                    "{}",
                    serde_json::json!({"type": "kumobad", "event": "preview_listen_skipped", "listen": address, "error": error.to_string()})
                );
                None
            }
            Err(error) => return Err(format!("cannot listen on {address}: {error}")),
        },
        None => None,
    };
    let previewing = browser
        .as_ref()
        .and_then(|_| options.preview_listen.clone());
    let preview_url = options.preview_url.clone().or_else(|| {
        previewing
            .as_ref()
            .map(|address| format!("http://{address}"))
    });
    service.set_public_url(preview_url.clone());
    eprintln!(
        "{}",
        serde_json::json!({
            "type": "kumobad",
            "event": "ready",
            "version": env!("CARGO_PKG_VERSION"),
            "socket": options.socket,
            "listen": listening,
            "previewListen": previewing,
            "previewUrl": preview_url,
            "tokenFile": options.token_file,
            "images": image_names,
        })
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
                if let Ok((stream, _)) = accepted { serve(stream, service.clone(), token.clone(), http::Listener::Api); }
            }
            accepted = async { tcp.as_ref().unwrap().accept().await }, if tcp.is_some() => {
                if let Ok((stream, _)) = accepted { serve(stream, service.clone(), token.clone(), http::Listener::Api); }
            }
            accepted = async { browser.as_ref().unwrap().accept().await }, if browser.is_some() => {
                if let Ok((stream, _)) = accepted { serve(stream, service.clone(), token.clone(), http::Listener::Browser); }
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
