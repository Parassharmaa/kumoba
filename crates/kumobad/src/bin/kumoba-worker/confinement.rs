use kumoba_protocol::WorkerConfig;

#[cfg(target_os = "macos")]
pub fn apply(config: &WorkerConfig) -> Result<(), Box<dyn std::error::Error>> {
    use std::ffi::{CStr, CString, c_char};
    unsafe extern "C" {
        fn sandbox_init(profile: *const c_char, flags: u64, error: *mut *mut c_char) -> i32;
        fn sandbox_free_error(error: *mut c_char);
    }
    let profile = CString::new(profile(config)?)?;
    let mut error = std::ptr::null_mut();
    if unsafe { sandbox_init(profile.as_ptr(), 0, &mut error) } != 0 {
        let message = if error.is_null() {
            "sandbox_init failed".into()
        } else {
            unsafe { CStr::from_ptr(error) }
                .to_string_lossy()
                .into_owned()
        };
        if !error.is_null() {
            unsafe { sandbox_free_error(error) }
        };
        return Err(message.into());
    }
    Ok(())
}
#[cfg(not(target_os = "macos"))]
pub fn apply(_: &WorkerConfig) -> Result<(), Box<dyn std::error::Error>> {
    Err("host mounts are disabled without a supported host filesystem confinement backend".into())
}
#[cfg(target_os = "macos")]
fn quoted(path: &std::path::Path) -> Result<String, Box<dyn std::error::Error>> {
    let value = path.to_str().ok_or("mount path must be UTF-8")?;
    if value.chars().any(char::is_control) {
        return Err("control characters in mount path".into());
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}
#[cfg(target_os = "macos")]
fn profile(config: &WorkerConfig) -> Result<String, Box<dyn std::error::Error>> {
    let mut read = vec![
        "(subpath \"/System\")".into(),
        "(subpath \"/usr/lib\")".into(),
        "(subpath \"/private/var/db/dyld\")".into(),
        "(literal \"/dev/urandom\")".into(),
        "(literal \"/dev/random\")".into(),
        "(literal \"/dev/null\")".into(),
    ];
    let mut write = vec!["(literal \"/dev/null\")".into()];
    for path in [&config.library, &config.firmware, &config.disk] {
        read.push(format!("(literal {})", quoted(path)?));
    }
    write.push(format!("(literal {})", quoted(&config.disk)?));
    let sockets = std::fs::canonicalize(config.socket.parent().ok_or("socket directory missing")?)?;
    read.push(format!("(subpath {})", quoted(&sockets)?));
    write.push(format!("(subpath {})", quoted(&sockets)?));
    for mount in &config.mounts {
        let source = std::fs::canonicalize(&mount.source)?;
        if source != mount.source || !source.is_dir() {
            return Err("mount directory changed".into());
        }
        read.push(format!("(subpath {})", quoted(&source)?));
        if !mount.read_only {
            write.push(format!("(subpath {})", quoted(&source)?));
        }
    }
    Ok(format!(
        "(version 1)\n(allow default)\n(deny file-read-data (require-not (require-any {})))\n(deny file-write* (require-not (require-any {})))\n",
        read.join(" "),
        write.join(" ")
    ))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use kumoba_protocol::HostMount;
    fn config(root: &std::path::Path, read_only: bool) -> WorkerConfig {
        WorkerConfig {
            version: 2,
            library: "/bin/sh".into(),
            firmware: "/usr/bin/true".into(),
            disk: root.join("root.raw"),
            socket: root.join("sockets/agent.sock"),
            egress_socket: None,
            console: "/dev/null".into(),
            vcpus: 1,
            memory_mib: 512,
            lock_fd: 3,
            mounts: vec![HostMount {
                source: root.join("share"),
                target: "/workspace/repo".into(),
                read_only,
            }],
        }
    }
    #[test]
    fn confinement_child() {
        let Some(root) = std::env::var_os("KUMOBA_CONFINEMENT_TEST_DIR") else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        let read_only = std::env::var_os("KUMOBA_CONFINEMENT_TEST_READ_ONLY").is_some();
        apply(&config(&root, read_only)).unwrap();
        assert_eq!(std::fs::read(root.join("share/input")).unwrap(), b"inside");
        let write = std::fs::write(root.join("share/output"), b"inside write");
        if read_only {
            assert!(write.is_err());
        } else {
            write.unwrap();
        }
        assert!(std::fs::read(root.join("outside/secret")).is_err());
        assert!(std::fs::write(root.join("outside/bad"), b"denied").is_err());
        assert!(std::fs::read(root.join("share/escape/secret")).is_err());
        assert!(std::fs::write(root.join("share/escape/bad"), b"denied").is_err());
    }
    #[test]
    fn seatbelt_rejects_reads_writes_and_symlinks_outside_grants() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        for name in ["share", "outside", "sockets"] {
            std::fs::create_dir(root.join(name)).unwrap();
        }
        std::fs::write(root.join("share/input"), b"inside").unwrap();
        std::fs::write(root.join("outside/secret"), b"must not be readable").unwrap();
        std::os::unix::fs::symlink(root.join("outside"), root.join("share/escape")).unwrap();
        for read_only in [false, true] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "confinement::tests::confinement_child",
                    "--nocapture",
                ])
                .env("KUMOBA_CONFINEMENT_TEST_DIR", &root);
            if read_only {
                command.env("KUMOBA_CONFINEMENT_TEST_READ_ONLY", "1");
            }
            let result = command.output().unwrap();
            assert!(
                result.status.success(),
                "{:?}: {} {}",
                result.status,
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
        }
        assert!(!root.join("outside/bad").exists());
    }
}
