use kumoba::{
    Command, Error, GuestPath, Runtime, RuntimeConfig, SandboxConfig, State, StopOutcome,
    Termination,
};
use std::os::unix::fs::PermissionsExt;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn path(s: &str) -> GuestPath {
    GuestPath::new(s).unwrap()
}
fn required(name: &str) -> PathBuf {
    std::env::var_os(name)
        .unwrap_or_else(|| panic!("set {name} using scripts/test-local.sh"))
        .into()
}

#[tokio::test]
#[ignore = "requires host worker, libkrun and matching Linux image; run scripts/test-local.sh"]
async fn real_vm_lifecycle() -> Result<(), Box<dyn std::error::Error>> {
    let state = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()?
        .keep();
    eprintln!("test state: {}", state.display());
    let started = Instant::now();
    let runtime = Runtime::open(RuntimeConfig::new(
        required("KUMOBA_TEST_WORKER"),
        required("KUMOBA_TEST_LIBRARY"),
        required("KUMOBA_TEST_FIRMWARE"),
        &state,
    ))
    .await?;
    let mut config = SandboxConfig::new(required("KUMOBA_TEST_IMAGE"));
    config.vcpus = 2;
    config.memory_mib = 512;
    let mut sandbox = runtime.create(config.clone()).await?;
    assert_eq!(sandbox.state()?, State::Stopped);
    assert!(matches!(runtime.get(sandbox.id()).await, Err(Error::InUse)));
    if let Err(error) = sandbox.start().await {
        eprintln!("preserved failed test state at {}", state.display());
        return Err(error.into());
    }
    eprintln!("ready after {:?}", started.elapsed());
    let output = sandbox.exec(Command::new("/bin/uname").arg("-s")).await?;
    assert!(output.success());
    assert_eq!(output.stdout, b"Linux\n");
    let cpus = sandbox
        .exec(Command::new("/bin/sh").args(["-c", "nproc; grep MemTotal /proc/meminfo"]))
        .await?;
    assert!(cpus.success());
    assert!(String::from_utf8_lossy(&cpus.stdout).starts_with("2\n"));
    let memory = String::from_utf8_lossy(&cpus.stdout)
        .lines()
        .nth(1)
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse::<u64>()?;
    assert!((450_000..524_288).contains(&memory));
    eprintln!("guest resources: {}", String::from_utf8_lossy(&cpus.stdout));
    for i in 0..1000 {
        let result = sandbox
            .exec(Command::new("/bin/echo").arg(format!("repeat {i}")))
            .await?;
        assert_eq!(result.stdout, format!("repeat {i}\n").as_bytes());
    }
    let literal = "$(touch /workspace/unexpected); spaces 'quotes'";
    eprintln!("testing literal arguments");
    assert_eq!(
        sandbox
            .exec(Command::new("/bin/echo").arg(literal))
            .await?
            .stdout,
        format!("{literal}\n").as_bytes()
    );
    assert!(
        sandbox
            .exec(Command::new("/bin/sh").args(["-c", "test ! -e /workspace/unexpected"]))
            .await?
            .success()
    );
    eprintln!("testing stdout/stderr/environment");
    let result = sandbox
        .exec(
            Command::new("/bin/sh")
                .args(["-c", "printf '%s' \"$VALUE\"; printf err >&2; exit 7"])
                .env("VALUE", "literal value"),
        )
        .await?;
    assert_eq!(result.termination, Termination::Exited(7));
    assert_eq!(result.stdout, b"literal value");
    assert_eq!(result.stderr, b"err");
    let data = vec![0, 1, 255, 10, 0];
    sandbox
        .write_file(&path("/workspace/input.bin"), &data)
        .await?;
    assert_eq!(
        sandbox
            .read_file(&path("/workspace/input.bin"), 100)
            .await?,
        data
    );
    assert!(
        sandbox
            .read_file(&path("/workspace/input.bin"), 2)
            .await
            .is_err()
    );
    assert_eq!(
        sandbox
            .exec(Command::new("/bin/cat").stdin(b"stdin works".to_vec()))
            .await?
            .stdout,
        b"stdin works"
    );
    let output = sandbox
        .exec(
            Command::new("/bin/sh")
                .args([
                    "-c",
                    "head -c 200000 /dev/zero; head -c 200000 /dev/zero >&2",
                ])
                .output_limit(1024),
        )
        .await?;
    assert!(output.success() && output.truncated);
    assert_eq!(output.stdout.len(), 1024);
    assert_eq!(output.stderr.len(), 1024);
    sandbox
        .write_file(&path("/workspace/index.html"), b"persistent service")
        .await?;
    let service = sandbox.exec(Command::new("/bin/sh").args(["-c", "/usr/local/bin/http-fixture >/workspace/http.log 2>&1 & for i in 1 2 3 4 5; do test -f /workspace/http-ready && exit 0; sleep 0.1; done; exit 1"])).await?;
    assert!(service.success(), "service startup failed: {service:?}");
    assert!(
        sandbox
            .exec(Command::new("/bin/echo").arg("service still alive"))
            .await?
            .success()
    );
    let mut stream = sandbox.connect(8080).await?;
    stream
        .write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .await?;
    let mut reply = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut reply)).await??;
    assert!(String::from_utf8_lossy(&reply).contains("persistent service"));
    drop(stream);
    let host_home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    assert!(sandbox.exec(Command::new("/bin/sh").args(["-c",&format!("test ! -e {host_home} && test ! -e /var/run/docker.sock && ! wget -T 1 -q -O - http://169.254.169.254/")])).await?.success());
    assert_eq!(sandbox.stop().await?, StopOutcome::Graceful);
    let snapshot = sandbox.snapshot().await?;
    sandbox.start().await?;
    assert_eq!(
        sandbox
            .read_file(&path("/workspace/input.bin"), 100)
            .await?,
        data
    );
    assert!(sandbox.connect(8080).await.is_err());
    sandbox
        .write_file(&path("/workspace/input.bin"), b"new version")
        .await?;
    assert_eq!(sandbox.stop().await?, StopOutcome::Graceful);
    let mut restored = runtime.restore(&snapshot).await?;
    restored.start().await?;
    assert_eq!(
        restored
            .read_file(&path("/workspace/input.bin"), 100)
            .await?,
        data
    );
    let id = sandbox.id();
    drop(sandbox);
    let mut reopened = runtime.get(id).await?;
    reopened.start().await?;
    assert_eq!(
        reopened
            .read_file(&path("/workspace/input.bin"), 100)
            .await?,
        b"new version"
    );
    config.vcpus = 1;
    config.memory_mib = 256;
    let mut excess = runtime.create(config).await?;
    assert!(matches!(excess.start().await, Err(Error::Capacity)));
    let timed = reopened
        .exec(
            Command::new("/bin/sh")
                .args(["-c", "sleep 30"])
                .timeout(Duration::from_millis(100)),
        )
        .await?;
    assert_eq!(timed.termination, Termination::TimedOut);
    assert_eq!(reopened.state()?, State::Stopped);
    excess.start().await?;
    assert_eq!(
        excess
            .exec(Command::new("/bin/sh").args(["-c", "nproc"]))
            .await?
            .stdout,
        b"1\n"
    );
    excess.stop().await?;
    reopened.start().await?;
    let cancelled = tokio::time::timeout(
        Duration::from_millis(100),
        reopened.exec(Command::new("/bin/sleep").arg("30")),
    )
    .await;
    assert!(cancelled.is_err());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match excess.start().await {
            Ok(()) => break,
            Err(Error::Capacity) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            result => panic!("cancelled VM retained capacity: {result:?}"),
        }
    }
    assert!(matches!(reopened.snapshot().await, Err(Error::NotClean)));
    let pending=excess.exec(Command::new("/bin/sh").args(["-c", "(trap '' TERM; printf ready > /workspace/cancel-stop-ready; while :; do sleep 1; done) >/workspace/cancel-stop.log 2>&1 & for i in 1 2 3 4 5; do test -f /workspace/cancel-stop-ready && exit 0; sleep 0.02; done; exit 1"])).await?;
    assert!(pending.success());
    let cancelled_stop = tokio::time::timeout(Duration::from_millis(100), excess.stop()).await;
    assert!(cancelled_stop.is_err());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match reopened.start().await {
            Ok(()) => break,
            Err(Error::Capacity) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            result => panic!("cancelled stop retained capacity: {result:?}"),
        }
    }
    reopened.delete().await?;
    reopened.delete().await?;
    restored.delete().await?;
    excess.delete().await?;
    assert!(runtime.list().await?.is_empty());
    assert!(runtime.open_snapshot(state.join("snapshots")).is_err());
    runtime.delete_snapshot(&snapshot).await?;
    runtime.delete_snapshot(&snapshot).await?;
    drop(reopened);
    drop(restored);
    drop(excess);
    drop(runtime);
    std::fs::remove_dir_all(&state)?;
    eprintln!("complete real guest suite in {:?}", started.elapsed());
    Ok(())
}

#[tokio::test]
#[ignore = "requires local runtime; run scripts/test-local.sh"]
async fn owner_death_releases_worker_and_disk() -> Result<(), Box<dyn std::error::Error>> {
    use tokio::io::AsyncBufReadExt;
    let state = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()?;
    let mut owner = tokio::process::Command::new(required("KUMOBA_TEST_OWNER"))
        .arg(state.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdout = tokio::io::BufReader::new(owner.stdout.take().unwrap());
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(30), stdout.read_line(&mut line)).await??;
    let id = line.trim().parse()?;
    let children = std::process::Command::new("/usr/bin/pgrep")
        .args(["-P", &owner.id().unwrap().to_string()])
        .output()?;
    let pids = String::from_utf8(children.stdout)?
        .split_whitespace()
        .map(str::parse::<i32>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(pids.len(), 1, "owner should supervise exactly one worker");
    assert!(matches!(
        Runtime::open(RuntimeConfig::new(
            required("KUMOBA_TEST_WORKER"),
            required("KUMOBA_TEST_LIBRARY"),
            required("KUMOBA_TEST_FIRMWARE"),
            state.path(),
        ))
        .await,
        Err(Error::InUse)
    ));
    owner.kill().await?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while unsafe { libc::kill(pids[0], 0) } == 0 {
        assert!(Instant::now() < deadline, "worker outlived its owner");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let runtime = Runtime::open(RuntimeConfig::new(
        required("KUMOBA_TEST_WORKER"),
        required("KUMOBA_TEST_LIBRARY"),
        required("KUMOBA_TEST_FIRMWARE"),
        state.path(),
    ))
    .await?;
    let mut sandbox = runtime.get(id).await?;
    assert!(matches!(sandbox.snapshot().await, Err(Error::NotClean)));
    sandbox.start().await?;
    assert!(
        sandbox
            .exec(Command::new("/bin/echo").arg("recovered"))
            .await?
            .success()
    );
    sandbox.delete().await?;
    eprintln!("owner SIGKILL: worker exited, disk lock released, recovered and deleted");
    Ok(())
}

#[tokio::test]
#[ignore = "requires local runtime; run scripts/test-local.sh"]
async fn unexpected_worker_exit_releases_capacity() -> Result<(), Box<dyn std::error::Error>> {
    let state = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()?;
    let mut config = RuntimeConfig::new(
        required("KUMOBA_TEST_WORKER"),
        required("KUMOBA_TEST_LIBRARY"),
        required("KUMOBA_TEST_FIRMWARE"),
        state.path(),
    );
    config.max_running = 1;
    let runtime = Runtime::open(config).await?;
    let image = SandboxConfig::new(required("KUMOBA_TEST_IMAGE"));
    let mut first = runtime.create(image.clone()).await?;
    let mut second = runtime.create(image).await?;
    first.start().await?;
    let children = std::process::Command::new("/usr/bin/pgrep")
        .args(["-P", &std::process::id().to_string()])
        .output()?;
    let pids = String::from_utf8(children.stdout)?
        .split_whitespace()
        .map(str::parse::<i32>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(pids.len(), 1);
    assert_eq!(unsafe { libc::kill(pids[0], libc::SIGKILL) }, 0);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match second.start().await {
            Ok(()) => break,
            Err(Error::Capacity) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            result => panic!("dead VM retained capacity: {result:?}"),
        }
    }
    assert!(
        second
            .exec(Command::new("/bin/echo").arg("replacement"))
            .await?
            .success()
    );
    assert!(matches!(first.snapshot().await, Err(Error::NotClean)));
    first.delete().await?;
    second.delete().await?;
    eprintln!("unexpected worker SIGKILL: replacement started with dead handle retained");
    Ok(())
}

#[tokio::test]
#[ignore = "requires local runtime; run scripts/test-local.sh"]
async fn repeated_boot_and_exec() -> Result<(), Box<dyn std::error::Error>> {
    let state = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()?
        .keep();
    eprintln!("repeat state: {}", state.display());
    let runtime = Runtime::open(RuntimeConfig::new(
        required("KUMOBA_TEST_WORKER"),
        required("KUMOBA_TEST_LIBRARY"),
        required("KUMOBA_TEST_FIRMWARE"),
        &state,
    ))
    .await?;
    let mut config = SandboxConfig::new(required("KUMOBA_TEST_IMAGE"));
    config.vcpus = 2;
    let mut sandbox = runtime.create(config).await?;
    for boot in 0..20 {
        eprintln!("boot {boot}");
        sandbox.start().await?;
        for i in 0..50 {
            let result = sandbox
                .exec(Command::new("/bin/echo").arg(format!("{boot}/{i}")))
                .await?;
            assert_eq!(result.stdout, format!("{boot}/{i}\n").as_bytes());
        }
        sandbox.stop().await?;
    }
    sandbox.delete().await?;
    drop(sandbox);
    drop(runtime);
    std::fs::remove_dir_all(state)?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires real guest; scripts/test-local.sh"]
async fn shutdown_waits_for_handlers_and_kills_term_ignoring_jobs()
-> Result<(), Box<dyn std::error::Error>> {
    let state = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()?;
    let runtime = Runtime::open(RuntimeConfig::new(
        required("KUMOBA_TEST_WORKER"),
        required("KUMOBA_TEST_LIBRARY"),
        required("KUMOBA_TEST_FIRMWARE"),
        state.path(),
    ))
    .await?;
    let mut config = SandboxConfig::new(required("KUMOBA_TEST_IMAGE"));
    config.vcpus = 2;
    config.memory_mib = 512;
    let mut sandbox = runtime.create(config).await?;
    sandbox.start().await?;
    let handler=b"#!/bin/sh\ntrap 'sleep 0.12; printf done > /workspace/term-done; exit 0' TERM\nprintf ready > /workspace/handler-ready\nwhile :; do sleep 0.02; done\n";
    sandbox
        .write_file(&path("/workspace/handler.sh"), handler)
        .await?;
    let ready=sandbox.exec(Command::new("/bin/sh").args(["-c","/bin/sh /workspace/handler.sh >/workspace/handler.log 2>&1 & for i in 1 2 3 4 5; do test -f /workspace/handler-ready && exit 0; sleep 0.02; done; exit 1"])).await?;
    assert!(ready.success());
    assert_eq!(sandbox.stop().await?, StopOutcome::Graceful);
    sandbox.start().await?;
    assert_eq!(
        sandbox
            .read_file(&path("/workspace/term-done"), 100)
            .await?,
        b"done"
    );
    let ignoring=b"#!/bin/sh\ntrap '' TERM\nprintf ready > /workspace/ignoring-ready\nwhile :; do sleep 1; done\n";
    sandbox
        .write_file(&path("/workspace/ignoring.sh"), ignoring)
        .await?;
    let ready=sandbox.exec(Command::new("/bin/sh").args(["-c","/bin/sh /workspace/ignoring.sh >/workspace/ignoring.log 2>&1 & for i in 1 2 3 4 5; do test -f /workspace/ignoring-ready && exit 0; sleep 0.02; done; exit 1"])).await?;
    assert!(ready.success());
    let timer = Instant::now();
    assert_eq!(sandbox.stop().await?, StopOutcome::Graceful);
    assert!(
        timer.elapsed() >= Duration::from_millis(450),
        "SIGTERM grace was cut short"
    );
    let snapshot = sandbox.snapshot().await?;
    let mut restored = runtime.restore(&snapshot).await?;
    restored.start().await?;
    assert_eq!(
        restored
            .read_file(&path("/workspace/term-done"), 100)
            .await?,
        b"done"
    );
    restored.delete().await?;
    sandbox.delete().await?;
    runtime.delete_snapshot(&snapshot).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires real guest; scripts/test-local.sh"]
async fn shutdown_requires_verified_cleanup_before_marking_disk_clean()
-> Result<(), Box<dyn std::error::Error>> {
    let state = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()?;
    let runtime = Runtime::open(RuntimeConfig::new(
        required("KUMOBA_TEST_WORKER"),
        required("KUMOBA_TEST_LIBRARY"),
        required("KUMOBA_TEST_FIRMWARE"),
        state.path(),
    ))
    .await?;
    let mut sandbox = runtime
        .create(SandboxConfig::new(required("KUMOBA_TEST_IMAGE")))
        .await?;
    sandbox.start().await?;
    let output = sandbox
        .exec(Command::new("/bin/umount").arg("/proc"))
        .await?;
    assert!(
        output.success(),
        "failed to inject unavailable process table: {output:?}"
    );
    assert_eq!(sandbox.stop().await?, StopOutcome::Forced);
    assert!(matches!(sandbox.snapshot().await, Err(Error::NotClean)));
    sandbox.start().await?;
    assert_eq!(sandbox.stop().await?, StopOutcome::Graceful);
    sandbox.delete().await?;
    Ok(())
}
