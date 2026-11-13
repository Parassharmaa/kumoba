use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum NetworkMode {
    #[default]
    Open,
    Allowlist,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NetworkSpec {
    #[serde(default)]
    #[schemars(
        description = "open reaches the public internet, allowlist only the listed hosts, blocked nothing"
    )]
    pub mode: NetworkMode,
    #[serde(default)]
    #[schemars(description = "Hosts an allowlist permits; *.example.com matches subdomains")]
    pub hosts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateRequest {
    #[schemars(description = "Image to boot, one of the names `images` lists")]
    pub image: String,
    #[serde(default)]
    #[schemars(description = "Free-form key=value labels to find the sandbox by later")]
    pub labels: BTreeMap<String, String>,
    #[serde(default = "default_cpu")]
    #[schemars(description = "Virtual CPUs")]
    pub cpu: u8,
    #[serde(default = "default_memory")]
    #[schemars(description = "Memory in MiB")]
    pub memory_mib: u32,
    #[serde(default)]
    #[schemars(description = "Stop after this many idle minutes; 0 never stops")]
    pub auto_stop_minutes: u32,
    #[serde(default)]
    #[schemars(description = "Delete this many minutes after stopping")]
    pub auto_delete_minutes: Option<u32>,
    #[serde(default)]
    #[schemars(description = "Outbound network policy")]
    pub network: NetworkSpec,
}

fn default_cpu() -> u8 {
    2
}

fn default_memory() -> u32 {
    4096
}

fn default_image() -> String {
    "default".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateRequest {
    #[serde(default)]
    #[schemars(description = "Replace the labels")]
    pub labels: Option<BTreeMap<String, String>>,
    #[serde(default)]
    #[schemars(description = "Replace the network policy; the sandbox must be stopped")]
    pub network: Option<NetworkSpec>,
    #[serde(default)]
    #[schemars(description = "Stop after this many idle minutes; 0 never stops")]
    pub auto_stop_minutes: Option<u32>,
    #[serde(default)]
    #[schemars(description = "Delete this many minutes after stopping; null keeps it")]
    pub auto_delete_minutes: Option<Option<u32>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    pub id: String,
    pub image: String,
    pub labels: BTreeMap<String, String>,
    pub cpu: u8,
    pub memory_mib: u32,
    pub auto_stop_minutes: u32,
    pub auto_delete_minutes: Option<u32>,
    pub network: NetworkSpec,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    #[serde(flatten)]
    pub meta: Meta,
    pub state: &'static str,
    pub last_activity_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecRequest {
    #[schemars(description = "Program and arguments, run without a shell")]
    pub argv: Vec<String>,
    #[serde(default)]
    #[schemars(description = "Working directory inside the guest")]
    pub cwd: Option<String>,
    #[serde(default)]
    #[schemars(description = "Extra environment variables")]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_timeout")]
    #[schemars(description = "Kill the command after this many milliseconds")]
    pub timeout_ms: u64,
    #[serde(default)]
    #[schemars(description = "Guest user to run as")]
    pub user: Option<String>,
}

fn default_timeout() -> u64 {
    600_000
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunRequest {
    #[serde(default = "default_image")]
    #[schemars(description = "Image to boot, one of the names `images` lists")]
    pub image: String,
    #[schemars(description = "Program and arguments, run without a shell")]
    pub argv: Vec<String>,
    #[serde(default)]
    #[schemars(description = "Working directory inside the guest")]
    pub cwd: Option<String>,
    #[serde(default)]
    #[schemars(description = "Extra environment variables")]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_timeout")]
    #[schemars(description = "Kill the command after this many milliseconds")]
    pub timeout_ms: u64,
    #[serde(default)]
    #[schemars(description = "Guest user to run as")]
    pub user: Option<String>,
    #[serde(default = "default_cpu")]
    #[schemars(description = "Virtual CPUs")]
    pub cpu: u8,
    #[serde(default = "default_memory")]
    #[schemars(description = "Memory in MiB")]
    pub memory_mib: u32,
    #[serde(default)]
    #[schemars(description = "Outbound network policy")]
    pub network: NetworkSpec,
    #[serde(default)]
    #[schemars(description = "Free-form key=value labels for the temporary sandbox")]
    pub labels: BTreeMap<String, String>,
}

impl RunRequest {
    pub fn split(self) -> (CreateRequest, ExecRequest) {
        (
            CreateRequest {
                image: self.image,
                labels: self.labels,
                cpu: self.cpu,
                memory_mib: self.memory_mib,
                auto_stop_minutes: 0,
                auto_delete_minutes: None,
                network: self.network,
            },
            ExecRequest {
                argv: self.argv,
                cwd: self.cwd,
                env: self.env,
                timeout_ms: self.timeout_ms,
                user: self.user,
            },
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecretSpec {
    #[schemars(description = "Name to refer to the secret by")]
    pub name: String,
    #[schemars(description = "Secret value; never enters the guest")]
    pub value: String,
    #[schemars(description = "HTTPS hosts the value is injected into")]
    pub hosts: Vec<String>,
    #[schemars(description = "Guest environment variables that hold a placeholder")]
    pub env_vars: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecretsRequest {
    #[schemars(description = "The full set of secrets for this sandbox, as a JSON array")]
    pub secrets: Vec<SecretSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteSecretsRequest {
    #[schemars(description = "Secret names to remove from every sandbox")]
    pub names: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewRequest {
    #[schemars(description = "Port inside the guest")]
    pub port: u16,
    #[schemars(description = "Seconds the link stays valid, up to 3600")]
    pub ttl_seconds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShellRequest {
    #[serde(default)]
    #[schemars(description = "Program and arguments; empty starts the user's login shell")]
    pub argv: Vec<String>,
    #[serde(default)]
    #[schemars(description = "Working directory inside the guest (default /workspace)")]
    pub cwd: Option<String>,
    #[serde(default)]
    #[schemars(description = "Extra environment variables")]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    #[schemars(description = "Guest user to log in as")]
    pub user: Option<String>,
    #[serde(default = "default_rows")]
    #[schemars(description = "Terminal height in rows; the CLI fills it in")]
    pub rows: u16,
    #[serde(default = "default_cols")]
    #[schemars(description = "Terminal width in columns; the CLI fills it in")]
    pub cols: u16,
}

fn default_rows() -> u16 {
    24
}

fn default_cols() -> u16 {
    80
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DesktopRequest {
    #[serde(default = "default_link_ttl")]
    #[schemars(description = "Seconds the live view link stays valid, up to 3600")]
    pub ttl_seconds: u32,
    #[serde(default)]
    #[schemars(description = "Guest user that owns the desktop session")]
    pub user: Option<String>,
}

fn default_link_ttl() -> u32 {
    3600
}

pub fn labels_match(labels: &BTreeMap<String, String>, filter: &[(String, String)]) -> bool {
    filter
        .iter()
        .all(|(key, value)| labels.get(key).is_some_and(|actual| actual == value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_filter_requires_every_pair() {
        let labels = BTreeMap::from([("org".into(), "a".into()), ("session".into(), "s".into())]);
        assert!(labels_match(&labels, &[("org".into(), "a".into())]));
        assert!(!labels_match(
            &labels,
            &[("org".into(), "a".into()), ("session".into(), "x".into())]
        ));
        assert!(labels_match(&labels, &[]));
    }

    #[test]
    fn create_defaults_to_open_network_and_no_auto_delete() {
        let request: CreateRequest = serde_json::from_str(r#"{"image":"default"}"#).unwrap();
        assert_eq!(request.network.mode, NetworkMode::Open);
        assert_eq!(request.auto_delete_minutes, None);
        assert_eq!(request.cpu, 2);
    }

    #[test]
    fn run_defaults_to_the_default_image() {
        let request: RunRequest = serde_json::from_str(r#"{"argv":["true"]}"#).unwrap();
        let (create, exec) = request.split();
        assert_eq!(create.image, "default");
        assert_eq!(exec.argv, ["true"]);
        assert_eq!(exec.timeout_ms, 600_000);
    }
}
