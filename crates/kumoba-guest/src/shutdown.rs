use std::io;

const PF_KTHREAD: u64 = 0x0020_0000;
struct Process {
    pid: i32,
    start: u64,
    live: bool,
}
fn parse(pid: i32, stat: &str) -> io::Result<Process> {
    let fields = stat
        .rsplit_once(')')
        .ok_or_else(|| io::Error::other("invalid process stat"))?
        .1;
    let fields: Vec<_> = fields.split_whitespace().collect();
    let state = fields
        .first()
        .ok_or_else(|| io::Error::other("missing process state"))?;
    let flags = fields
        .get(6)
        .and_then(|f| f.parse::<u64>().ok())
        .ok_or_else(|| io::Error::other("missing process flags"))?;
    let start = fields
        .get(19)
        .and_then(|f| f.parse::<u64>().ok())
        .ok_or_else(|| io::Error::other("missing process start time"))?;
    Ok(Process {
        pid,
        start,
        live: !matches!(*state, "Z" | "X") && flags & PF_KTHREAD == 0,
    })
}
#[cfg(target_os = "linux")]
fn processes() -> io::Result<Vec<Process>> {
    let mut result = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<i32>().ok())
        else {
            continue;
        };
        match std::fs::read_to_string(entry.path().join("stat")) {
            Ok(stat) => result.push(parse(pid, &stat)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    if !result.iter().any(|p| p.pid == std::process::id() as i32) {
        return Err(io::Error::other("proc did not expose the guest agent"));
    }
    Ok(result)
}
#[cfg(target_os = "linux")]
pub(crate) enum Scope {
    Workloads(Vec<(i32, u64)>),
    BrowserHome(Vec<u8>),
}
#[cfg(target_os = "linux")]
impl Scope {
    pub(crate) fn capture() -> io::Result<Self> {
        Ok(Self::Workloads(
            processes()?
                .into_iter()
                .filter(|p| p.live)
                .map(|p| (p.pid, p.start))
                .collect(),
        ))
    }
    pub(crate) fn browser_session(home: &std::path::Path) -> Self {
        use std::os::unix::ffi::OsStrExt;
        let mut marker = b"HOME=".to_vec();
        marker.extend_from_slice(home.as_os_str().as_bytes());
        Self::BrowserHome(marker)
    }
    fn workloads(&self) -> io::Result<Vec<Process>> {
        let mut selected = Vec::new();
        for process in processes()?.into_iter().filter(|p| p.live) {
            let owns = match self {
                Self::Workloads(bootstrap) => !bootstrap.contains(&(process.pid, process.start)),
                Self::BrowserHome(marker) => {
                    match std::fs::read(format!("/proc/{}/environ", process.pid)) {
                        Ok(environment) => {
                            environment.split(|b| *b == 0).any(|entry| entry == marker)
                        }
                        Err(error)
                            if matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ESRCH)) =>
                        {
                            false
                        }
                        Err(error) => return Err(error),
                    }
                }
            };
            if owns {
                selected.push(process);
            }
        }
        Ok(selected)
    }
    fn signal(&self, signal: i32) -> io::Result<()> {
        use std::os::fd::{AsRawFd, FromRawFd};
        for process in self.workloads()? {
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, process.pid, 0) } as i32;
            if fd < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    continue;
                }
                return Err(error);
            }
            let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
            let current = match std::fs::read_to_string(format!("/proc/{}/stat", process.pid)) {
                Ok(stat) => parse(process.pid, &stat)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if current.start != process.start {
                continue;
            }
            if unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    fd.as_raw_fd(),
                    signal,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            } < 0
            {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
        }
        Ok(())
    }
    async fn wait_for_exit(&self, budget: std::time::Duration) -> io::Result<bool> {
        let deadline = tokio::time::Instant::now() + budget;
        loop {
            if self.workloads()?.is_empty() {
                return Ok(true);
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(false);
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
    pub(crate) async fn quiesce(&self) -> io::Result<()> {
        self.signal(libc::SIGTERM)?;
        self.wait_for_exit(std::time::Duration::from_millis(500))
            .await?;
        self.signal(libc::SIGKILL)?;
        if !self
            .wait_for_exit(std::time::Duration::from_secs(1))
            .await?
        {
            return Err(io::Error::other("guest processes did not exit before sync"));
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn stat(name: &str, state: &str, flags: u64) -> String {
        format!("42 ({name}) {state} 1 2 3 4 5 {flags} 0 0 0 0 0 0 0 0 0 0 0 0 123 0")
    }
    #[test]
    fn identifies_live_userspace_without_trusting_process_names() {
        let process = parse(42, &stat("a ) Z 0 ( b\n", "S", 4194304)).unwrap();
        assert!(process.live);
        assert_eq!((process.pid, process.start), (42, 123));
        assert!(!parse(42, &stat("zombie", "Z", 0)).unwrap().live);
        assert!(!parse(42, &stat("kernel", "I", PF_KTHREAD)).unwrap().live);
        assert!(parse(42, &stat("io wait", "D", 0)).unwrap().live);
        assert!(parse(42, "bad stat").is_err());
    }
}
