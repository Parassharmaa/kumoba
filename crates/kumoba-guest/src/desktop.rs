use base64::{Engine, engine::general_purpose::STANDARD};
use kumoba_protocol::{
    Response,
    desktop::{
        Action, CDP_PORT, DesktopRequest, MAX_SCREENSHOT, VNC_PORT, valid_action, valid_size,
    },
};
use std::os::unix::fs::PermissionsExt;
use std::{io, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UnixStream},
    process::Command,
    time::{Instant, timeout},
};

const DISPLAY: &str = ":99";
const ROOT: &str = "/run/kumoba-desktop";
fn failed(message: &str) -> io::Error {
    io::Error::other(message)
}
fn chromium() -> Option<&'static str> {
    ["/usr/bin/chromium", "/usr/bin/chromium-browser"]
        .into_iter()
        .find(|path| Path::new(path).is_file())
}
async fn browser_ready() -> bool {
    matches!(
        timeout(Duration::from_millis(400), async {
            let mut socket = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, CDP_PORT)).await?;
            socket
                .write_all(
                    b"GET /json/version HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
                )
                .await?;
            let mut response = Vec::new();
            while response.len() < 8192 {
                let mut block = [0; 1024];
                let count = socket.read(&mut block).await?;
                if count == 0 {
                    break;
                }
                response.extend_from_slice(&block[..count]);
                if String::from_utf8_lossy(&response).contains("webSocketDebuggerUrl") {
                    return Ok::<_, io::Error>(true);
                }
            }
            Ok(false)
        })
        .await,
        Ok(Ok(true))
    )
}
pub(crate) fn available() -> bool {
    chromium().is_some()
        && (Path::new("/usr/bin/xfce4-session").is_file()
            || Path::new("/usr/bin/openbox").is_file())
        && [
            "/usr/bin/Xvfb",
            "/usr/bin/xdotool",
            "/usr/bin/scrot",
            "/usr/bin/certutil",
            "/usr/bin/x11vnc",
        ]
        .iter()
        .all(|path| Path::new(path).is_file())
}
pub(crate) fn reset() -> io::Result<()> {
    match std::fs::remove_dir_all(ROOT) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::fs::create_dir_all(ROOT)
}
struct Process {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    exited: tokio::sync::watch::Receiver<bool>,
}
impl Process {
    fn spawn(mut command: Command) -> io::Result<Self> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        let group = crate::linux::pid_of(&child)?;
        let (stop, receive_stop) = tokio::sync::oneshot::channel();
        let (exited, receiver) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = child.wait() => {},
                _ = receive_stop => {
                    if matches!(child.try_wait(), Ok(None)) {
                        unsafe { libc::kill(-group, libc::SIGKILL); }
                    }
                    let _ = child.wait().await;
                }
            }
            let _ = exited.send(true);
        });
        Ok(Self {
            stop: Some(stop),
            exited: receiver,
        })
    }
    fn alive(&mut self) -> bool {
        !*self.exited.borrow() && self.exited.has_changed().is_ok()
    }
    async fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        while !*self.exited.borrow() {
            if self.exited.changed().await.is_err() {
                break;
            }
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}
struct Session {
    width: u16,
    height: u16,
    processes: Vec<Process>,
    browser: Option<Process>,
    directory: tempfile::TempDir,
}
impl Session {
    fn home(&self) -> std::path::PathBuf {
        if Path::new("/usr/bin/xfce4-session").is_file() {
            "/workspace/home".into()
        } else {
            self.directory.path().to_path_buf()
        }
    }
    fn environment(&self) -> std::collections::BTreeMap<String, String> {
        let mut env = std::collections::BTreeMap::from([
            (
                "PATH".into(),
                "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".into(),
            ),
            ("DISPLAY".into(), DISPLAY.into()),
            (
                "KUMOBA_BROWSER_HOME".into(),
                self.directory.path().display().to_string(),
            ),
            (
                "XDG_CACHE_HOME".into(),
                self.directory.path().join("cache").display().to_string(),
            ),
            ("HOME".into(), self.home().display().to_string()),
            (
                "XDG_RUNTIME_DIR".into(),
                self.directory.path().join("runtime").display().to_string(),
            ),
            ("KUMOBA_DESKTOP_WIDTH".into(), self.width.to_string()),
            ("KUMOBA_DESKTOP_HEIGHT".into(), self.height.to_string()),
            (
                "KUMOBA_EGRESS".into(),
                std::env::var("KUMOBA_EGRESS").unwrap_or_else(|_| "0".into()),
            ),
            ("LANG".into(), "C.UTF-8".into()),
        ]);
        if Path::new("/usr/bin/xfce4-session").is_file() {
            env.extend([
                (
                    "DBUS_SESSION_BUS_ADDRESS".into(),
                    format!("unix:path={}/runtime/bus", self.directory.path().display()),
                ),
                ("XDG_CURRENT_DESKTOP".into(), "XFCE".into()),
                ("DESKTOP_SESSION".into(), "xfce".into()),
                ("XDG_CONFIG_DIRS".into(), "/etc/xdg".into()),
                ("XDG_DATA_DIRS".into(), "/usr/local/share:/usr/share".into()),
            ]);
        }
        env
    }
    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command.env_clear().envs(self.environment());
        command
    }
    async fn tool(&self, program: &str, args: &[String]) -> io::Result<()> {
        let mut command = self.command(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let status = timeout(Duration::from_secs(5), command.status())
            .await
            .map_err(|_| failed("desktop tool timed out"))??;
        if !status.success() {
            return Err(failed("desktop tool failed"));
        }
        Ok(())
    }
    fn info(&self) -> Response {
        Response::Desktop {
            width: self.width,
            height: self.height,
            cdp_port: CDP_PORT,
            vnc_port: Some(VNC_PORT),
        }
    }
    fn alive(&mut self) -> bool {
        if self
            .browser
            .as_mut()
            .is_some_and(|browser| !browser.alive())
        {
            self.browser.take();
        }
        self.processes.iter_mut().all(Process::alive)
    }
    async fn stop(&mut self) -> io::Result<()> {
        if let Some(mut browser) = self.browser.take() {
            browser.stop().await;
        }
        let browser_cleanup = crate::shutdown::Scope::browser_session(self.directory.path())
            .quiesce()
            .await;
        for process in self.processes.iter_mut().rev() {
            process.stop().await;
        }
        self.processes.clear();
        for path in ["/tmp/.X99-lock", "/tmp/.X11-unix/X99"] {
            let _ = std::fs::remove_file(path);
        }
        browser_cleanup
    }
    async fn start(width: u16, height: u16, startup_timeout_ms: u64) -> io::Result<Self> {
        if !kumoba_protocol::desktop::valid_startup_timeout_ms(startup_timeout_ms) {
            return Err(failed("desktop startup timeout must be 1..180 seconds"));
        }
        if !available() {
            return Err(failed("image lacks browser/desktop tools"));
        }
        if !valid_size(width, height) {
            return Err(failed("invalid desktop dimensions"));
        }
        if UnixStream::connect("/tmp/.X11-unix/X99").await.is_ok()
            || TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, CDP_PORT))
                .await
                .is_ok()
            || TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, VNC_PORT))
                .await
                .is_ok()
        {
            return Err(failed(
                "reserved desktop display or CDP port already in use",
            ));
        }
        for path in ["/tmp/.X99-lock", "/tmp/.X11-unix/X99"] {
            let _ = std::fs::remove_file(path);
        }
        let mut session = Self {
            width,
            height,
            processes: Vec::new(),
            browser: None,
            directory: tempfile::Builder::new()
                .prefix("session-")
                .permissions(std::fs::Permissions::from_mode(0o700))
                .tempdir_in(ROOT)?,
        };
        std::fs::create_dir_all(session.home())?;
        std::fs::set_permissions(session.home(), std::fs::Permissions::from_mode(0o700))?;
        let runtime_dir = session.directory.path().join("runtime");
        std::fs::create_dir(&runtime_dir)?;
        std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700))?;
        let mut x = session.command("/usr/bin/Xvfb");
        x.args([
            DISPLAY,
            "-screen",
            "0",
            &format!("{width}x{height}x24"),
            "-nolisten",
            "tcp",
            "-ac",
        ]);
        session.processes.push(Process::spawn(x)?);
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if !session.alive() {
                return Err(failed("Xvfb exited during startup"));
            }
            if UnixStream::connect("/tmp/.X11-unix/X99").await.is_ok() {
                break;
            }
            if Instant::now() > deadline {
                return Err(failed("Xvfb readiness timeout"));
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        if Path::new("/usr/bin/xfce4-session").is_file() {
            session.start_xfce().await?;
        } else {
            let wm = session.command("/usr/bin/openbox");
            session.processes.push(Process::spawn(wm)?);
        }
        let mut vnc = session.command("/usr/bin/x11vnc");
        vnc.args([
            "-display",
            DISPLAY,
            "-rfbport",
            "5900",
            "-listen",
            "127.0.0.1",
            "-noipv6",
            "-forever",
            "-shared",
            "-nopw",
            "-noclipboard",
            "-noremote",
            "-nosel",
            "-quiet",
        ]);
        session.processes.push(Process::spawn(vnc)?);
        if Path::new("/tmp/kumoba-ca.pem").is_file() {
            let nss = session.directory.path().join(".pki/nssdb");
            std::fs::create_dir_all(&nss)?;
            let db = format!("sql:{}", nss.display());
            session
                .tool(
                    "/usr/bin/certutil",
                    &[
                        "-N".into(),
                        "--empty-password".into(),
                        "-d".into(),
                        db.clone(),
                    ],
                )
                .await?;
            session
                .tool(
                    "/usr/bin/certutil",
                    &[
                        "-A".into(),
                        "-d".into(),
                        db,
                        "-n".into(),
                        "Kumoba runtime".into(),
                        "-t".into(),
                        "C,,".into(),
                        "-i".into(),
                        "/tmp/kumoba-ca.pem".into(),
                    ],
                )
                .await?;
        }
        session.start_browser(startup_timeout_ms).await?;
        if Path::new("/usr/bin/xfce4-session").is_file() {
            timeout(Duration::from_secs(20), async {
                loop {
                    if !session.alive() {
                        return Err(failed("XFCE session exited"));
                    }
                    if session
                        .tool(
                            "/usr/bin/xdotool",
                            &[
                                "search".into(),
                                "--onlyvisible".into(),
                                "--class".into(),
                                "xfce4-panel".into(),
                            ],
                        )
                        .await
                        .is_ok()
                    {
                        break Ok(());
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            })
            .await
            .map_err(|_| failed("XFCE panel readiness timeout"))??;
        }
        timeout(Duration::from_secs(5), async {
            loop {
                if !session.alive() {
                    return Err(failed("desktop VNC process exited"));
                }
                if let Ok(mut stream) =
                    TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, VNC_PORT)).await
                {
                    let mut banner = [0; 12];
                    if matches!(
                        timeout(Duration::from_millis(300), stream.read_exact(&mut banner)).await,
                        Ok(Ok(_))
                    ) && banner.starts_with(b"RFB ")
                    {
                        return Ok(());
                    }
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .map_err(|_| failed("VNC readiness timeout"))??;
        Ok(session)
    }
    async fn start_browser(&mut self, startup_timeout_ms: u64) -> io::Result<()> {
        if self.browser.as_mut().is_some_and(Process::alive) || browser_ready().await {
            return Ok(());
        }
        if let Some(mut browser) = self.browser.take() {
            browser.stop().await;
        }
        let mut browser = if Path::new("/usr/local/bin/kumoba-browser").is_file() {
            self.command("/usr/local/bin/kumoba-browser")
        } else {
            let profile = self.directory.path().join("chromium");
            let mut browser = self.command(chromium().unwrap());
            browser
                .args([
                    "--no-sandbox",
                    "--disable-gpu",
                    "--disable-dev-shm-usage",
                    "--disable-quic",
                    "--disable-background-networking",
                    "--disable-component-update",
                    "--disable-sync",
                    "--no-first-run",
                    "--no-default-browser-check",
                    "--password-store=basic",
                    "--ozone-platform=x11",
                    "--force-device-scale-factor=1",
                    "--start-maximized",
                    "--window-position=0,0",
                    "--disk-cache-size=16777216",
                ])
                .arg(format!("--user-data-dir={}", profile.display()))
                .arg(format!("--remote-debugging-port={CDP_PORT}"))
                .arg("--remote-debugging-address=127.0.0.1")
                .arg(format!("--window-size={},{}", self.width, self.height));
            if std::env::var("KUMOBA_EGRESS").as_deref() == Ok("1") {
                browser.arg("--proxy-server=http://127.0.0.1:3128");
            }
            browser
        };
        browser.arg("about:blank");
        self.browser = Some(Process::spawn(browser)?);
        let deadline = Instant::now() + Duration::from_millis(startup_timeout_ms);
        loop {
            if !self.alive() || !self.browser.as_mut().is_some_and(Process::alive) {
                return Err(failed("desktop process exited during startup"));
            }
            if browser_ready().await {
                break;
            }
            if Instant::now() > deadline {
                return Err(failed("Chromium readiness timeout"));
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        Ok(())
    }
    async fn start_xfce(&mut self) -> io::Result<()> {
        let config = self.home().join(".config/xfce4/xfconf/xfce-perchannel-xml");
        std::fs::create_dir_all(&config)?;
        for name in ["xfce4-panel.xml", "xfce4-session.xml"] {
            if !config.join(name).exists() {
                std::fs::copy(
                    Path::new("/usr/local/share/kumoba/xfce").join(name),
                    config.join(name),
                )?;
            }
        }
        let mut bus = self.command("/usr/bin/dbus-daemon");
        let socket = self.directory.path().join("runtime/bus");
        bus.args(["--session", "--nofork", "--nopidfile"])
            .arg(format!("--address=unix:path={}", socket.display()));
        self.processes.push(Process::spawn(bus)?);
        timeout(Duration::from_secs(5), async {
            loop {
                if !self.alive() {
                    return Err(failed("desktop D-Bus exited"));
                }
                if UnixStream::connect(&socket).await.is_ok() {
                    break Ok(());
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .map_err(|_| failed("desktop D-Bus readiness timeout"))??;
        self.processes
            .push(Process::spawn(self.command("/usr/bin/xfce4-session"))?);
        Ok(())
    }
    async fn screenshot(&self) -> io::Result<Response> {
        let path = self.directory.path().join("screen.png");
        let _ = std::fs::remove_file(&path);
        self.tool("/usr/bin/scrot", &[path.display().to_string()])
            .await?;
        let mut png = Vec::new();
        tokio::fs::File::open(path)
            .await?
            .take((MAX_SCREENSHOT + 1) as u64)
            .read_to_end(&mut png)
            .await?;
        if png.len() > MAX_SCREENSHOT {
            return Err(failed("desktop screenshot exceeds limit"));
        }
        Ok(Response::Screenshot {
            width: self.width,
            height: self.height,
            png: STANDARD.encode(png),
        })
    }
    async fn action(&self, action: Action) -> io::Result<Response> {
        if !valid_action(&action, self.width, self.height) {
            return Err(failed("invalid desktop action"));
        }
        let args = match action {
            Action::Move { x, y } => vec!["mousemove".into(), x.to_string(), y.to_string()],
            Action::Click {
                x,
                y,
                button,
                count,
            } => vec![
                "mousemove".into(),
                x.to_string(),
                y.to_string(),
                "click".into(),
                "--repeat".into(),
                count.to_string(),
                button.to_string(),
            ],
            Action::Button { button, down } => vec![
                if down {
                    "mousedown".into()
                } else {
                    "mouseup".into()
                },
                button.to_string(),
            ],
            Action::Scroll { x, y, rows } => {
                if rows == 0 {
                    return Ok(Response::Ok);
                }
                vec![
                    "mousemove".into(),
                    x.to_string(),
                    y.to_string(),
                    "click".into(),
                    "--repeat".into(),
                    rows.unsigned_abs().to_string(),
                    if rows > 0 { "5".into() } else { "4".into() },
                ]
            }
            Action::Type { text } => vec![
                "type".into(),
                "--clearmodifiers".into(),
                "--delay".into(),
                "0".into(),
                "--".into(),
                text,
            ],
            Action::Key { chord } => vec!["key".into(), "--clearmodifiers".into(), chord],
        };
        self.tool("/usr/bin/xdotool", &args).await?;
        Ok(Response::Ok)
    }
}

#[derive(Default)]
pub(crate) struct Desktop {
    session: Option<Session>,
}
impl Desktop {
    pub(crate) fn inherit_environment(
        &mut self,
        env: &mut std::collections::BTreeMap<String, String>,
    ) {
        if Path::new("/usr/bin/xfce4-session").is_file()
            && let Some(session) = self.session.as_mut()
            && session.alive()
        {
            for (name, value) in session.environment() {
                env.entry(name).or_insert(value);
            }
        }
    }
    pub(crate) async fn request(&mut self, request: DesktopRequest) -> io::Result<Response> {
        if self
            .session
            .as_mut()
            .is_some_and(|session| !session.alive())
            && let Some(mut session) = self.session.take()
        {
            session.stop().await?;
        }
        match request {
            DesktopRequest::Start {
                width,
                height,
                startup_timeout_ms,
            } => {
                if let Some(session) = &mut self.session {
                    if session.width != width || session.height != height {
                        return Err(failed("stop the desktop before changing dimensions"));
                    }
                    session.start_browser(startup_timeout_ms).await?;
                    return Ok(session.info());
                }
                let session = Session::start(width, height, startup_timeout_ms).await?;
                let info = session.info();
                self.session = Some(session);
                Ok(info)
            }
            DesktopRequest::Stop => {
                if let Some(mut session) = self.session.take() {
                    session.stop().await?;
                }
                Ok(Response::Ok)
            }
            DesktopRequest::Screenshot => {
                self.session
                    .as_ref()
                    .ok_or_else(|| failed("desktop is not running"))?
                    .screenshot()
                    .await
            }
            DesktopRequest::Action(action) => {
                self.session
                    .as_ref()
                    .ok_or_else(|| failed("desktop is not running"))?
                    .action(action)
                    .await
            }
        }
    }
}
