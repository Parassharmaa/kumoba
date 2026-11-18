use crate::macos::{Event, Failure, Machine, Shared, Vm, configuration, encode, ns_error, url};
use block2::RcBlock;
use kumoba_protocol::{AGENT_PORT, Request, Response, receive, send};
use objc2::{
    AllocAnyThread, msg_send,
    rc::Retained,
    runtime::{AnyClass, AnyObject, Bool},
};
use objc2_foundation::{NSError, NSString};
use objc2_virtualization::{
    VZMACAddress, VZMacAuxiliaryStorage, VZMacAuxiliaryStorageInitializationOptions,
    VZMacMachineIdentifier, VZMacOSInstaller, VZMacOSRestoreImage,
    VZMacOSVirtualMachineStartOptions,
};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

const USER: &str = "kumoba";

struct Options {
    ipsw: PathBuf,
    cache: PathBuf,
    agent: PathBuf,
    out: PathBuf,
    disk_gib: u64,
    cpus: u8,
    memory_mib: u32,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Build {
    machine: Machine,
    stage: String,
}

fn parse(args: Vec<String>) -> Result<Options, Failure> {
    let mut options = Options {
        ipsw: PathBuf::new(),
        cache: PathBuf::new(),
        agent: PathBuf::new(),
        out: PathBuf::new(),
        disk_gib: 64,
        cpus: 4,
        memory_mib: 4096,
    };
    let mut args = args.into_iter();
    while let Some(flag) = args.next() {
        let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--ipsw" => options.ipsw = value.into(),
            "--cache" => options.cache = value.into(),
            "--agent" => options.agent = value.into(),
            "--out" => options.out = value.into(),
            "--disk-gib" => options.disk_gib = value.parse()?,
            "--cpus" => options.cpus = value.parse()?,
            "--memory-mib" => options.memory_mib = value.parse()?,
            _ => return Err(format!("unknown option {flag}").into()),
        }
    }
    if options.out.as_os_str().is_empty() || options.agent.as_os_str().is_empty() {
        return Err(
            "usage: kumoba-worker macos-image --out DIR --agent KUMOBA_GUEST [--ipsw FILE | --cache DIR] [--disk-gib 64] [--cpus 4] [--memory-mib 4096]"
                .into(),
        );
    }
    if !(2..=8).contains(&options.cpus) || !(4096..=16384).contains(&options.memory_mib) {
        return Err("a macOS guest needs 2..=8 CPUs and 4096..=16384 MiB of memory".into());
    }
    Ok(options)
}

pub fn main(args: Vec<String>) -> Result<(), Failure> {
    let options = parse(args)?;
    if AnyClass::get(c"VZMacGuestProvisioningOptions").is_none() {
        return Err("building a macOS image needs macOS 27 or later on this Mac".into());
    }
    if !options.agent.is_file() {
        return Err(format!("{} is not a file", options.agent.display()).into());
    }
    std::fs::create_dir_all(&options.out)?;
    std::fs::set_permissions(&options.out, std::fs::Permissions::from_mode(0o700))?;
    let progress = options.out.join("build.json");
    if options.out.join("machine.json").exists() && !progress.exists() {
        return Err(format!(
            "a macOS image is already at {}; delete that directory to build a new one",
            options.out.display()
        )
        .into());
    }
    let mut build = match File::open(&progress) {
        Ok(file) => serde_json::from_reader(file)?,
        Err(_) => {
            let downloaded = options.ipsw.as_os_str().is_empty();
            let ipsw = if downloaded {
                download(&options.cache)?
            } else {
                options.ipsw.clone()
            };
            let build = install(&options, &ipsw)?;
            if downloaded {
                std::fs::remove_file(&ipsw)?;
            }
            build
        }
    };
    save(&progress, &build)?;
    if build.stage == "installed" {
        provision(&options, &build.machine)?;
        build.stage = "provisioned".into();
        save(&progress, &build)?;
    }
    if build.stage == "provisioned" {
        verify(&options, &build.machine)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(options.out.join("machine.json"))?;
        serde_json::to_writer_pretty(&mut file, &build.machine)?;
        file.sync_all()?;
        std::fs::remove_file(&progress)?;
    }
    say(&format!("macOS image ready at {}", options.out.display()));
    Ok(())
}

fn say(message: &str) {
    eprintln!("kumoba-worker: {message}");
}

fn save(path: &Path, build: &Build) -> Result<(), Failure> {
    let temporary = path.with_extension("tmp");
    serde_json::to_writer_pretty(File::create(&temporary)?, build)?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

fn free_gib(path: &Path) -> Result<u64, Failure> {
    let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &raw mut stats) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(u64::from(stats.f_bavail) * stats.f_frsize / (1 << 30))
}

fn restore_image(path: &Path) -> Result<Shared<Retained<VZMacOSRestoreImage>>, Failure> {
    let (sender, receiver) = mpsc::channel();
    let block = RcBlock::new(
        move |image: *mut VZMacOSRestoreImage, error: *mut NSError| {
            let result = match (unsafe { Retained::retain(image) }, unsafe {
                error.as_ref()
            }) {
                (Some(image), None) => Ok(Shared(image)),
                (_, Some(error)) => Err(ns_error(error).message),
                (None, None) => Err("the restore image did not load".into()),
            };
            let _ = sender.send(result);
        },
    );
    unsafe { VZMacOSRestoreImage::loadFileURL_completionHandler(&url(path), &block) };
    Ok(receiver.recv()??)
}

fn latest() -> Result<String, Failure> {
    let (sender, receiver) = mpsc::channel();
    let block = RcBlock::new(
        move |image: *mut VZMacOSRestoreImage, error: *mut NSError| {
            let result = match (unsafe { image.as_ref() }, unsafe { error.as_ref() }) {
                (Some(image), None) => Ok(unsafe { image.URL() }
                    .absoluteString()
                    .map(|url| url.to_string())
                    .unwrap_or_default()),
                (_, Some(error)) => Err(ns_error(error).message),
                (None, None) => Err("Apple's restore image catalog was empty".into()),
            };
            let _ = sender.send(result);
        },
    );
    unsafe { VZMacOSRestoreImage::fetchLatestSupportedWithCompletionHandler(&block) };
    Ok(receiver.recv()??)
}

fn download(cache: &Path) -> Result<PathBuf, Failure> {
    if cache.as_os_str().is_empty() {
        return Err("pass --ipsw FILE or --cache DIR to download Apple's restore image".into());
    }
    let address = latest()?;
    let name = address
        .rsplit('/')
        .next()
        .filter(|name| name.ends_with(".ipsw"))
        .ok_or("Apple's catalog returned an unexpected restore image address")?;
    std::fs::create_dir_all(cache)?;
    let target = cache.join(name);
    if target.is_file() {
        return Ok(target);
    }
    let partial = cache.join(format!("{name}.part"));
    let free = free_gib(cache)?;
    if free < 50 {
        say(&format!(
            "only {free} GiB free; the download and install need about 50 GiB"
        ));
    }
    say(&format!("downloading {address}"));
    let status = Command::new("/usr/bin/curl")
        .args(["-fL", "-C", "-", "--retry", "5", "--progress-bar", "-o"])
        .arg(&partial)
        .arg(&address)
        .status()?;
    if !status.success() {
        return Err("downloading the restore image failed; run the command again to resume".into());
    }
    std::fs::rename(&partial, &target)?;
    Ok(target)
}

fn install(options: &Options, ipsw: &Path) -> Result<Build, Failure> {
    if !ipsw.is_file() {
        return Err(format!("{} is not a macOS restore image", ipsw.display()).into());
    }
    let free = free_gib(&options.out)?;
    if free < 25 {
        return Err(format!("installing macOS needs about 25 GiB free; {free} GiB left").into());
    }
    say(&format!("loading {}", ipsw.display()));
    let image = restore_image(ipsw)?;
    let requirements = unsafe { image.0.mostFeaturefulSupportedConfiguration() }
        .ok_or("this Mac cannot run the macOS version in the restore image")?;
    let model = unsafe { requirements.hardwareModel() };
    if !unsafe { model.isSupported() } {
        return Err("this Mac does not support the restore image's hardware model".into());
    }
    let minimum_cpus = unsafe { requirements.minimumSupportedCPUCount() };
    let minimum_memory = unsafe { requirements.minimumSupportedMemorySize() } >> 20;
    if usize::from(options.cpus) < minimum_cpus || u64::from(options.memory_mib) < minimum_memory {
        return Err(format!(
            "the restore image needs at least {minimum_cpus} CPUs and {minimum_memory} MiB"
        )
        .into());
    }
    let disk = options.out.join("root.raw");
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&disk)?
        .set_len(options.disk_gib << 30)?;
    unsafe {
        VZMacAuxiliaryStorage::initCreatingStorageAtURL_hardwareModel_options_error(
            VZMacAuxiliaryStorage::alloc(),
            &url(&options.out.join("aux.img")),
            &model,
            VZMacAuxiliaryStorageInitializationOptions(1),
        )
        .map_err(|error| ns_error(&error))?;
    }
    let machine = Machine {
        hardware_model: encode(&*unsafe { model.dataRepresentation() }),
        machine_identifier: encode(&*unsafe { VZMacMachineIdentifier::new().dataRepresentation() }),
        cpus: options.cpus,
        memory_mib: options.memory_mib,
    };
    let (events, _receiver) = mpsc::channel();
    let vm = Vm::new(
        configuration(&machine, &options.out, &disk, None)?,
        events,
        None,
    );
    let installer = vm.on_queue({
        let ipsw = url(ipsw);
        move |machine| {
            Shared(unsafe {
                VZMacOSInstaller::initWithVirtualMachine_restoreImageURL(
                    VZMacOSInstaller::alloc(),
                    machine,
                    &ipsw,
                )
            })
        }
    });
    let (sender, receiver) = mpsc::channel();
    let pending = Shared(installer.0.clone());
    vm.queue.exec_async(move || {
        let pending = pending;
        let block = RcBlock::new(move |error: *mut NSError| {
            let _ = sender.send(match unsafe { error.as_ref() } {
                None => Ok(()),
                Some(error) => Err(ns_error(error).message),
            });
        });
        unsafe { pending.0.installWithCompletionHandler(&block) };
    });
    say("installing macOS; this takes about 15 minutes");
    loop {
        match receiver.recv_timeout(Duration::from_secs(30)) {
            Ok(result) => {
                result?;
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let watched = Shared(installer.0.clone());
                let done =
                    vm.on_queue(move |_| unsafe { watched.0.progress().fractionCompleted() });
                say(&format!("installing macOS: {:.0}%", done * 100.0));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("the installer stopped without a result".into());
            }
        }
    }
    say("macOS installed");
    Ok(Build {
        machine,
        stage: "installed".into(),
    })
}

fn random_password() -> Result<String, Failure> {
    let mut bytes = [0u8; 18];
    std::io::Read::read_exact(&mut File::open("/dev/urandom")?, &mut bytes)?;
    Ok(base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        bytes,
    ))
}

fn provisioning(password: &str) -> Result<Retained<VZMacOSVirtualMachineStartOptions>, Failure> {
    let class = AnyClass::get(c"VZMacGuestProvisioningOptions")
        .ok_or("guest provisioning needs macOS 27 or later on the host")?;
    unsafe {
        let options: Retained<AnyObject> = msg_send![class, new];
        let _: () = msg_send![&*options, setFullName: &*NSString::from_str("Kumoba")];
        let _: () = msg_send![&*options, setUsername: &*NSString::from_str(USER)];
        let _: () = msg_send![&*options, setPassword: &*NSString::from_str(password)];
        let _: () = msg_send![&*options, setLogsInAutomatically: Bool::YES];
        let _: () = msg_send![&*options, setEnablesRemoteLogin: Bool::YES];
        let start = VZMacOSVirtualMachineStartOptions::new();
        let mut error: *mut NSError = std::ptr::null_mut();
        let accepted: Bool =
            msg_send![&*start, setGuestProvisioningOptions: &*options, error: &mut error];
        if !accepted.as_bool() {
            return Err(error
                .as_ref()
                .map_or_else(
                    || "guest provisioning was refused".into(),
                    |e| ns_error(e).message,
                )
                .into());
        }
        Ok(start)
    }
}

fn normalized(mac: &str) -> String {
    mac.split(':')
        .map(|part| part.trim_start_matches('0'))
        .map(|part| if part.is_empty() { "0" } else { part })
        .collect::<Vec<_>>()
        .join(":")
        .to_lowercase()
}

fn lease(mac: &str) -> Option<String> {
    lease_in(&std::fs::read_to_string("/var/db/dhcpd_leases").ok()?, mac)
}

fn lease_in(leases: &str, mac: &str) -> Option<String> {
    let wanted = normalized(mac);
    leases.split('}').find_map(|block| {
        let mut address = None;
        let mut hardware = None;
        for line in block.lines().map(str::trim) {
            if let Some(value) = line.strip_prefix("ip_address=") {
                address = Some(value.to_string());
            } else if let Some(value) = line.strip_prefix("hw_address=") {
                hardware = value.split_once(',').map(|(_, mac)| normalized(mac));
            }
        }
        (hardware.as_deref() == Some(wanted.as_str()))
            .then_some(address)
            .flatten()
    })
}

struct Ssh {
    address: String,
    askpass: PathBuf,
    password: String,
}

impl Ssh {
    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command
            .env("SSH_ASKPASS", &self.askpass)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("KUMOBA_SSH_PASSWORD", &self.password)
            .args([
                "-F",
                "/dev/null",
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "LogLevel=ERROR",
                "-o",
                "ConnectTimeout=5",
                "-o",
                "PubkeyAuthentication=no",
                "-o",
                "PreferredAuthentications=password,keyboard-interactive",
                "-o",
                "NumberOfPasswordPrompts=1",
            ])
            .stdin(Stdio::null());
        command
    }

    fn run(&self, remote: &str, input: Option<&str>) -> Result<bool, Failure> {
        let mut command = self.command("/usr/bin/ssh");
        command.arg(format!("{USER}@{}", self.address)).arg(remote);
        if input.is_some() {
            command.stdin(Stdio::piped());
        }
        let mut child = command.spawn()?;
        if let Some(input) = input {
            child
                .stdin
                .take()
                .ok_or("ssh stdin")?
                .write_all(input.as_bytes())?;
        }
        Ok(child.wait()?.success())
    }

    fn copy(&self, local: &Path, remote: &str) -> Result<(), Failure> {
        let status = self
            .command("/usr/bin/scp")
            .arg(local)
            .arg(format!("{USER}@{}:{remote}", self.address))
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("copying {} into the guest failed", local.display()).into())
        }
    }
}

const BOOTSTRAP: &str = r#"set -e
install -d -m 755 /usr/local/bin
install -m 755 -o root -g wheel /tmp/kumoba-guest /usr/local/bin/kumoba-guest
cat > /Library/LaunchDaemons/dev.kumoba.guest.plist <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>dev.kumoba.guest</string>
<key>ProgramArguments</key><array><string>/usr/local/bin/kumoba-guest</string></array>
<key>RunAtLoad</key><true/>
<key>KeepAlive</key><true/>
<key>StandardErrorPath</key><string>/var/log/kumoba-guest.log</string>
</dict></plist>
PLIST
chown root:wheel /Library/LaunchDaemons/dev.kumoba.guest.plist
chmod 644 /Library/LaunchDaemons/dev.kumoba.guest.plist
rm -f /tmp/kumoba-guest /tmp/kumoba-bootstrap.sh
launchctl bootstrap system /Library/LaunchDaemons/dev.kumoba.guest.plist 2>/dev/null || launchctl kickstart -k system/dev.kumoba.guest
"#;

const SETUP: &str = r#"set -e
mkdir -p /System/Volumes/Data/workspace
chown kumoba:staff /System/Volumes/Data/workspace
grep -q '^workspace' /etc/synthetic.conf 2>/dev/null || printf 'workspace\tSystem/Volumes/Data/workspace\n' >> /etc/synthetic.conf
printf '%s\n' 'Defaults env_keep += "HTTP_PROXY HTTPS_PROXY NO_PROXY http_proxy https_proxy no_proxy SSL_CERT_FILE CURL_CA_BUNDLE REQUESTS_CA_BUNDLE NODE_EXTRA_CA_CERTS"' 'kumoba ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/kumoba.new
chmod 440 /etc/sudoers.d/kumoba.new
visudo -cf /etc/sudoers.d/kumoba.new
mv /etc/sudoers.d/kumoba.new /etc/sudoers.d/kumoba
pmset -a sleep 0 displaysleep 0 disksleep 0 || true
mdutil -a -i off >/dev/null 2>&1 || true
softwareupdate --schedule off >/dev/null 2>&1 || true
defaults write /Library/Preferences/com.apple.SoftwareUpdate AutomaticDownload -bool false || true
defaults write /Library/Preferences/com.apple.SoftwareUpdate AutomaticallyInstallMacOSUpdates -bool false || true
sudo -u kumoba defaults -currentHost write com.apple.screensaver idleTime -int 0 || true
"#;

const IMAGE_CHECK: &str = "test -x /usr/local/bin/kumoba-guest && xcode-select -p >/dev/null && test -L /workspace && visudo -cf /etc/sudoers.d/kumoba >/dev/null && launchctl print-disabled system | grep -q '\"com.openssh.sshd\" => disabled' && ! pgrep -x sshd >/dev/null";

const FORGET_CLOCK: &str = "rm -f /var/db/timed/com.apple.timed.plist && (nvram -d com.apple.System.rtc-offset 2>/dev/null || true)";

const SSH_OFF: &str = "launchctl disable system/com.openssh.sshd && (launchctl bootout system/com.openssh.sshd 2>/dev/null || true) && launchctl print-disabled system | grep -q '\"com.openssh.sshd\" => disabled'";

fn guest_password(directory: &Path) -> Result<String, Failure> {
    let path = directory.join("password");
    if let Ok(existing) = std::fs::read_to_string(&path)
        && !existing.is_empty()
    {
        return Ok(existing);
    }
    let password = random_password()?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(password.as_bytes())?;
    file.sync_all()?;
    Ok(password)
}

const TOOLS: &str = r#"set -e
xcode-select -p >/dev/null 2>&1 && exit 0
marker=/tmp/.com.apple.dt.CommandLineTools.installondemand.in-progress
touch "$marker"
label=$(softwareupdate -l 2>/dev/null | grep -B 1 -E 'Command Line Tools' | awk -F'*' '/^ *\*/ {print $2}' | sed -e 's/^ *Label: //' -e 's/^ *//' | sort -V | tail -n 1)
if [ -z "$label" ]; then
  rm -f "$marker"
  echo "softwareupdate offers no Command Line Tools" >&2
  exit 1
fi
softwareupdate -i "$label"
rm -f "$marker"
xcode-select -p
"#;

fn provision(options: &Options, machine: &Machine) -> Result<(), Failure> {
    let password = guest_password(&options.out)?;
    let askpass = options.out.join(".askpass");
    std::fs::write(
        &askpass,
        "#!/bin/sh\nprintf '%s\\n' \"$KUMOBA_SSH_PASSWORD\"\n",
    )?;
    std::fs::set_permissions(&askpass, std::fs::Permissions::from_mode(0o700))?;

    let address = unsafe { VZMACAddress::randomLocallyAdministeredAddress() };
    let mac = unsafe { address.string() }.to_string();
    let (events, stopped) = mpsc::channel();
    let disk = options.out.join("root.raw");
    let vm = Vm::new(
        configuration(machine, &options.out, &disk, Some(&address))?,
        events,
        None,
    );
    let start = Shared(provisioning(&password)?);
    vm.call(move |machine, done| unsafe {
        machine.startWithOptions_completionHandler(&start.0, done);
    })?;
    say(&format!(
        "first boot; waiting for the guest ({mac}) to finish setup"
    ));
    let deadline = Instant::now() + Duration::from_secs(900);
    let mut agent = loop {
        if Instant::now() > deadline {
            return Err("the guest never became reachable over SSH or the agent".into());
        }
        if let Ok(Event::Stopped(error)) = stopped.try_recv() {
            return Err(format!("the guest stopped during setup: {error:?}").into());
        }
        if let Some(mut agent) = AgentSession::open(&vm) {
            say("the guest agent is already installed; updating it");
            agent.replace_agent(&options.agent)?;
            break agent;
        }
        if let Some(address) = lease(&mac) {
            let ssh = Ssh {
                address,
                askpass: askpass.clone(),
                password: password.clone(),
            };
            if ssh.run("true", None)? {
                say(&format!(
                    "guest reachable at {}; installing the agent",
                    ssh.address
                ));
                bootstrap(options, &ssh, &password)?;
                break wait_for_agent(&vm)?;
            }
        }
        std::thread::sleep(Duration::from_secs(5));
    };
    std::fs::remove_file(&askpass)?;
    say("configuring the guest");
    agent.run_checked(SETUP, "guest setup failed")?;
    say("installing Apple's Command Line Tools (git, clang, python3)");
    agent.run_checked(
        TOOLS,
        "Apple's Command Line Tools did not install; run the command again to retry",
    )?;
    agent.run_checked(SSH_OFF, "could not turn off SSH in the guest")?;
    if !agent.shutdown() {
        return Err("the agent did not accept the shutdown request".into());
    }
    say("waiting for the guest to shut down");
    match stopped.recv_timeout(Duration::from_secs(300)) {
        Ok(Event::Stopped(None)) => Ok(()),
        Ok(Event::Stopped(Some(error))) => Err(error.into()),
        _ => {
            let _ = vm.stop();
            Err("the guest did not shut down cleanly; run the command again".into())
        }
    }
}

fn bootstrap(options: &Options, ssh: &Ssh, password: &str) -> Result<(), Failure> {
    ssh.copy(&options.agent, "/tmp/kumoba-guest")?;
    let script = options.out.join(".bootstrap.sh");
    std::fs::write(&script, BOOTSTRAP)?;
    ssh.copy(&script, "/tmp/kumoba-bootstrap.sh")?;
    std::fs::remove_file(&script)?;
    if ssh.run(
        "sudo -S -p '' sh /tmp/kumoba-bootstrap.sh",
        Some(&format!("{password}\n")),
    )? {
        Ok(())
    } else {
        Err("installing the guest agent failed".into())
    }
}

fn wait_for_agent(vm: &Vm) -> Result<AgentSession, Failure> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Some(agent) = AgentSession::open(vm) {
            return Ok(agent);
        }
        if Instant::now() > deadline {
            return Err("the guest agent did not start after installation".into());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

struct AgentSession {
    runtime: tokio::runtime::Runtime,
    stream: tokio::net::UnixStream,
}

impl AgentSession {
    fn open(vm: &Vm) -> Option<Self> {
        let fd = vm.connect(AGENT_PORT).ok()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        let stream = runtime.block_on(async move {
            let stream = std::os::unix::net::UnixStream::from(fd);
            stream.set_nonblocking(true).ok()?;
            let mut stream = tokio::net::UnixStream::from_std(stream).ok()?;
            send(&mut stream, &Request::Hello).await.ok()?;
            let answer = tokio::time::timeout(Duration::from_secs(5), receive(&mut stream))
                .await
                .ok()?
                .ok()?;
            tokio::io::AsyncWriteExt::write_u8(&mut stream, 1)
                .await
                .ok()?;
            matches!(answer, Response::Hello { .. }).then_some(stream)
        })?;
        Some(Self { runtime, stream })
    }

    fn run(&mut self, script: &str) -> Result<Option<i32>, Failure> {
        let request = Request::Exec(kumoba_protocol::ExecRequest {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: "/".into(),
            env: [(
                "PATH".to_string(),
                "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".to_string(),
            )]
            .into(),
            stdin: String::new(),
            timeout_ms: 3_600_000,
            output_limit: 64 << 20,
            user: None,
        });
        let stream = &mut self.stream;
        self.runtime.block_on(async move {
            send(stream, &request).await?;
            loop {
                let event: kumoba_protocol::ExecEvent =
                    kumoba_protocol::receive_limited(stream, kumoba_protocol::MAX_EXEC_FRAME)
                        .await?;
                tokio::io::AsyncWriteExt::write_u8(stream, 1).await?;
                match event {
                    kumoba_protocol::ExecEvent::Output { data, .. } => {
                        let bytes = base64::Engine::decode(
                            &base64::engine::general_purpose::STANDARD,
                            data,
                        )?;
                        std::io::stderr().write_all(&bytes)?;
                    }
                    kumoba_protocol::ExecEvent::Finished { code, .. } => return Ok(code),
                    kumoba_protocol::ExecEvent::Failed { message } => {
                        return Err(message.into());
                    }
                }
            }
        })
    }

    fn write_at(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<(), Failure> {
        let request = Request::WriteAt {
            path: path.into(),
            offset,
            data: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, data),
            truncate: offset == 0,
            user: None,
        };
        let stream = &mut self.stream;
        let answer = self.runtime.block_on(async move {
            send(stream, &request).await?;
            let answer: Response = receive(stream).await?;
            tokio::io::AsyncWriteExt::write_u8(stream, 1).await?;
            Ok::<_, std::io::Error>(answer)
        })?;
        match answer {
            Response::Ok => Ok(()),
            Response::Error(message) => Err(message.into()),
            _ => Err("unexpected answer to a file write".into()),
        }
    }

    fn replace_agent(&mut self, agent: &Path) -> Result<(), Failure> {
        let staged = "/usr/local/bin/.kumoba-guest.new";
        let bytes = std::fs::read(agent)?;
        let mut offset = 0;
        for chunk in bytes.chunks(512 * 1024) {
            self.write_at(staged, offset, chunk)?;
            offset += chunk.len() as u64;
        }
        self.run_checked(
            &format!(
                "install -m 755 -o root -g wheel {staged} /usr/local/bin/kumoba-guest && rm -f {staged}"
            ),
            "could not replace the guest agent",
        )
    }

    fn run_checked(&mut self, script: &str, failure: &str) -> Result<(), Failure> {
        match self.run(script)? {
            Some(0) => Ok(()),
            _ => Err(failure.into()),
        }
    }

    fn shutdown(mut self) -> bool {
        let stream = &mut self.stream;
        self.runtime.block_on(async move {
            if send(stream, &Request::Shutdown).await.is_err() {
                return false;
            }
            let answer = tokio::time::timeout(Duration::from_secs(10), receive(stream)).await;
            let _ = tokio::io::AsyncWriteExt::write_u8(stream, 1).await;
            matches!(answer, Ok(Ok(Response::Ok)))
        })
    }
}

fn verify(options: &Options, machine: &Machine) -> Result<(), Failure> {
    let (events, stopped) = mpsc::channel();
    let disk = options.out.join("root.raw");
    let vm = Vm::new(
        configuration(machine, &options.out, &disk, None)?,
        events,
        None,
    );
    let began = Instant::now();
    vm.start()?;
    say("booting the image without a network card to check the agent");
    let mut agent = loop {
        if let Some(agent) = AgentSession::open(&vm) {
            break agent;
        }
        if began.elapsed() > Duration::from_secs(600) {
            return Err("the agent did not answer after boot".into());
        }
        if let Ok(Event::Stopped(error)) = stopped.try_recv() {
            return Err(format!("the guest stopped while booting: {error:?}").into());
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    say(&format!(
        "agent answered {:.0} s after boot; checking the image",
        began.elapsed().as_secs_f64()
    ));
    agent.run_checked(
        IMAGE_CHECK,
        "the image is incomplete; delete it and build again",
    )?;
    agent.run_checked(FORGET_CLOCK, "could not reset the guest clock state")?;
    if !agent.shutdown() {
        return Err("the agent did not accept the shutdown request".into());
    }
    match stopped.recv_timeout(Duration::from_secs(300)) {
        Ok(Event::Stopped(None)) => Ok(()),
        Ok(Event::Stopped(Some(error))) => Err(error.into()),
        _ => Err("the guest did not shut down".into()),
    }
}
