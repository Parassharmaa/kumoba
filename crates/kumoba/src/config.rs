use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub worker: PathBuf,
    pub library: PathBuf,
    pub firmware: PathBuf,
    pub state_dir: PathBuf,
    pub max_running: usize,
    pub boot_timeout: Duration,
    pub stop_timeout: Duration,
    pub secret_store: Option<crate::SecretStore>,
    pub access_sink: Option<AccessSink>,
}

#[derive(Clone)]
pub struct AccessSink(Arc<dyn Fn(SandboxId, crate::AccessRecord) + Send + Sync>);

impl AccessSink {
    pub fn new(sink: impl Fn(SandboxId, crate::AccessRecord) + Send + Sync + 'static) -> Self {
        Self(Arc::new(sink))
    }
    pub(crate) fn for_sandbox(&self, id: SandboxId) -> crate::egress::AccessSink {
        let sink = self.0.clone();
        Arc::new(move |record| sink(id, record))
    }
}

impl std::fmt::Debug for AccessSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccessSink")
    }
}

impl RuntimeConfig {
    pub fn new(
        worker: impl Into<PathBuf>,
        library: impl Into<PathBuf>,
        firmware: impl Into<PathBuf>,
        state_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            worker: worker.into(),
            library: library.into(),
            firmware: firmware.into(),
            state_dir: state_dir.into(),
            max_running: 2,
            boot_timeout: Duration::from_secs(30),
            stop_timeout: Duration::from_secs(10),
            secret_store: None,
            access_sink: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum Network {
    #[default]
    Disabled,
    HttpConnect(crate::egress::HttpConnectPolicy),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    pub image: PathBuf,
    pub vcpus: u8,
    pub memory_mib: u32,
    pub network: Network,
    #[serde(default)]
    pub secrets: BTreeMap<String, crate::SecretId>,
    #[serde(default)]
    pub mounts: Vec<crate::HostMount>,
}
impl SandboxConfig {
    pub fn new(image: impl Into<PathBuf>) -> Self {
        Self {
            image: image.into(),
            vcpus: 1,
            memory_mib: 512,
            network: Network::Disabled,
            secrets: BTreeMap::new(),
            mounts: Vec::new(),
        }
    }
    pub(crate) fn validate(&self) -> Result<()> {
        crate::mounts::validate_host_mounts(&self.mounts)?;
        if !self.secrets.is_empty() && matches!(self.network, Network::Disabled) {
            return Err(Error::InvalidInput(
                "network secrets require an HTTPS proxy policy".into(),
            ));
        }
        if !(1..=8).contains(&self.vcpus) || !(256..=16384).contains(&self.memory_mib) {
            return Err(Error::InvalidInput(
                "vcpus must be 1..=8 and memory_mib 256..=16384".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct SandboxId(pub(crate) uuid::Uuid);
impl std::fmt::Display for SandboxId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::str::FromStr for SandboxId {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        uuid::Uuid::parse_str(s)
            .map(Self)
            .map_err(|e| Error::InvalidInput(e.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestPath(String);
impl GuestPath {
    pub fn new(path: impl Into<String>) -> Result<Self> {
        let path = path.into();
        if !path.starts_with('/')
            || path.len() > 4096
            || path.contains('\0')
            || path.split('/').any(|p| p == "..")
        {
            return Err(Error::InvalidInput(
                "guest path must be absolute without NUL or '..' components".into(),
            ));
        }
        Ok(Self(path))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub struct Command {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) cwd: String,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) stdin: Vec<u8>,
    pub(crate) timeout: Duration,
    pub(crate) output_limit: usize,
    pub(crate) user: Option<String>,
}
impl Command {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: "/workspace".into(),
            env: BTreeMap::from([(
                "PATH".into(),
                "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".into(),
            )]),
            stdin: Vec::new(),
            timeout: Duration::from_secs(30),
            output_limit: 64 * 1024,
            user: None,
        }
    }
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }
    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }
    pub fn cwd(mut self, path: GuestPath) -> Self {
        self.cwd = path.0;
        self
    }
    pub fn env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(name.into(), value.into());
        self
    }
    pub fn stdin(mut self, data: impl Into<Vec<u8>>) -> Self {
        self.stdin = data.into();
        self
    }
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    pub fn output_limit(mut self, bytes: usize) -> Self {
        self.output_limit = bytes;
        self
    }
    pub fn user(mut self, name: impl Into<String>) -> Self {
        self.user = Some(name.into());
        self
    }
    pub fn timeout_duration(&self) -> Duration {
        self.timeout
    }
    pub(crate) fn into_request(self) -> kumoba_protocol::ExecRequest {
        use base64::Engine;
        kumoba_protocol::ExecRequest {
            program: self.program,
            args: self.args,
            cwd: self.cwd,
            env: self.env,
            stdin: base64::engine::general_purpose::STANDARD.encode(self.stdin),
            timeout_ms: self.timeout.as_millis() as u64,
            output_limit: self.output_limit,
            user: self.user,
        }
    }
    pub(crate) fn validate(&self) -> Result<()> {
        GuestPath::new(&self.program)?;
        if self.timeout.as_millis() == 0
            || self.timeout > Duration::from_secs(3600)
            || self.output_limit == 0
            || self.output_limit > kumoba_protocol::MAX_OUTPUT_LIMIT
            || self.stdin.len() > kumoba_protocol::MAX_DATA
        {
            return Err(Error::InvalidInput(
                "invalid timeout, input or output limit".into(),
            ));
        }
        if self.args.len() > 1024
            || self.args.iter().any(|x| x.contains('\0'))
            || self
                .env
                .iter()
                .any(|(k, v)| k.is_empty() || k.contains(['=', '\0']) || v.contains('\0'))
            || self.args.iter().map(String::len).sum::<usize>()
                + self
                    .env
                    .iter()
                    .map(|(k, v)| k.len() + v.len())
                    .sum::<usize>()
                > 128 * 1024
        {
            return Err(Error::InvalidInput(
                "invalid or oversized arguments/environment".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Stopped,
    Running,
    Deleted,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    AlreadyStopped,
    Graceful,
    Forced,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Termination {
    Exited(i32),
    Signaled(i32),
    TimedOut,
}
#[derive(Debug)]
pub struct ExecOutput {
    pub termination: Termination,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub truncated: bool,
}
impl ExecOutput {
    pub fn success(&self) -> bool {
        self.termination == Termination::Exited(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_boundaries() {
        for p in ["../host", "relative", "/a/../b", "/a\0b"] {
            assert!(GuestPath::new(p).is_err());
        }
        assert!(
            Command::new("/bin/echo")
                .arg("a; $(touch bad)")
                .validate()
                .is_ok()
        );
        assert!(Command::new("echo").validate().is_err());
        assert!(
            Command::new("/bin/echo")
                .env("A=B", "x")
                .validate()
                .is_err()
        );
        assert!(
            Command::new("/bin/echo")
                .timeout(Duration::ZERO)
                .validate()
                .is_err()
        );
        let mut config = SandboxConfig::new("base.raw");
        config.vcpus = 0;
        assert!(config.validate().is_err());
    }
}

#[derive(Debug)]
pub struct ExecSummary {
    pub termination: Termination,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub truncated: bool,
}
impl ExecSummary {
    pub fn success(&self) -> bool {
        self.termination == Termination::Exited(0)
    }
}
