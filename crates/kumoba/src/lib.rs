mod agent;
pub use agent::Agent;
mod config;
mod mounts;
pub use mounts::validate_host_mounts;
pub use kumoba_protocol::HostMount;
mod platform;
pub use platform::HostPlatform;
mod desktop;
mod egress;
pub use egress::{AccessRecord, EgressConfig, PrivateAllow, UpstreamProxy};
mod execution;
mod public_http;
mod runtime;
mod secret_https;
mod secret_stream;
mod secrets;
mod session;
mod snapshot_image;
pub use snapshot_image::{SnapshotImage, SnapshotImageInfo};

pub use config::{
    AccessSink, Command, ExecOutput, ExecSummary, GuestPath, Network, RuntimeConfig, SandboxConfig, SandboxId,
    State, StopOutcome, Termination,
};
pub use runtime::{Runtime, Snapshot};
pub use session::{Sandbox, Tunnel};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("invalid configuration: {0}")]
    InvalidInput(String),
    #[error("state directory or sandbox is already owned by another handle or process")]
    InUse,
    #[error("sandbox is not running")]
    NotRunning,
    #[error("sandbox has been deleted")]
    Deleted,
    #[error("running sandbox capacity exhausted")]
    Capacity,
    #[error("sandbox has a pending disk operation")]
    Busy,
    #[error("boot failed: {0}")]
    Boot(String),
    #[error("guest operation failed: {0}")]
    Guest(String),
    #[error("guest connection failed; operation outcome may be unknown: {0}")]
    OutcomeUnknown(String),
    #[error("invalid guest protocol: {0}")]
    Protocol(String),
    #[error("snapshot requires a cleanly stopped sandbox")]
    NotClean,
    #[error("snapshot integrity check failed")]
    Integrity,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("background operation failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

pub use egress::{EgressEndpoint, HttpConnectPolicy};

pub use secrets::{SecretId, SecretMetadata, SecretStore, SecretValue};

#[cfg(test)]
mod secrets_vm_tests;

pub use desktop::{DesktopAction, DesktopConfig, DesktopInfo, DesktopScreenshot, MouseButton};
