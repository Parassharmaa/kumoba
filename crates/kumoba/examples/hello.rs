use kumoba::{Command, Runtime, RuntimeConfig, SandboxConfig};
use std::time::Instant;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(worker), Some(image)) = (args.next(), args.next()) else {
        eprintln!("usage: hello <path/to/kumoba-worker> <path/to/image.raw>");
        std::process::exit(2);
    };
    let state = tempfile::tempdir()?;
    let runtime = Runtime::open(RuntimeConfig::for_host(&worker, state.path().join("runtime"))?).await?;

    let mut config = SandboxConfig::new(image);
    config.vcpus = 1;
    config.memory_mib = 512;
    let started = Instant::now();
    let mut sandbox = runtime.create(config).await?;
    sandbox.start().await?;
    println!("booted in {:?}", started.elapsed());

    let output = sandbox
        .exec(Command::new("/bin/sh").args(["-c", "uname -a; echo hello from pid $$"]))
        .await?;
    print!("{}", String::from_utf8_lossy(&output.stdout));
    println!("exit: {}", output.success());

    sandbox.stop().await?;
    sandbox.delete().await?;
    Ok(())
}
