use crate::{
    Command, Error, ExecOutput, ExecSummary, GuestPath, Network, Result, SandboxId, State,
    StopOutcome, Termination,
    runtime::{Inner, Manifest, Snapshot, save_manifest},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use kumoba_protocol::{Request, Response, VERSION, WorkerConfig, receive, send};
use std::{
    fs::File,
    os::{
        fd::AsRawFd,
        unix::fs::{OpenOptionsExt, PermissionsExt},
    },
    path::PathBuf,
    process::{ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    net::UnixStream,
    process::ChildStdin,
    sync::{oneshot, watch},
};

const MACOS_BOOT_TIMEOUT: Duration = Duration::from_secs(300);
const OUTDATED_MACOS_AGENT: &str = "this macOS sandbox's guest agent is from another kumoba release; run `kumobad macos-image` to update the image, then create the sandbox again";
const MACOS_STOP_TIMEOUT: Duration = Duration::from_secs(90);
const VM_LIMIT_EXIT: i32 = 75;

pub type Tunnel = UnixStream;

pub struct Sandbox {
    inner: Arc<Inner>,
    directory: PathBuf,
    manifest: Manifest,
    lock: File,
    phase: Phase,
    copying: Arc<AtomicBool>,
    bindings: crate::secrets::LiveBindings,
}
enum Phase {
    Stopped,
    Running(Box<Running>),
    Deleted,
}
struct Running {
    owner: Option<ChildStdin>,
    socket: PathBuf,
    agent: Option<UnixStream>,
    kill: Option<oneshot::Sender<()>>,
    exit: watch::Receiver<Option<std::result::Result<ExitStatus, String>>>,
    tainted: bool,
    desktop_available: bool,
    desktop_startup_timeout: bool,
    shell_available: bool,
    upload_available: bool,
    os: String,
    desktop: Option<crate::DesktopInfo>,
}
impl Running {
    fn request_kill(&mut self) {
        self.tainted = true;
        if let Some(kill) = self.kill.take() {
            let _ = kill.send(());
        }
    }
    async fn wait(&mut self) -> Result<ExitStatus> {
        loop {
            if let Some(result) = self.exit.borrow().clone() {
                return result.map_err(Error::OutcomeUnknown);
            }
            self.exit.changed().await.map_err(|_| {
                Error::OutcomeUnknown("worker supervisor ended without exit status".into())
            })?;
        }
    }
    async fn exec<O, E>(
        &mut self,
        request: Request,
        timeout: Duration,
        output_limit: usize,
        stdout: &mut O,
        stderr: &mut E,
        progress: &mut crate::execution::Progress,
    ) -> Result<(Termination, bool)>
    where
        O: tokio::io::AsyncWrite + Unpin + Send,
        E: tokio::io::AsyncWrite + Unpin + Send,
    {
        let mut guard = CancelGuard {
            vm: self,
            armed: true,
        };
        let mut exit = guard.vm.exit.clone();
        let stream = guard.vm.agent.as_mut().ok_or(Error::NotRunning)?;
        let result = tokio::time::timeout(timeout, async {
            tokio::select! {
                result = crate::execution::drive(stream, request, output_limit, stdout, stderr, progress) => result,
                _ = async {
                    loop {
                        if exit.borrow().is_some() { break; }
                        if exit.changed().await.is_err() { break; }
                    }
                } => Err(Error::OutcomeUnknown("worker exited during execution".into())),
            }
        })
        .await;
        if matches!(result, Ok(Ok(_) | Err(Error::Guest(_)))) {
            guard.armed = false;
        }
        result.unwrap_or(Ok((Termination::TimedOut, true)))
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        self.owner.take();
        self.request_kill();
    }
}
struct CancelGuard<'a> {
    vm: &'a mut Running,
    armed: bool,
}
impl Drop for CancelGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.vm.request_kill();
        }
    }
}
struct CopyGuard(Arc<AtomicBool>);
impl Drop for CopyGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Sandbox {
    pub(crate) fn new(
        inner: Arc<Inner>,
        directory: PathBuf,
        manifest: Manifest,
        lock: File,
    ) -> Self {
        let bindings = crate::secrets::LiveBindings::new(manifest.config.secrets.clone());
        Self {
            inner,
            directory,
            manifest,
            bindings,
            lock,
            phase: Phase::Stopped,
            copying: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn id(&self) -> SandboxId {
        self.manifest.id
    }
    pub fn config(&self) -> &crate::SandboxConfig {
        &self.manifest.config
    }
    pub fn state(&mut self) -> Result<State> {
        if let Phase::Running(vm) = &mut self.phase
            && matches!(&*vm.exit.borrow(), Some(Ok(_)))
        {
            self.phase = Phase::Stopped;
        }
        Ok(match self.phase {
            Phase::Stopped => State::Stopped,
            Phase::Running(_) => State::Running,
            Phase::Deleted => State::Deleted,
        })
    }
    fn available(&self) -> Result<()> {
        if matches!(self.phase, Phase::Deleted) {
            return Err(Error::Deleted);
        }
        if self.copying.load(Ordering::Acquire) {
            return Err(Error::Busy);
        }
        Ok(())
    }
    pub async fn start(&mut self) -> Result<()> {
        self.available()?;
        self.manifest.config.validate()?;
        self.inner.validate_secrets(&self.manifest.config)?;
        if self.state()? == State::Running {
            if matches!(&self.phase,Phase::Running(vm) if !vm.tainted) {
                return Ok(());
            }
            self.force_stop().await?;
        }
        let machine = crate::runtime::is_machine(&self.directory).then(|| self.directory.clone());
        if machine.is_some() && !self.manifest.config.mounts.is_empty() {
            return Err(Error::InvalidInput(
                "host mounts are not supported in macOS sandboxes yet".into(),
            ));
        }
        let permit = self
            .inner
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Capacity)?;
        let machine_permit = match machine {
            Some(_) => Some(
                self.inner
                    .macos_capacity
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| Error::Capacity)?,
            ),
            None => None,
        };
        let machine_vm = machine.is_some();
        let boot_timeout = if machine_vm {
            self.inner.config.boot_timeout.max(MACOS_BOOT_TIMEOUT)
        } else {
            self.inner.config.boot_timeout
        };
        let socket_dir = tempfile::Builder::new()
            .prefix("ks-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()?;
        let socket = socket_dir.path().join("agent.sock");
        let (egress_socket, mut broker) = match &self.manifest.config.network {
            Network::Disabled => (None, None),
            Network::HttpConnect(policy) => {
                let path = socket_dir.path().join("egress.sock");
                let inspection = match (&self.inner.authority, &self.inner.config.secret_store) {
                    (Some(authority), Some(store)) => Some(crate::secret_https::Inspection::new(
                        authority.clone(),
                        crate::secrets::SecretSession::new(store.clone(), self.bindings.clone()),
                    )),
                    _ => None,
                };
                let sink = self
                    .inner
                    .config
                    .access_sink
                    .as_ref()
                    .map(|sink| sink.for_sandbox(self.id()));
                let broker = crate::egress::Broker::bind(
                    &path,
                    policy.clone(),
                    inspection,
                    sink,
                    self.inner.config.egress.clone(),
                )
                .await?;
                (Some(path), Some(broker))
            }
        };
        let worker = WorkerConfig {
            version: VERSION,
            library: self.inner.config.library.clone(),
            firmware: self.inner.config.firmware.clone(),
            disk: self.directory.join("root.raw"),
            socket: socket.clone(),
            egress_socket,
            console: "/dev/null".into(),
            vcpus: self.manifest.config.vcpus,
            memory_mib: self.manifest.config.memory_mib,
            lock_fd: self.lock.as_raw_fd(),
            mounts: self.manifest.config.mounts.clone(),
            machine,
        };
        self.manifest.clean = false;
        save_manifest(&self.directory, &self.manifest)?;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(self.directory.join("worker.log"))?;
        let mut process = tokio::process::Command::new(&self.inner.config.worker);
        process
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(log)
            .kill_on_drop(true);
        let fd = self.lock.as_raw_fd();
        #[cfg(target_os = "macos")]
        process.env(
            "DYLD_FALLBACK_LIBRARY_PATH",
            self.inner.config.firmware.parent().unwrap(),
        );
        #[cfg(target_os = "linux")]
        process.env(
            "LD_LIBRARY_PATH",
            self.inner.config.firmware.parent().unwrap(),
        );
        unsafe {
            process.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = process.spawn()?;
        let owner = child.stdin.take().unwrap();
        let (kill, mut kill_rx) = oneshot::channel();
        let (exit_tx, exit) = watch::channel(None);
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let status = tokio::select! {
                status = child.wait() => status,
                _ = async {
                    match &mut broker {
                        Some(broker) => broker.wait().await,
                        None => std::future::pending().await,
                    }
                } => {
                    let _ = child.start_kill();
                    child.wait().await
                },
                _ = &mut kill_rx => {
                    let _ = child.start_kill();
                    child.wait().await
                }
            };
            if let Some(broker) = &mut broker {
                let _ = broker.shutdown().await;
            }
            drop(permit);
            drop(machine_permit);
            drop(socket_dir);
            let _ = exit_tx.send(Some(status.map_err(|e| e.to_string())));
            drop(inner);
        });
        self.phase = Phase::Running(Box::new(Running {
            owner: Some(owner),
            socket,
            agent: None,
            kill: Some(kill),
            exit,
            tainted: false,
            desktop_available: false,
            desktop_startup_timeout: false,
            shell_available: false,
            upload_available: false,
            os: "linux".into(),
            desktop: None,
        }));
        let Phase::Running(vm) = &mut self.phase else {
            unreachable!()
        };
        let mut guard = CancelGuard { vm, armed: true };
        let mut encoded = serde_json::to_vec(&worker)?;
        encoded.push(b'\n');
        guard.vm.owner.as_mut().unwrap().write_all(&encoded).await?;
        guard.vm.owner.as_mut().unwrap().flush().await?;
        let ready = tokio::time::timeout(boot_timeout, async {
            loop {
                if let Some(status) = guard.vm.exit.borrow().as_ref() {
                    if machine_vm
                        && matches!(status, Ok(status) if status.code() == Some(VM_LIMIT_EXIT))
                    {
                        return Err(Error::Capacity);
                    }
                    return Err(Error::Boot(format!(
                        "worker exited {status:?}; inspect {}",
                        self.directory.join("worker.log").display()
                    )));
                }
                if let Ok(mut socket) = UnixStream::connect(&guard.vm.socket).await
                    && send(&mut socket, &Request::Hello).await.is_ok()
                    && let Ok(Ok(Response::Hello {
                        version,
                        desktop,
                        desktop_startup_timeout,
                        shell,
                        os,
                        clock,
                        upload,
                        ..
                    })) =
                        tokio::time::timeout(Duration::from_millis(300), receive(&mut socket)).await
                {
                    if machine_vm && (version != VERSION || !clock) {
                        return Err(Error::Protocol(OUTDATED_MACOS_AGENT.into()));
                    }
                    if version != VERSION {
                        return Err(Error::Protocol("guest agent version mismatch".into()));
                    }
                    socket.write_u8(1).await?;
                    guard.vm.agent = Some(socket);
                    guard.vm.desktop_available = desktop;
                    guard.vm.desktop_startup_timeout = desktop_startup_timeout;
                    guard.vm.shell_available = shell;
                    guard.vm.upload_available = upload;
                    guard.vm.os = os;
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .map_err(|_| Error::Boot("agent readiness deadline exceeded".into()))?;
        ready?;
        guard.armed = false;
        drop(guard);
        if self.is_macos()
            && let Err(error) = self.set_guest_clock().await
        {
            self.force_stop().await?;
            return Err(error);
        }
        if let Err(error) = self.mount_host_directories().await {
            self.force_stop().await?;
            return Err(error);
        }
        if let Some(authority) = &self.inner.authority
            && matches!(self.manifest.config.network, Network::HttpConnect(_))
        {
            let certificate = authority.certificate_pem();
            if let Err(error) = self.install_trust(&certificate).await {
                self.force_stop().await?;
                return Err(error);
            }
        }
        Ok(())
    }

    pub fn set_mounts(&mut self, mounts: Vec<crate::HostMount>) -> Result<()> {
        self.available()?;
        if self.state()? != State::Stopped {
            return Err(Error::Busy);
        }
        if !mounts.is_empty() && crate::runtime::is_machine(&self.directory) {
            return Err(Error::InvalidInput(
                "host mounts are not supported in macOS sandboxes yet".into(),
            ));
        }
        let mut next = self.manifest.config.clone();
        next.mounts = mounts;
        next.validate()?;
        let previous = std::mem::replace(&mut self.manifest.config, next);
        if let Err(error) = save_manifest(&self.directory, &self.manifest) {
            self.manifest.config = previous;
            return Err(error);
        }
        Ok(())
    }

    pub fn set_network(&mut self, network: Network) -> Result<()> {
        self.available()?;
        if self.state()? != State::Stopped {
            return Err(Error::Busy);
        }
        let mut next = self.manifest.config.clone();
        next.network = network;
        next.validate()?;
        let previous = std::mem::replace(&mut self.manifest.config, next);
        if let Err(error) = save_manifest(&self.directory, &self.manifest) {
            self.manifest.config = previous;
            return Err(error);
        }
        Ok(())
    }

    async fn mount_host_directories(&mut self) -> Result<()> {
        for (index, mount) in self.manifest.config.mounts.clone().iter().enumerate() {
            let mkdir = self
                .exec(Command::new("/bin/mkdir").args(["-p", &mount.target]))
                .await?;
            if !mkdir.success() {
                return Err(Error::Boot("could not create guest mount directory".into()));
            }
            let mounted = self
                .exec(Command::new("/bin/mount").args([
                    "-t",
                    "virtiofs",
                    "-o",
                    if mount.read_only {
                        "ro,nosuid,nodev"
                    } else {
                        "rw,nosuid,nodev"
                    },
                    &format!("kumoba-{index}"),
                    &mount.target,
                ]))
                .await?;
            if !mounted.success() {
                return Err(Error::Boot(format!(
                    "host directory mount failed: {}",
                    String::from_utf8_lossy(&mounted.stderr)
                )));
            }
        }
        Ok(())
    }

    pub fn screen_socket(&self) -> Option<PathBuf> {
        match &self.phase {
            Phase::Running(vm) if !vm.tainted && crate::runtime::is_machine(&self.directory) => {
                Some(vm.socket.with_file_name(kumoba_protocol::SCREEN_SOCKET))
            }
            _ => None,
        }
    }

    fn is_macos(&self) -> bool {
        matches!(&self.phase, Phase::Running(vm) if vm.os == "macos")
    }

    async fn set_guest_clock(&mut self) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| Error::Boot(e.to_string()))?;
        let unix_micros = u64::try_from(now.as_micros()).map_err(|e| Error::Boot(e.to_string()))?;
        match self
            .rpc(Request::SetClock { unix_micros }, Duration::from_secs(10))
            .await?
        {
            Response::Ok => Ok(()),
            Response::Error(e) => Err(Error::Boot(format!("could not set the guest clock: {e}"))),
            _ => Err(Error::Protocol("unexpected clock response".into())),
        }
    }

    async fn install_trust(&mut self, certificate: &str) -> Result<()> {
        let system_bundle = if self.is_macos() {
            "/etc/ssl/cert.pem"
        } else {
            "/etc/ssl/certs/ca-certificates.crt"
        };
        let mut bundle = self
            .read_file(
                &GuestPath::new(system_bundle)?,
                kumoba_protocol::MAX_DATA / 2,
            )
            .await?;
        bundle.extend_from_slice(b"\n");
        bundle.extend_from_slice(certificate.as_bytes());
        self.write_file(
            &GuestPath::new("/tmp/kumoba-ca.pem")?,
            certificate.as_bytes(),
        )
        .await?;
        self.write_file(&GuestPath::new("/tmp/kumoba-ca-bundle.pem")?, &bundle)
            .await
    }

    pub async fn update_secret_bindings(
        &mut self,
        bindings: std::collections::BTreeMap<String, crate::SecretId>,
    ) -> Result<()> {
        self.available()?;
        let mut next = self.manifest.config.clone();
        next.secrets = bindings.clone();
        next.validate()?;
        self.inner.validate_secrets(&next)?;
        let previous = std::mem::replace(&mut self.manifest.config, next);
        if let Err(error) = save_manifest(&self.directory, &self.manifest) {
            self.manifest.config = previous;
            return Err(error);
        }
        if let Err(error) = self.bindings.replace(bindings.clone()) {
            self.force_stop().await?;
            self.bindings = crate::secrets::LiveBindings::new(bindings);
            return Err(error);
        }
        Ok(())
    }

    pub(crate) async fn rpc(&mut self, request: Request, timeout: Duration) -> Result<Response> {
        self.available()?;
        self.state()?;
        let Phase::Running(vm) = &mut self.phase else {
            return Err(Error::NotRunning);
        };
        if vm.tainted {
            return Err(Error::NotRunning);
        }
        let mut guard = CancelGuard { vm, armed: true };
        let response = tokio::time::timeout(timeout, async {
            let stream = guard
                .vm
                .agent
                .as_mut()
                .ok_or_else(|| std::io::Error::other("agent not connected"))?;
            send(stream, &request).await?;
            let response = receive::<_, Response>(stream).await?;
            stream.write_u8(1).await?;
            Ok::<_, std::io::Error>(response)
        })
        .await
        .map_err(|_| Error::OutcomeUnknown("response deadline exceeded; VM terminated".into()))?
        .map_err(|e| Error::OutcomeUnknown(e.to_string()))?;
        guard.armed = false;
        Ok(response)
    }

    pub(crate) fn require_desktop_capability(&mut self) -> Result<()> {
        self.available()?;
        self.state()?;
        match &self.phase {
            Phase::Running(vm) if !vm.tainted && vm.desktop_available => Ok(()),
            Phase::Running(_) => Err(Error::InvalidInput(
                "image does not provide a running desktop-capable agent".into(),
            )),
            _ => Err(Error::NotRunning),
        }
    }
    pub fn supports_shell(&self) -> bool {
        matches!(&self.phase, Phase::Running(vm) if vm.shell_available)
    }

    pub(crate) fn supports_desktop_startup_timeout(&self) -> bool {
        matches!(&self.phase, Phase::Running(vm) if vm.desktop_startup_timeout)
    }

    pub(crate) fn set_desktop_info(&mut self, info: Option<crate::DesktopInfo>) -> Result<()> {
        if let Phase::Running(vm) = &mut self.phase {
            vm.desktop = info;
            Ok(())
        } else {
            Err(Error::NotRunning)
        }
    }
    pub fn desktop_info(&mut self) -> Result<crate::DesktopInfo> {
        self.require_desktop_capability()?;
        if let Phase::Running(vm) = &self.phase {
            vm.desktop
                .ok_or_else(|| Error::Guest("desktop is not running".into()))
        } else {
            Err(Error::NotRunning)
        }
    }

    pub async fn exec(&mut self, command: Command) -> Result<ExecOutput> {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let summary = self
            .exec_streamed(command, &mut stdout, &mut stderr)
            .await?;
        Ok(ExecOutput {
            termination: summary.termination,
            stdout,
            stderr,
            truncated: summary.truncated,
        })
    }

    pub fn prepare_command(&mut self, mut command: Command) -> Result<Command> {
        command.validate()?;
        self.available()?;
        self.state()?;
        if matches!(self.manifest.config.network, Network::HttpConnect(_)) {
            for name in ["HTTPS_PROXY", "https_proxy", "http_proxy"] {
                command
                    .env
                    .entry(name.into())
                    .or_insert_with(|| "http://127.0.0.1:3128".into());
            }
            for name in ["NO_PROXY", "no_proxy"] {
                command
                    .env
                    .entry(name.into())
                    .or_insert_with(|| "localhost,127.0.0.1".into());
            }
        }
        if matches!(&self.phase, Phase::Running(vm) if vm.desktop.is_some()) {
            command
                .env
                .entry("DISPLAY".into())
                .or_insert_with(|| ":99".into());
            command
                .env
                .entry("NODE_PATH".into())
                .or_insert_with(|| "/opt/kumoba/browser/node_modules".into());
        }
        if let Some(store) = &self.inner.config.secret_store {
            let session = crate::secrets::SecretSession::new(store.clone(), self.bindings.clone());
            for (name, placeholder) in session.environment()? {
                if command.env.contains_key(&name) {
                    return Err(Error::InvalidInput(
                        "command overrides a mounted secret variable".into(),
                    ));
                }
                command.env.insert(name, placeholder);
            }
            if matches!(self.manifest.config.network, Network::HttpConnect(_)) {
                for name in ["SSL_CERT_FILE", "CURL_CA_BUNDLE", "REQUESTS_CA_BUNDLE"] {
                    command
                        .env
                        .insert(name.into(), "/tmp/kumoba-ca-bundle.pem".into());
                }
                command
                    .env
                    .insert("NODE_EXTRA_CA_CERTS".into(), "/tmp/kumoba-ca.pem".into());
            }
        }
        command.validate()?;
        Ok(command)
    }

    pub fn agent(&mut self) -> Result<crate::agent::Agent> {
        self.available()?;
        self.state()?;
        let Phase::Running(vm) = &self.phase else {
            return Err(Error::NotRunning);
        };
        if vm.tainted {
            return Err(Error::NotRunning);
        }
        Ok(crate::agent::Agent::new(
            vm.socket.clone(),
            vm.exit.clone(),
            vm.upload_available,
        ))
    }

    pub async fn exec_streamed<O, E>(
        &mut self,
        command: Command,
        stdout: &mut O,
        stderr: &mut E,
    ) -> Result<ExecSummary>
    where
        O: tokio::io::AsyncWrite + Unpin + Send,
        E: tokio::io::AsyncWrite + Unpin + Send,
    {
        let command = self.prepare_command(command)?;
        let timeout = command.timeout;
        let output_limit = command.output_limit;
        let request = Request::Exec(command.into_request());
        let mut progress = crate::execution::Progress::default();
        let (result, tainted) = {
            let Phase::Running(vm) = &mut self.phase else {
                return Err(Error::NotRunning);
            };
            if vm.tainted {
                return Err(Error::NotRunning);
            }
            let result = vm
                .exec(
                    request,
                    timeout,
                    output_limit,
                    stdout,
                    stderr,
                    &mut progress,
                )
                .await;
            (result, vm.tainted)
        };
        let (termination, truncated) = match result {
            Ok(result) => result,
            Err(error) => {
                if tainted {
                    self.force_stop().await?;
                }
                return Err(error);
            }
        };
        if termination == Termination::TimedOut {
            self.force_stop().await?;
        }
        Ok(ExecSummary {
            termination,
            stdout_bytes: progress.stdout,
            stderr_bytes: progress.stderr,
            truncated,
        })
    }
    pub async fn read_file(&mut self, path: &GuestPath, max_bytes: usize) -> Result<Vec<u8>> {
        if max_bytes > kumoba_protocol::MAX_DATA {
            return Err(Error::InvalidInput("maximum file transfer is 1 MiB".into()));
        }
        match self
            .rpc(
                Request::Read {
                    path: path.as_str().into(),
                    max_bytes,
                },
                Duration::from_secs(10),
            )
            .await?
        {
            Response::Data(data) => decode_bounded(data, max_bytes),
            Response::Error(error) => Err(Error::Guest(error)),
            _ => Err(Error::Protocol("unexpected file response".into())),
        }
    }
    pub async fn write_file(&mut self, path: &GuestPath, data: &[u8]) -> Result<()> {
        if data.len() > kumoba_protocol::MAX_DATA {
            return Err(Error::InvalidInput("maximum file transfer is 1 MiB".into()));
        }
        match self
            .rpc(
                Request::Write {
                    path: path.as_str().into(),
                    data: STANDARD.encode(data),
                },
                Duration::from_secs(10),
            )
            .await?
        {
            Response::Ok => Ok(()),
            Response::Error(e) => Err(Error::Guest(e)),
            _ => Err(Error::Protocol("unexpected file response".into())),
        }
    }
    pub async fn connect(&mut self, port: u16) -> Result<Tunnel> {
        self.available()?;
        self.state()?;
        if port == 0 {
            return Err(Error::InvalidInput("port must be nonzero".into()));
        }
        let Phase::Running(vm) = &self.phase else {
            return Err(Error::NotRunning);
        };
        if vm.tainted {
            return Err(Error::NotRunning);
        }
        connect_guest(&vm.socket, port).await
    }
    pub async fn stop(&mut self) -> Result<StopOutcome> {
        self.available()?;
        if self.state()? == State::Stopped {
            return Ok(StopOutcome::AlreadyStopped);
        }
        let grace = if crate::runtime::is_machine(&self.directory) {
            self.inner.config.stop_timeout.max(MACOS_STOP_TIMEOUT)
        } else {
            self.inner.config.stop_timeout
        };
        let acknowledged = matches!(self.rpc(Request::Shutdown, grace).await, Ok(Response::Ok));
        let Phase::Running(vm) = &mut self.phase else {
            return Ok(StopOutcome::AlreadyStopped);
        };
        let status = {
            let mut guard = CancelGuard { vm, armed: true };
            let status = tokio::time::timeout(grace, guard.vm.wait()).await;
            guard.armed = false;
            status
        };
        let clean = acknowledged && matches!(status, Ok(Ok(s)) if s.success());
        if !clean {
            self.force_stop().await?;
            return Ok(StopOutcome::Forced);
        }
        self.phase = Phase::Stopped;
        self.manifest.clean = true;
        save_manifest(&self.directory, &self.manifest)?;
        Ok(StopOutcome::Graceful)
    }
    pub async fn force_stop(&mut self) -> Result<()> {
        self.available()?;
        if let Phase::Running(vm) = &mut self.phase {
            vm.request_kill();
            vm.wait().await?;
        }
        self.phase = Phase::Stopped;
        self.manifest.clean = false;
        save_manifest(&self.directory, &self.manifest)?;
        Ok(())
    }
    pub async fn snapshot(&mut self) -> Result<Snapshot> {
        self.available()?;
        if crate::runtime::is_machine(&self.directory) {
            return Err(Error::InvalidInput(
                "snapshots of macOS sandboxes are not supported yet".into(),
            ));
        }
        if self.state()? != State::Stopped || !self.manifest.clean {
            return Err(Error::NotClean);
        }
        self.copying.store(true, Ordering::Release);
        let guard = CopyGuard(self.copying.clone());
        let lock = self.lock.try_clone()?;
        let inner = self.inner.clone();
        let source = self.directory.join("root.raw");
        let config = self.manifest.config.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            let _lock = lock;
            crate::runtime::snapshot(inner, &source, config)
        })
        .await?
    }
    pub async fn delete(&mut self) -> Result<()> {
        let trash = self
            .inner
            .config
            .state_dir
            .join("trash")
            .join(self.id().to_string());
        if !matches!(self.phase, Phase::Deleted) {
            self.available()?;
            self.stop().await?;
            std::fs::rename(&self.directory, &trash)?;
            self.phase = Phase::Deleted;
            File::open(self.directory.parent().unwrap())?.sync_all()?;
            File::open(trash.parent().unwrap())?.sync_all()?;
        }
        crate::runtime::remove_trash(&trash)
    }
}

fn decode_bounded(data: String, limit: usize) -> Result<Vec<u8>> {
    if data.len() > limit.div_ceil(3) * 4 {
        return Err(Error::Protocol("response exceeds data limit".into()));
    }
    let bytes = STANDARD
        .decode(data)
        .map_err(|e| Error::Protocol(e.to_string()))?;
    if bytes.len() > limit {
        return Err(Error::Protocol("response exceeds data limit".into()));
    }
    Ok(bytes)
}

pub(crate) async fn connect_guest(socket: &std::path::Path, port: u16) -> Result<Tunnel> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut stream = UnixStream::connect(socket).await?;
        send(&mut stream, &Request::Connect { port }).await?;
        match receive(&mut stream).await? {
            Response::Connected => {
                stream.write_u8(1).await?;
                Ok(stream)
            }
            Response::Error(e) => {
                stream.write_u8(1).await?;
                Err(Error::Guest(e))
            }
            _ => Err(Error::Protocol("unexpected tunnel response".into())),
        }
    })
    .await
    .map_err(|_| Error::Guest("guest port connection timed out".into()))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use kumoba_protocol::ExecEvent;
    use tokio::io::AsyncReadExt;

    struct Fake {
        vm: Running,
        killed: oneshot::Receiver<()>,
        exit: watch::Sender<Option<std::result::Result<ExitStatus, String>>>,
    }

    fn running(agent: UnixStream) -> Fake {
        let (kill, killed) = oneshot::channel();
        let (exit_tx, exit) = watch::channel(None);
        Fake {
            vm: Running {
                owner: None,
                socket: PathBuf::new(),
                agent: Some(agent),
                kill: Some(kill),
                exit,
                tainted: false,
                desktop_available: false,
                desktop_startup_timeout: false,
                shell_available: false,
                os: "linux".into(),
                desktop: None,
            },
            killed,
            exit: exit_tx,
        }
    }

    async fn answer(guest: &mut UnixStream, event: ExecEvent) {
        let _: Request = receive(guest).await.unwrap();
        send(guest, &event).await.unwrap();
        assert_eq!(guest.read_u8().await.unwrap(), 1);
    }

    async fn exec(vm: &mut Running) -> Result<(Termination, bool)> {
        exec_within(vm, Duration::from_secs(5)).await
    }

    async fn exec_within(vm: &mut Running, timeout: Duration) -> Result<(Termination, bool)> {
        vm.exec(
            Request::Hello,
            timeout,
            1024,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut crate::execution::Progress::default(),
        )
        .await
    }

    #[tokio::test]
    async fn a_command_that_fails_to_start_keeps_the_agent_connection() {
        let (host, mut guest) = UnixStream::pair().unwrap();
        let mut fake = running(host);
        let peer = tokio::spawn(async move {
            answer(
                &mut guest,
                ExecEvent::Failed {
                    message: "No such file or directory".into(),
                },
            )
            .await;
            answer(
                &mut guest,
                ExecEvent::Finished {
                    code: Some(0),
                    signal: None,
                    timed_out: false,
                    truncated: false,
                },
            )
            .await;
        });
        assert!(matches!(exec(&mut fake.vm).await, Err(Error::Guest(_))));
        assert!(!fake.vm.tainted);
        assert!(fake.killed.try_recv().is_err());
        assert_eq!(
            exec(&mut fake.vm).await.unwrap(),
            (Termination::Exited(0), false)
        );
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn a_broken_exec_stream_still_stops_the_vm() {
        let (host, mut guest) = UnixStream::pair().unwrap();
        let mut fake = running(host);
        let peer = tokio::spawn(async move {
            let _: Request = receive(&mut guest).await.unwrap();
            send(
                &mut guest,
                &ExecEvent::Finished {
                    code: Some(0),
                    signal: Some(9),
                    timed_out: false,
                    truncated: false,
                },
            )
            .await
            .unwrap();
        });
        assert!(matches!(exec(&mut fake.vm).await, Err(Error::Protocol(_))));
        assert!(fake.vm.tainted);
        assert!(fake.killed.try_recv().is_ok());
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn a_command_past_its_timeout_still_stops_the_vm() {
        let (host, mut guest) = UnixStream::pair().unwrap();
        let mut fake = running(host);
        let peer = tokio::spawn(async move {
            let _: Request = receive(&mut guest).await.unwrap();
            guest
        });
        assert_eq!(
            exec_within(&mut fake.vm, Duration::from_millis(50))
                .await
                .unwrap(),
            (Termination::TimedOut, true)
        );
        assert!(fake.vm.tainted);
        assert!(fake.killed.try_recv().is_ok());
        drop(peer.await.unwrap());
    }

    #[tokio::test]
    async fn a_worker_exit_during_exec_still_stops_the_vm() {
        let (host, mut guest) = UnixStream::pair().unwrap();
        let mut fake = running(host);
        let exit = fake.exit.clone();
        let peer = tokio::spawn(async move {
            let _: Request = receive(&mut guest).await.unwrap();
            exit.send(Some(Err("worker crashed".into()))).unwrap();
            guest
        });
        assert!(matches!(
            exec(&mut fake.vm).await,
            Err(Error::OutcomeUnknown(_))
        ));
        assert!(fake.vm.tainted);
        assert!(fake.killed.try_recv().is_ok());
        drop(peer.await.unwrap());
    }
}
