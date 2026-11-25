use crate::service::{DroppedRecords, Service, ServiceConfig};
use kumoba::{Runtime, RuntimeConfig, SecretStore};
use std::{
    collections::BTreeMap, os::unix::fs::PermissionsExt, path::PathBuf, sync::Arc, time::Duration,
};

pub struct Daemon {
    root: tempfile::TempDir,
}

impl Daemon {
    pub fn new() -> Self {
        let root = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let worker = root.path().join("worker");
        std::fs::write(&worker, "#!/bin/sh\nexec /bin/cat >/dev/null\n").unwrap();
        std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(root.path().join("default.raw"), b"disk").unwrap();
        Self { root }
    }

    pub async fn runtime(&self) -> Runtime {
        let worker = self.root.path().join("worker");
        let mut config =
            RuntimeConfig::new(&worker, &worker, &worker, self.root.path().join("runtime"));
        config.boot_timeout = Duration::from_secs(60);
        Runtime::open(config).await.unwrap()
    }

    pub fn image(&self) -> PathBuf {
        self.root.path().join("default.raw")
    }

    pub fn meta_dir(&self) -> PathBuf {
        self.root.path().join("state").join("meta")
    }

    pub async fn service(&self, runtime: Runtime) -> Arc<Service> {
        let (_, access) = tokio::sync::mpsc::channel(1);
        Service::open(
            runtime,
            SecretStore::new(),
            ServiceConfig {
                state_dir: self.root.path().join("state"),
                images: BTreeMap::from([("default".to_string(), self.image())]),
                view: self.root.path().join("kumoba-view"),
            },
            access,
            DroppedRecords::default(),
        )
        .await
        .unwrap()
    }
}
