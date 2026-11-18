#[cfg(any(target_os = "linux", target_os = "macos"))]
mod agent;
#[cfg(target_os = "linux")]
mod desktop;
#[cfg(target_os = "macos")]
#[path = "desktop_unavailable.rs"]
mod desktop;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod egress;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod execute;
#[cfg(any(target_os = "linux", target_os = "macos", test))]
mod files;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod shell;
#[cfg(any(target_os = "linux", test))]
mod shutdown;

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    linux::run().await
}

#[cfg(target_os = "macos")]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    macos::run().await
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn main() {
    eprintln!("kumoba-guest runs inside a Linux or macOS sandbox");
    std::process::exit(1);
}
