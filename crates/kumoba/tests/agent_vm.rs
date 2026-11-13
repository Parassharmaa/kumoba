use kumoba::{Command, Runtime, RuntimeConfig, SandboxConfig, Termination};
use std::{os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};

fn required(name: &str) -> PathBuf {
    std::env::var_os(name)
        .unwrap_or_else(|| panic!("set {name}; see scripts/test-vm.sh"))
        .into()
}

fn shell(script: &str) -> Command {
    Command::new("/bin/sh")
        .args(["-c", script])
        .timeout(Duration::from_secs(20))
}

#[tokio::test]
#[ignore = "requires a host worker, libkrun and a test image; run scripts/test-vm.sh"]
async fn agent_operations_survive_timeouts_aborts_and_run_concurrently()
-> Result<(), Box<dyn std::error::Error>> {
    let state = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()?;
    let runtime = Runtime::open(RuntimeConfig::for_host(
        required("KUMOBA_TEST_WORKER"),
        state.path(),
    )?)
    .await?;
    let mut config = SandboxConfig::new(required("KUMOBA_TEST_IMAGE"));
    config.vcpus = 2;
    config.memory_mib = 512;
    let mut sandbox = runtime.create(config).await?;
    sandbox.start().await?;
    let agent = sandbox.agent()?;

    let slow = agent.clone();
    let fast = agent.clone();
    let started = std::time::Instant::now();
    let (slow_out, fast_out) = tokio::join!(
        async move {
            let mut out = Vec::new();
            let mut err = Vec::new();
            let summary = slow
                .exec_streamed(shell("sleep 2; echo slow"), &mut out, &mut err)
                .await;
            (summary, out)
        },
        async move {
            let mut out = Vec::new();
            let mut err = Vec::new();
            let summary = fast
                .exec_streamed(shell("echo fast"), &mut out, &mut err)
                .await;
            (summary, out, started.elapsed())
        }
    );
    assert_eq!(slow_out.0?.termination, Termination::Exited(0));
    assert_eq!(slow_out.1, b"slow\n");
    let (fast_summary, fast_bytes, fast_elapsed) = fast_out;
    assert_eq!(fast_summary?.termination, Termination::Exited(0));
    assert_eq!(fast_bytes, b"fast\n");
    assert!(
        fast_elapsed < Duration::from_secs(2),
        "fast exec waited for slow exec"
    );

    let mut out = Vec::new();
    let mut err = Vec::new();
    let timed = agent
        .exec_streamed(
            Command::new("/bin/sh")
                .args(["-c", "echo $$ > /tmp/timed.pid; sleep 30"])
                .timeout(Duration::from_secs(1)),
            &mut out,
            &mut err,
        )
        .await?;
    assert_eq!(timed.termination, Termination::TimedOut);

    let aborted = agent.clone();
    let task = tokio::spawn(async move {
        let mut out = Vec::new();
        let mut err = Vec::new();
        aborted
            .exec_streamed(
                shell("echo $$ > /tmp/aborted.pid; sleep 30"),
                &mut out,
                &mut err,
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(800)).await;
    task.abort();
    let _ = task.await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut out = Vec::new();
    let mut err = Vec::new();
    let alive = agent
        .exec_streamed(
            shell("for f in /tmp/timed.pid /tmp/aborted.pid; do kill -0 $(cat $f) 2>/dev/null && echo alive || echo gone; done"),
            &mut out,
            &mut err,
        )
        .await?;
    assert_eq!(alive.termination, Termination::Exited(0));
    assert_eq!(String::from_utf8(out)?, "gone\ngone\n");

    let started = std::time::Instant::now();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let detached = agent
        .exec_streamed(
            shell("sleep 30 & echo $! > /tmp/bg.pid; echo started"),
            &mut out,
            &mut err,
        )
        .await?;
    assert_eq!(detached.termination, Termination::Exited(0));
    assert_eq!(out, b"started\n");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "exec waited for background child"
    );
    let mut out = Vec::new();
    let mut err = Vec::new();
    agent
        .exec_streamed(
            shell("kill -0 $(cat /tmp/bg.pid) && echo alive"),
            &mut out,
            &mut err,
        )
        .await?;
    assert_eq!(out, b"alive\n");

    let payload: Vec<u8> = (0..3 * 1024 * 1024 + 17)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    agent
        .write_all("/home/kumoba/deep/dir/data.bin", &payload, Some("kumoba"))
        .await?;
    let back = agent
        .read_all(
            "/home/kumoba/deep/dir/data.bin",
            16 * 1024 * 1024,
            Some("kumoba"),
        )
        .await?;
    assert_eq!(back, payload);
    let entries = agent.list("/home/kumoba/deep", Some("kumoba")).await?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "dir");

    let mut out = Vec::new();
    let mut err = Vec::new();
    let whoami = agent
        .exec_streamed(
            shell("id -un; echo $HOME; stat -c %U /home/kumoba/deep/dir/data.bin").user("kumoba"),
            &mut out,
            &mut err,
        )
        .await?;
    assert_eq!(whoami.termination, Termination::Exited(0));
    assert_eq!(String::from_utf8(out)?, "kumoba\n/home/kumoba\nkumoba\n");

    sandbox.delete().await?;
    Ok(())
}
