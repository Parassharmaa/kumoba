#[cfg(target_os = "linux")]
mod desktop;
#[cfg(target_os = "linux")]
mod egress;
#[cfg(target_os = "linux")]
mod execute;
#[cfg(any(target_os = "linux", test))]
mod files;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(any(target_os = "linux", test))]
mod shutdown;

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    linux::run().await
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("kumoba-guest must be built for Linux and run inside a VM");
    std::process::exit(1);
}
