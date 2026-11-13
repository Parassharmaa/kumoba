use crate::model::{
    CreateRequest, Meta, NetworkMode, SecretSpec, Summary, UpdateRequest, labels_match,
};
use kumoba::{
    EgressEndpoint, HttpConnectPolicy, Network, Runtime, Sandbox, SandboxConfig, SecretId,
    SecretStore, SecretValue, State,
};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};
use tokio::sync::{Mutex, RwLock};

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{1}")]
    Status(u16, String),
    #[error(transparent)]
    Sandbox(#[from] kumoba::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl ApiError {
    pub fn status(&self) -> u16 {
        match self {
            ApiError::Status(status, _) => *status,
            ApiError::Sandbox(kumoba::Error::InvalidInput(_)) => 400,
            ApiError::Sandbox(kumoba::Error::NotRunning) => 409,
            ApiError::Sandbox(kumoba::Error::Deleted) => 404,
            ApiError::Sandbox(kumoba::Error::Capacity) => 429,
            ApiError::Sandbox(kumoba::Error::Busy | kumoba::Error::InUse) => 409,
            ApiError::Sandbox(kumoba::Error::Guest(_)) => 422,
            _ => 500,
        }
    }

    pub fn code(&self) -> &'static str {
        match self.status() {
            400 => "invalid_request",
            404 => "not_found",
            409 => "conflict",
            422 => "guest_error",
            429 => "capacity",
            _ => "internal",
        }
    }

    pub fn not_found() -> Self {
        ApiError::Status(404, "sandbox not found".into())
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        ApiError::Status(400, message.into())
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

const RESTART_DELETE_GRACE_MS: u64 = 60 * 60 * 1000;

pub fn now_ms() -> u64 {
    crate::access_log::epoch_ms(SystemTime::now())
}

pub struct Entry {
    pub meta: std::sync::Mutex<Meta>,
    pub sandbox: Mutex<Sandbox>,
    activity_ms: AtomicU64,
    stopped_at_ms: AtomicU64,
    operations: AtomicUsize,
    secrets: std::sync::Mutex<BTreeMap<String, (SecretId, Vec<String>)>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntrySnapshot {
    pub activity_ms: u64,
    pub stopped_at_ms: u64,
    pub operations: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReapAction {
    Stop,
    Delete,
}

pub fn reap_action(
    state: State,
    meta: &Meta,
    snapshot: EntrySnapshot,
    now_ms: u64,
) -> Option<ReapAction> {
    let minutes = |value: u32| u64::from(value) * 60_000;
    if snapshot.operations > 0 {
        return None;
    }
    match state {
        State::Running => (meta.auto_stop_minutes > 0
            && now_ms.saturating_sub(snapshot.activity_ms) >= minutes(meta.auto_stop_minutes))
        .then_some(ReapAction::Stop),
        State::Stopped => meta
            .auto_delete_minutes
            .filter(|delete_after| {
                snapshot.stopped_at_ms > 0
                    && now_ms.saturating_sub(snapshot.stopped_at_ms) >= minutes(*delete_after)
            })
            .map(|_| ReapAction::Delete),
        State::Deleted => None,
    }
}

pub struct OperationGuard(Arc<Entry>);

impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.0.operations.fetch_sub(1, Ordering::SeqCst);
        self.0.touch();
    }
}

impl Entry {
    fn new(meta: Meta, sandbox: Sandbox, stopped_at_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            meta: std::sync::Mutex::new(meta),
            sandbox: Mutex::new(sandbox),
            activity_ms: AtomicU64::new(now_ms()),
            stopped_at_ms: AtomicU64::new(stopped_at_ms),
            operations: AtomicUsize::new(0),
            secrets: std::sync::Mutex::new(BTreeMap::new()),
        })
    }

    pub fn snapshot(&self) -> EntrySnapshot {
        EntrySnapshot {
            activity_ms: self.activity_ms.load(Ordering::SeqCst),
            stopped_at_ms: self.stopped_at_ms.load(Ordering::SeqCst),
            operations: self.operations.load(Ordering::SeqCst),
        }
    }

    pub fn touch(&self) {
        self.activity_ms.store(now_ms(), Ordering::SeqCst);
    }

    pub fn begin(self: &Arc<Self>) -> OperationGuard {
        self.operations.fetch_add(1, Ordering::SeqCst);
        self.touch();
        OperationGuard(self.clone())
    }

    pub fn meta(&self) -> Meta {
        self.meta.lock().expect("meta lock").clone()
    }
}

pub struct ServiceConfig {
    pub state_dir: PathBuf,
    pub images: BTreeMap<String, PathBuf>,
}

#[derive(Clone, Default)]
pub struct DroppedRecords(Arc<std::sync::Mutex<HashMap<String, u64>>>);

impl DroppedRecords {
    pub fn add(&self, id: &str, count: u64) {
        let mut dropped = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *dropped.entry(id.to_string()).or_insert(0) += count;
    }

    fn take_if(&self, id: &str, ready: impl Fn(u64) -> bool) -> Option<u64> {
        let mut dropped = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let count = *dropped.get(id)?;
        ready(count).then(|| dropped.remove(id)).flatten()
    }

    fn take_all(&self) -> Vec<(String, u64)> {
        let mut dropped = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        dropped.drain().collect()
    }
}

pub struct Service {
    pub runtime: Runtime,
    pub preview_key: crate::preview::PreviewKey,
    pub access_log: Arc<crate::access_log::AccessLog>,
    dropped: DroppedRecords,
    store: SecretStore,
    config: ServiceConfig,
    public_url: std::sync::RwLock<Option<String>>,
    entries: RwLock<HashMap<String, Arc<Entry>>>,
}

fn state_name(state: State) -> &'static str {
    match state {
        State::Running => "started",
        State::Stopped => "stopped",
        State::Deleted => "destroyed",
    }
}

pub fn network_policy(spec: &crate::model::NetworkSpec) -> ApiResult<Network> {
    Ok(match spec.mode {
        NetworkMode::Blocked => Network::Disabled,
        NetworkMode::Open => Network::HttpConnect(HttpConnectPolicy::public_web()),
        NetworkMode::Allowlist => {
            let mut endpoints = Vec::new();
            let mut domains = Vec::new();
            for host in &spec.hosts {
                if let Some(domain) = host.strip_prefix("*.") {
                    domains.push(domain.to_string());
                    continue;
                }
                for port in [80, 443] {
                    endpoints.push(EgressEndpoint::new(host.clone(), port)?);
                }
            }
            Network::HttpConnect(HttpConnectPolicy::with_domains(endpoints, domains)?)
        }
    })
}

impl Service {
    pub async fn open(
        runtime: Runtime,
        store: SecretStore,
        config: ServiceConfig,
        mut access: tokio::sync::mpsc::Receiver<(kumoba::SandboxId, kumoba::AccessRecord)>,
        dropped: DroppedRecords,
    ) -> ApiResult<Arc<Self>> {
        std::fs::create_dir_all(config.state_dir.join("meta"))?;
        let preview_key =
            crate::preview::PreviewKey::load_or_create(&config.state_dir.join("preview.key"))?;
        let access_log = Arc::new(crate::access_log::AccessLog::open(
            config.state_dir.join("access"),
        )?);
        let service = Arc::new(Self {
            runtime,
            preview_key,
            access_log,
            dropped,
            store,
            config,
            public_url: std::sync::RwLock::new(None),
            entries: RwLock::new(HashMap::new()),
        });
        service.reconcile().await?;
        let recorder = service.clone();
        tokio::spawn(async move {
            while let Some((id, record)) = access.recv().await {
                let id = id.to_string();
                let labels = recorder.labels_of(&id).await;
                let entry = crate::access_log::AccessEntry::new(id.clone(), labels.clone(), record);
                match recorder.access_log.append(&entry) {
                    Ok(crate::access_log::Appended::Written) => {}
                    Ok(crate::access_log::Appended::OverCap) => recorder.dropped.add(&id, 1),
                    Err(error) => log("access_log_failed", &id, Some(ApiError::Io(error))),
                }
                if let Some(count) = recorder.dropped.take_if(&id, |count| {
                    count >= crate::access_log::DROPPED_MARKER_INTERVAL
                }) {
                    recorder.write_dropped_marker(&id, labels, count);
                }
            }
        });
        Ok(service)
    }

    async fn labels_of(&self, id: &str) -> BTreeMap<String, String> {
        self.entries
            .read()
            .await
            .get(id)
            .map(|entry| entry.meta().labels)
            .unwrap_or_default()
    }

    fn write_dropped_marker(&self, id: &str, labels: BTreeMap<String, String>, count: u64) {
        let marker =
            crate::access_log::AccessEntry::dropped_marker(id.to_string(), labels, now_ms(), count);
        if let Err(error) = self.access_log.append(&marker) {
            log("access_log_failed", id, Some(ApiError::Io(error)));
        }
    }

    async fn flush_dropped_markers(&self) {
        for (id, count) in self.dropped.take_all() {
            let labels = self.labels_of(&id).await;
            self.write_dropped_marker(&id, labels, count);
        }
    }

    pub async fn access_entries(
        &self,
        since_ms: u64,
        until_ms: u64,
        limit: usize,
        cursor: Option<crate::access_log::Cursor>,
    ) -> ApiResult<crate::access_log::Page> {
        let access_log = self.access_log.clone();
        tokio::task::spawn_blocking(move || access_log.query(since_ms, until_ms, limit, cursor))
            .await
            .map_err(std::io::Error::other)?
            .map_err(ApiError::from)
    }

    fn meta_path(&self, id: &str) -> PathBuf {
        self.config
            .state_dir
            .join("meta")
            .join(format!("{id}.json"))
    }

    fn persist(&self, meta: &Meta) -> ApiResult<()> {
        let path = self.meta_path(&meta.id);
        let mut file = tempfile::NamedTempFile::new_in(path.parent().expect("meta dir"))?;
        serde_json::to_writer(&mut file, meta).map_err(std::io::Error::other)?;
        file.as_file().sync_all()?;
        file.persist(&path).map_err(|error| error.error)?;
        Ok(())
    }

    async fn reconcile(&self) -> ApiResult<()> {
        let mut entries = self.entries.write().await;
        for id in self.runtime.list().await? {
            let key = id.to_string();
            let meta = match std::fs::read(self.meta_path(&key)) {
                Ok(bytes) => {
                    serde_json::from_slice::<Meta>(&bytes).map_err(std::io::Error::other)?
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let mut orphan = self.runtime.get(id).await?;
                    orphan.delete().await?;
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let mut sandbox = self.runtime.get(id).await?;
            if !sandbox.config().secrets.is_empty() {
                sandbox.update_secret_bindings(BTreeMap::new()).await?;
            }
            entries.insert(
                key,
                Entry::new(meta, sandbox, now_ms() + RESTART_DELETE_GRACE_MS),
            );
        }
        Ok(())
    }

    pub fn set_public_url(&self, url: Option<String>) {
        *self.public_url.write().expect("public url lock") =
            url.map(|url| url.trim_end_matches('/').to_string());
    }

    pub fn public_link(&self, path: &str) -> Option<String> {
        self.public_url
            .read()
            .expect("public url lock")
            .as_ref()
            .map(|base| format!("{base}{path}"))
    }

    pub fn image_names(&self) -> Vec<String> {
        self.config.images.keys().cloned().collect()
    }

    pub fn image_path(&self, name: &str) -> ApiResult<PathBuf> {
        self.config
            .images
            .get(name)
            .cloned()
            .ok_or_else(|| ApiError::bad_request(format!("unknown image {name}")))
    }

    pub async fn entry(&self, id: &str) -> ApiResult<Arc<Entry>> {
        self.entries
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(ApiError::not_found)
    }

    pub async fn summary(&self, entry: &Arc<Entry>) -> ApiResult<Summary> {
        let state = entry.sandbox.lock().await.state()?;
        Ok(Summary {
            meta: entry.meta(),
            state: state_name(state),
            last_activity_at_ms: entry.activity_ms.load(Ordering::SeqCst),
        })
    }

    pub async fn create(&self, request: CreateRequest) -> ApiResult<Summary> {
        let image = self.image_path(&request.image)?;
        let mut config = SandboxConfig::new(image);
        config.vcpus = request.cpu;
        config.memory_mib = request.memory_mib;
        config.network = network_policy(&request.network)?;
        let sandbox = self.runtime.create(config).await?;
        let meta = Meta {
            id: sandbox.id().to_string(),
            image: request.image,
            labels: request.labels,
            cpu: request.cpu,
            memory_mib: request.memory_mib,
            auto_stop_minutes: request.auto_stop_minutes,
            auto_delete_minutes: request.auto_delete_minutes,
            network: request.network,
            created_at_ms: now_ms(),
        };
        self.persist(&meta)?;
        let entry = Entry::new(meta.clone(), sandbox, 0);
        self.entries
            .write()
            .await
            .insert(meta.id.clone(), entry.clone());
        self.summary(&entry).await
    }

    pub async fn list(
        &self,
        filter: &[(String, String)],
        limit: usize,
    ) -> ApiResult<(Vec<Summary>, bool)> {
        let entries: Vec<Arc<Entry>> = self.entries.read().await.values().cloned().collect();
        let mut matched = Vec::new();
        for entry in entries {
            if labels_match(&entry.meta().labels, filter) {
                matched.push(self.summary(&entry).await?);
            }
        }
        matched.sort_by(|left, right| {
            left.meta
                .created_at_ms
                .cmp(&right.meta.created_at_ms)
                .then_with(|| left.meta.id.cmp(&right.meta.id))
        });
        let truncated = matched.len() > limit;
        matched.truncate(limit);
        Ok((matched, truncated))
    }

    pub async fn update(&self, id: &str, request: UpdateRequest) -> ApiResult<Summary> {
        let entry = self.entry(id).await?;
        if let Some(network) = &request.network {
            let policy = network_policy(network)?;
            let mut sandbox = entry.sandbox.lock().await;
            if sandbox.state()? != State::Stopped {
                return Err(ApiError::Status(
                    409,
                    "stop the sandbox before changing its network".into(),
                ));
            }
            sandbox.set_network(policy)?;
            entry.meta.lock().expect("meta lock").network = network.clone();
        }
        let meta = {
            let mut meta = entry.meta.lock().expect("meta lock");
            if let Some(labels) = request.labels {
                meta.labels = labels;
            }
            if let Some(minutes) = request.auto_stop_minutes {
                meta.auto_stop_minutes = minutes;
            }
            if let Some(minutes) = request.auto_delete_minutes {
                meta.auto_delete_minutes = minutes;
            }
            meta.clone()
        };
        self.persist(&meta)?;
        self.summary(&entry).await
    }

    pub async fn start(&self, id: &str) -> ApiResult<Summary> {
        let entry = self.entry(id).await?;
        {
            let mut sandbox = entry.sandbox.lock().await;
            if sandbox.state()? != State::Running {
                sandbox.start().await?;
            }
            entry.stopped_at_ms.store(0, Ordering::SeqCst);
        }
        entry.touch();
        self.summary(&entry).await
    }

    pub async fn stop(&self, id: &str) -> ApiResult<Summary> {
        let entry = self.entry(id).await?;
        entry.sandbox.lock().await.stop().await?;
        entry.stopped_at_ms.store(now_ms(), Ordering::SeqCst);
        self.summary(&entry).await
    }

    async fn forget(&self, id: &str, entry: &Entry) {
        let secrets = std::mem::take(&mut *entry.secrets.lock().expect("secrets lock"));
        for (secret, _) in secrets.into_values() {
            let _ = self.store.delete(secret);
        }
        self.entries.write().await.remove(id);
        let _ = std::fs::remove_file(self.meta_path(id));
    }

    pub async fn delete(&self, id: &str) -> ApiResult<()> {
        let entry = self.entry(id).await?;
        entry.sandbox.lock().await.delete().await?;
        let secrets = std::mem::take(&mut *entry.secrets.lock().expect("secrets lock"));
        for (secret, _) in secrets.into_values() {
            self.store.delete(secret)?;
        }
        self.entries.write().await.remove(id);
        match std::fs::remove_file(self.meta_path(id)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        }
    }

    pub async fn put_secrets(&self, id: &str, specs: Vec<SecretSpec>) -> ApiResult<Vec<String>> {
        let entry = self.entry(id).await?;
        let mut sandbox = entry.sandbox.lock().await;
        let mut current = entry.secrets.lock().expect("secrets lock").clone();
        let mut next = BTreeMap::new();
        for spec in specs {
            if spec.hosts.is_empty() {
                return Err(ApiError::bad_request(format!(
                    "secret {} has no hosts",
                    spec.name
                )));
            }
            let scope = HttpConnectPolicy::new(
                spec.hosts
                    .iter()
                    .map(|host| EgressEndpoint::new(host.clone(), 443))
                    .collect::<Result<Vec<_>, _>>()?,
            )?;
            let value = SecretValue::new(spec.value)?;
            let secret = match current.remove(&spec.name) {
                Some((secret, _)) => {
                    self.store.rotate(secret, value)?;
                    self.store.update_scope(secret, scope)?;
                    secret
                }
                None => {
                    let store_name = format!("g_{}", unique_suffix());
                    self.store.create(store_name, value, scope)?.id
                }
            };
            next.insert(spec.name, (secret, spec.env_vars));
        }
        let bindings: BTreeMap<String, SecretId> = next
            .values()
            .flat_map(|(secret, env_vars)| env_vars.iter().map(move |name| (name.clone(), *secret)))
            .collect();
        sandbox.update_secret_bindings(bindings.clone()).await?;
        for (secret, _) in current.into_values() {
            self.store.delete(secret)?;
        }
        *entry.secrets.lock().expect("secrets lock") = next;
        Ok(bindings.into_keys().collect())
    }

    pub async fn delete_secrets(&self, names: &[String]) -> ApiResult<Vec<String>> {
        let entries: Vec<(String, Arc<Entry>)> = self
            .entries
            .read()
            .await
            .iter()
            .map(|(id, entry)| (id.clone(), entry.clone()))
            .collect();
        let mut deleted = std::collections::BTreeSet::new();
        for (id, entry) in entries {
            let remaining: Vec<SecretSpecRef> = {
                let secrets = entry.secrets.lock().expect("secrets lock");
                if !names.iter().any(|name| secrets.contains_key(name)) {
                    continue;
                }
                secrets
                    .iter()
                    .filter(|(name, _)| !names.contains(name))
                    .map(|(_, (secret, env_vars))| SecretSpecRef {
                        secret: *secret,
                        env_vars: env_vars.clone(),
                    })
                    .collect()
            };
            let bindings: BTreeMap<String, SecretId> = remaining
                .iter()
                .flat_map(|spec| spec.env_vars.iter().map(|name| (name.clone(), spec.secret)))
                .collect();
            entry
                .sandbox
                .lock()
                .await
                .update_secret_bindings(bindings)
                .await?;
            let mut secrets = entry.secrets.lock().expect("secrets lock");
            for name in names {
                if let Some((secret, _)) = secrets.remove(name) {
                    self.store.delete(secret)?;
                    deleted.insert(name.clone());
                }
            }
            log("secrets_deleted", &id, None);
        }
        Ok(deleted.into_iter().collect())
    }

    pub async fn running_agent(
        &self,
        id: &str,
        command: Option<kumoba::Command>,
    ) -> ApiResult<(Arc<Entry>, kumoba::Agent, Option<kumoba::Command>)> {
        let entry = self.entry(id).await?;
        let mut sandbox = entry.sandbox.lock().await;
        if sandbox.state()? != State::Running {
            return Err(ApiError::Status(409, "sandbox is not started".into()));
        }
        let prepared = command
            .map(|command| sandbox.prepare_command(command))
            .transpose()?;
        let agent = sandbox.agent()?;
        drop(sandbox);
        Ok((entry, agent, prepared))
    }

    pub async fn reap(&self) {
        self.flush_dropped_markers().await;
        if let Err(error) = self
            .access_log
            .prune(now_ms(), crate::access_log::RETENTION)
        {
            log("access_log_prune_failed", "", Some(ApiError::Io(error)));
        }
        let entries: Vec<(String, Arc<Entry>)> = self
            .entries
            .read()
            .await
            .iter()
            .map(|(id, entry)| (id.clone(), entry.clone()))
            .collect();
        let now = now_ms();
        for (id, entry) in entries {
            let meta = entry.meta();
            let Ok(state) = entry.sandbox.lock().await.state() else {
                continue;
            };
            match reap_action(state, &meta, entry.snapshot(), now) {
                Some(ReapAction::Stop) => {
                    let mut sandbox = entry.sandbox.lock().await;
                    if matches!(sandbox.state(), Ok(State::Running))
                        && reap_action(State::Running, &meta, entry.snapshot(), now_ms())
                            == Some(ReapAction::Stop)
                    {
                        let result = sandbox.stop().await;
                        if result.is_ok() {
                            entry.stopped_at_ms.store(now_ms(), Ordering::SeqCst);
                        }
                        log("auto_stop", &id, result.err().map(ApiError::from));
                    }
                    continue;
                }
                Some(ReapAction::Delete) => {}
                None => continue,
            }
            let stopped_at = entry.stopped_at_ms.load(Ordering::SeqCst);
            {
                let mut sandbox = entry.sandbox.lock().await;
                if !matches!(sandbox.state(), Ok(State::Stopped))
                    || entry.stopped_at_ms.load(Ordering::SeqCst) != stopped_at
                    || entry.operations.load(Ordering::SeqCst) > 0
                {
                    continue;
                }
                let result = sandbox.delete().await;
                drop(sandbox);
                if result.is_ok() {
                    self.forget(&id, &entry).await;
                }
                log("auto_delete", &id, result.err().map(ApiError::from));
            }
        }
    }

    pub async fn stop_all(&self) {
        let entries: Vec<Arc<Entry>> = self.entries.read().await.values().cloned().collect();
        for entry in entries {
            let _ = tokio::time::timeout(Duration::from_secs(15), async {
                entry.sandbox.lock().await.stop().await
            })
            .await;
        }
    }
}

struct SecretSpecRef {
    secret: SecretId,
    env_vars: Vec<String>,
}

fn unique_suffix() -> String {
    use std::sync::atomic::AtomicU64;
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!("{:x}{:x}", now_ms(), COUNTER.fetch_add(1, Ordering::SeqCst))
}

pub(crate) fn log(event: &str, id: &str, error: Option<ApiError>) {
    let line = serde_json::json!({
        "type": "kumobad",
        "event": event,
        "sandboxId": id,
        "error": error.map(|error| error.to_string()),
    });
    eprintln!("{line}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(auto_stop_minutes: u32, auto_delete_minutes: Option<u32>) -> Meta {
        Meta {
            id: "sbx".into(),
            image: "test".into(),
            labels: BTreeMap::new(),
            cpu: 1,
            memory_mib: 512,
            auto_stop_minutes,
            auto_delete_minutes,
            network: crate::model::NetworkSpec::default(),
            created_at_ms: 0,
        }
    }

    fn snapshot(activity_ms: u64, stopped_at_ms: u64, operations: usize) -> EntrySnapshot {
        EntrySnapshot {
            activity_ms,
            stopped_at_ms,
            operations,
        }
    }

    const MINUTE: u64 = 60_000;

    #[test]
    fn running_sandboxes_stop_only_when_idle_past_their_limit() {
        let m = meta(15, None);
        assert_eq!(
            reap_action(State::Running, &m, snapshot(0, 0, 0), 15 * MINUTE),
            Some(ReapAction::Stop)
        );
        assert_eq!(
            reap_action(State::Running, &m, snapshot(0, 0, 0), 15 * MINUTE - 1),
            None
        );
        assert_eq!(
            reap_action(State::Running, &m, snapshot(0, 0, 1), 60 * MINUTE),
            None
        );
        assert_eq!(
            reap_action(State::Running, &meta(0, None), snapshot(0, 0, 0), u64::MAX),
            None
        );
        assert_eq!(
            reap_action(State::Running, &m, snapshot(10 * MINUTE, 0, 0), 24 * MINUTE),
            None
        );
    }

    #[test]
    fn stopped_sandboxes_delete_only_after_a_recorded_stop() {
        let m = meta(15, Some(30));
        assert_eq!(
            reap_action(State::Stopped, &m, snapshot(0, MINUTE, 0), 31 * MINUTE),
            Some(ReapAction::Delete)
        );
        assert_eq!(
            reap_action(State::Stopped, &m, snapshot(0, MINUTE, 0), 30 * MINUTE),
            None
        );
        assert_eq!(
            reap_action(State::Stopped, &m, snapshot(0, 0, 0), u64::MAX),
            None
        );
        assert_eq!(
            reap_action(
                State::Stopped,
                &meta(15, None),
                snapshot(0, MINUTE, 0),
                u64::MAX
            ),
            None
        );
        assert_eq!(
            reap_action(State::Stopped, &m, snapshot(0, MINUTE, 2), u64::MAX),
            None
        );
        assert_eq!(
            reap_action(State::Deleted, &m, snapshot(0, MINUTE, 0), u64::MAX),
            None
        );
        let restarted = snapshot(0, 5 * MINUTE + RESTART_DELETE_GRACE_MS, 0);
        assert_eq!(
            reap_action(
                State::Stopped,
                &m,
                restarted,
                5 * MINUTE + RESTART_DELETE_GRACE_MS + 29 * MINUTE
            ),
            None
        );
        assert_eq!(
            reap_action(
                State::Stopped,
                &m,
                restarted,
                5 * MINUTE + RESTART_DELETE_GRACE_MS + 30 * MINUTE
            ),
            Some(ReapAction::Delete)
        );
    }
}
