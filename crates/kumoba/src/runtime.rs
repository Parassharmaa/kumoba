use crate::{Error, Result, RuntimeConfig, Sandbox, SandboxConfig, SandboxId};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

pub(crate) struct Inner {
    pub config: RuntimeConfig,
    pub capacity: Arc<tokio::sync::Semaphore>,
    pub _lock: File,
    pub authority: Option<Arc<crate::secret_https::Authority>>,
}
impl Inner {
    pub(crate) fn validate_secrets(&self, config: &SandboxConfig) -> Result<()> {
        if config.secrets.is_empty() {
            return Ok(());
        }
        let store = self
            .config
            .secret_store
            .as_ref()
            .ok_or_else(|| Error::InvalidInput("runtime has no host secret store".into()))?;
        let crate::Network::HttpConnect(policy) = &config.network else {
            return Err(Error::InvalidInput(
                "network secrets require an HTTPS proxy policy".into(),
            ));
        };
        store.validate_bindings(&config.secrets, policy)
    }
}

#[derive(Clone)]
pub struct Runtime {
    pub(crate) inner: Arc<Inner>,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub(crate) directory: PathBuf,
}
impl Snapshot {
    pub fn path(&self) -> &Path {
        &self.directory
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub version: u32,
    pub id: SandboxId,
    pub config: SandboxConfig,
    pub clean: bool,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct SnapshotManifest {
    pub version: u32,
    pub config: SandboxConfig,
    pub sha256: String,
    #[serde(default)]
    pub architecture: Option<String>,
}

impl Runtime {
    pub async fn open(mut config: RuntimeConfig) -> Result<Self> {
        if !(1..=1024).contains(&config.max_running)
            || config.boot_timeout.is_zero()
            || config.stop_timeout.is_zero()
        {
            return Err(Error::InvalidInput(
                "capacity and timeouts must be positive".into(),
            ));
        }
        config.worker = std::fs::canonicalize(&config.worker)?;
        config.library = std::fs::canonicalize(&config.library)?;
        config.firmware = std::fs::canonicalize(&config.firmware)?;
        private_dir(&config.state_dir)?;
        config.state_dir = std::fs::canonicalize(&config.state_dir)?;
        let lock = locked(&config.state_dir.join("owner.lock"))?;
        private_dir(&config.state_dir.join("sandboxes"))?;
        private_dir(&config.state_dir.join("snapshots"))?;
        let trash = config.state_dir.join("trash");
        private_dir(&trash)?;
        for entry in std::fs::read_dir(&trash)? {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && entry
                    .file_name()
                    .to_str()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    .is_some()
            {
                remove_trash(&entry.path())?;
            }
        }
        let capacity = Arc::new(tokio::sync::Semaphore::new(config.max_running));
        let authority = config
            .secret_store
            .as_ref()
            .map(|_| crate::secret_https::Authority::new())
            .transpose()?
            .map(Arc::new);
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                capacity,
                _lock: lock,
                authority,
            }),
        })
    }

    pub async fn create(&self, config: SandboxConfig) -> Result<Sandbox> {
        self.create_verified(config, None).await
    }

    pub(crate) async fn create_verified(
        &self,
        config: SandboxConfig,
        digest: Option<String>,
    ) -> Result<Sandbox> {
        self.create_verified_with_copy(config, digest, |source, target| {
            crate::disk_copy::copy_disk(source, target)
        })
        .await
    }

    pub(crate) async fn create_verified_with_copy(
        &self,
        config: SandboxConfig,
        digest: Option<String>,
        copy: fn(&Path, &Path) -> std::io::Result<u64>,
    ) -> Result<Sandbox> {
        config.validate()?;
        self.inner.validate_secrets(&config)?;
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let source = std::fs::canonicalize(&config.image)?;
            if !std::fs::metadata(&source)?.is_file() {
                return Err(Error::InvalidInput(
                    "image must be a regular raw disk file".into(),
                ));
            }
            let id = SandboxId(uuid::Uuid::new_v4());
            let parent = inner.config.state_dir.join("sandboxes");
            let staging = tempfile::Builder::new()
                .prefix(".creating-")
                .permissions(std::fs::Permissions::from_mode(0o700))
                .tempdir_in(&parent)?;
            copy(&source, &staging.path().join("root.raw"))?;
            if let Some(digest) = digest
                && hash_file(&staging.path().join("root.raw"))? != digest
            {
                return Err(Error::Integrity);
            }
            std::fs::set_permissions(
                staging.path().join("root.raw"),
                std::fs::Permissions::from_mode(0o600),
            )?;
            File::open(staging.path().join("root.raw"))?.sync_all()?;
            let manifest = Manifest {
                version: 2,
                id,
                config,
                clean: true,
            };
            save_manifest(staging.path(), &manifest)?;
            let directory = parent.join(id.to_string());
            std::fs::rename(staging.path(), &directory)?;
            File::open(&parent)?.sync_all()?;
            let lock = locked(&directory.join("disk.lock"))?;
            Ok(Sandbox::new(inner, directory, manifest, lock))
        })
        .await?
    }

    pub async fn get(&self, id: SandboxId) -> Result<Sandbox> {
        let directory = self
            .inner
            .config
            .state_dir
            .join("sandboxes")
            .join(id.to_string());
        let lock = locked(&directory.join("disk.lock"))?;
        let manifest: Manifest =
            serde_json::from_reader(File::open(directory.join("manifest.json"))?)?;
        if manifest.version != 2 || manifest.id != id {
            return Err(Error::InvalidInput(
                "incompatible sandbox manifest; protocol v2 requires a rebuilt image".into(),
            ));
        }
        manifest.config.validate()?;
        Ok(Sandbox::new(self.inner.clone(), directory, manifest, lock))
    }

    pub async fn list(&self) -> Result<Vec<SandboxId>> {
        let mut ids = Vec::new();
        for entry in std::fs::read_dir(self.inner.config.state_dir.join("sandboxes"))? {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && let Some(name) = entry.file_name().to_str()
                && let Ok(id) = name.parse()
            {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    pub async fn restore(&self, snapshot: &Snapshot) -> Result<Sandbox> {
        let directory = snapshot.directory.clone();
        let (config, digest) = tokio::task::spawn_blocking(move || {
            let manifest: SnapshotManifest =
                serde_json::from_reader(File::open(directory.join("snapshot.json"))?)?;
            if manifest.version != 2 {
                return Err(Error::Integrity);
            }
            if manifest
                .architecture
                .as_deref()
                .is_some_and(|arch| arch != std::env::consts::ARCH)
            {
                return Err(Error::InvalidInput(
                    "snapshot architecture does not match this host".into(),
                ));
            }
            let mut config = manifest.config;
            config.image = directory.join("root.raw");
            config.secrets.clear();
            config.mounts.clear();
            Ok((config, manifest.sha256))
        })
        .await??;
        self.create_verified(config, Some(digest)).await
    }

    pub fn open_snapshot(&self, path: impl AsRef<Path>) -> Result<Snapshot> {
        let directory = std::fs::canonicalize(path)?;
        if directory.parent() != Some(self.inner.config.state_dir.join("snapshots").as_path())
            || directory
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|s| uuid::Uuid::parse_str(s).ok())
                .is_none()
        {
            return Err(Error::InvalidInput(
                "snapshot must belong to this runtime store".into(),
            ));
        }
        Ok(Snapshot { directory })
    }

    pub async fn delete_snapshot(&self, snapshot: &Snapshot) -> Result<()> {
        let name = snapshot
            .directory
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| uuid::Uuid::parse_str(s).is_ok())
            .ok_or_else(|| Error::InvalidInput("invalid snapshot path".into()))?;
        if snapshot.directory.parent()
            != Some(self.inner.config.state_dir.join("snapshots").as_path())
        {
            return Err(Error::InvalidInput(
                "snapshot must belong to this runtime store".into(),
            ));
        }
        let trash = self.inner.config.state_dir.join("trash").join(name);
        match self.open_snapshot(&snapshot.directory) {
            Ok(snapshot) => {
                std::fs::rename(&snapshot.directory, &trash)?;
                File::open(snapshot.directory.parent().unwrap())?.sync_all()?;
                File::open(trash.parent().unwrap())?.sync_all()?;
                remove_trash(&trash)
            }
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => remove_trash(&trash),
            Err(e) => Err(e),
        }
    }
}

pub(crate) fn snapshot(
    inner: Arc<Inner>,
    source: &Path,
    config: SandboxConfig,
) -> Result<Snapshot> {
    let parent = inner.config.state_dir.join("snapshots");
    let temp = tempfile::Builder::new()
        .prefix(".creating-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(&parent)?;
    let target = temp.path().join("root.raw");
    crate::disk_copy::copy_disk(source, &target)?;
    File::open(&target)?.sync_all()?;
    let manifest = SnapshotManifest {
        version: 2,
        config,
        sha256: hash_file(&target)?,
        architecture: Some(std::env::consts::ARCH.into()),
    };
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(temp.path().join("snapshot.json"))?;
    serde_json::to_writer(&mut file, &manifest)?;
    file.sync_all()?;
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o400))?;
    let directory = parent.join(uuid::Uuid::new_v4().to_string());
    File::open(&target)?.sync_all()?;
    File::open(temp.path())?.sync_all()?;
    std::fs::rename(temp.path(), &directory)?;
    File::open(parent)?.sync_all()?;
    Ok(Snapshot { directory })
}

pub(crate) fn save_manifest(directory: &Path, manifest: &Manifest) -> Result<()> {
    let mut temp = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer(&mut temp, manifest)?;
    temp.flush()?;
    temp.as_file().sync_all()?;
    temp.persist(directory.join("manifest.json"))
        .map_err(|e| e.error)?;
    File::open(directory)?.sync_all()?;
    Ok(())
}
fn private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    if std::fs::metadata(path)?.permissions().mode() & 0o077 != 0 {
        return Err(Error::InvalidInput(
            "state directories must be private to their owner (mode 0700)".into(),
        ));
    }
    Ok(())
}
fn locked(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.try_lock_exclusive().map_err(|_| Error::InUse)?;
    Ok(file)
}
pub(crate) fn hash_file(path: &Path) -> Result<String> {
    let mut source = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = source.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub(crate) fn remove_trash(path: &Path) -> Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => File::open(path.parent().unwrap())?.sync_all()?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
