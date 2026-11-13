use crate::{Error, Result, RuntimeConfig};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostPlatform {
    MacOsArm64,
    LinuxArm64,
    LinuxX86_64,
}
impl HostPlatform {
    pub fn current() -> Result<Self> {
        match (std::env::consts::OS,std::env::consts::ARCH) {
            ("macos","aarch64") => Ok(Self::MacOsArm64),
            ("linux","aarch64") => Ok(Self::LinuxArm64),
            ("linux","x86_64") => Ok(Self::LinuxX86_64),
            _ => Err(Error::InvalidInput("supported hosts are macOS/ARM64 and Linux/ARM64 or x86_64; Windows requires the experimental WSL2 Linux route".into())),
        }
    }
    pub fn image_arch(self) -> &'static str {
        match self {
            Self::MacOsArm64 | Self::LinuxArm64 => "arm64",
            Self::LinuxX86_64 => "x86_64",
        }
    }
    pub fn guest_target(self) -> &'static str {
        match self {
            Self::MacOsArm64 | Self::LinuxArm64 => "aarch64-unknown-linux-musl",
            Self::LinuxX86_64 => "x86_64-unknown-linux-musl",
        }
    }
    fn libraries(self) -> (Vec<PathBuf>, Vec<PathBuf>) {
        match self {
            Self::MacOsArm64 => (
                vec![
                    "/opt/homebrew/opt/libkrun/lib/libkrun.1.dylib".into(),
                    "/opt/homebrew/opt/libkrun/lib/libkrun.dylib".into(),
                ],
                vec!["/opt/homebrew/opt/libkrunfw/lib/libkrunfw.5.dylib".into()],
            ),
            Self::LinuxArm64 | Self::LinuxX86_64 => {
                let arch = if self == Self::LinuxArm64 {
                    "aarch64"
                } else {
                    "x86_64"
                };
                let dirs = [
                    "/usr/local/lib64".to_owned(),
                    "/usr/local/lib".to_owned(),
                    "/usr/lib64".to_owned(),
                    format!("/usr/local/lib/{arch}-linux-gnu"),
                    format!("/usr/lib/{arch}-linux-gnu"),
                    "/usr/lib".to_owned(),
                ];
                (
                    dirs.iter()
                        .map(|d| Path::new(d).join("libkrun.so.1"))
                        .collect(),
                    dirs.iter()
                        .map(|d| Path::new(d).join("libkrunfw.so.5"))
                        .collect(),
                )
            }
        }
    }
}
fn find_library(variable: &str, candidates: Vec<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(variable) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(Error::InvalidInput(format!(
            "{variable} must name an existing trusted library file"
        )));
    }
    candidates.into_iter().find(|p|p.is_file()).ok_or_else(||Error::InvalidInput(format!("required host library not found; install libkrun 1.19.6/libkrunfw 5.6.2 or set {variable}")))
}
fn beside_executable(mut candidates: Vec<PathBuf>, names: &[&str]) -> Vec<PathBuf> {
    let bundled = std::env::current_exe().ok().and_then(|exe| {
        exe.parent()
            .and_then(Path::parent)
            .map(|dir| dir.join("lib"))
    });
    if let Some(dir) = bundled {
        let mut first: Vec<PathBuf> = names.iter().map(|name| dir.join(name)).collect();
        first.append(&mut candidates);
        candidates = first;
    }
    candidates
}
impl RuntimeConfig {
    pub fn for_host(worker: impl Into<PathBuf>, state_dir: impl Into<PathBuf>) -> Result<Self> {
        let platform = HostPlatform::current()?;
        let (libraries, firmware) = platform.libraries();
        let (libraries, firmware) = match platform {
            HostPlatform::MacOsArm64 => (
                beside_executable(libraries, &["libkrun.1.dylib"]),
                beside_executable(firmware, &["libkrunfw.5.dylib"]),
            ),
            HostPlatform::LinuxArm64 | HostPlatform::LinuxX86_64 => (
                beside_executable(libraries, &["libkrun.so.1"]),
                beside_executable(firmware, &["libkrunfw.so.5"]),
            ),
        };
        Ok(Self::new(
            worker,
            find_library("KUMOBAD_LIBKRUN", libraries)?,
            find_library("KUMOBAD_LIBKRUNFW", firmware)?,
            state_dir,
        ))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guest_architecture_and_host_library_names() {
        assert_eq!(
            HostPlatform::LinuxX86_64.guest_target(),
            "x86_64-unknown-linux-musl"
        );
        assert_eq!(
            HostPlatform::LinuxArm64.image_arch(),
            HostPlatform::MacOsArm64.image_arch()
        );
        for p in [HostPlatform::LinuxArm64, HostPlatform::LinuxX86_64] {
            let (lib, fw) = p.libraries();
            assert!(lib.iter().all(|p| p.ends_with("libkrun.so.1")));
            assert!(fw.iter().all(|p| p.ends_with("libkrunfw.so.5")));
        }
    }
}
