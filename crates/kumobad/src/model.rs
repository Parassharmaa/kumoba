use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NetworkMode {
    #[default]
    Open,
    Allowlist,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NetworkSpec {
    #[serde(default)]
    pub mode: NetworkMode,
    #[serde(default)]
    pub hosts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateRequest {
    pub image: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default = "default_cpu")]
    pub cpu: u8,
    #[serde(default = "default_memory")]
    pub memory_mib: u32,
    #[serde(default)]
    pub auto_stop_minutes: u32,
    #[serde(default)]
    pub auto_delete_minutes: Option<u32>,
    #[serde(default)]
    pub network: NetworkSpec,
}

fn default_cpu() -> u8 {
    2
}

fn default_memory() -> u32 {
    4096
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateRequest {
    #[serde(default)]
    pub labels: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub network: Option<NetworkSpec>,
    #[serde(default)]
    pub auto_stop_minutes: Option<u32>,
    #[serde(default)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecRequest {
    pub argv: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub user: Option<String>,
}

fn default_timeout() -> u64 {
    600_000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecretSpec {
    pub name: String,
    pub value: String,
    pub hosts: Vec<String>,
    pub env_vars: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecretsRequest {
    pub secrets: Vec<SecretSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteSecretsRequest {
    pub names: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewRequest {
    pub port: u16,
    pub ttl_seconds: u32,
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
}
