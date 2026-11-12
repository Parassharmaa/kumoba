use crate::{Error, Result, Runtime, Sandbox, SandboxConfig, Snapshot};
use serde::{Deserialize, Serialize};
use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::{ffi::OsStrExt, fs::OpenOptionsExt, fs::PermissionsExt},
    path::{Path, PathBuf},
};

const FORMAT_VERSION: u32 = 1;
const MAX_MANIFEST_BYTES: u64 = 16 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotImageInfo {
    pub format_version: u32,
    pub guest_protocol: u32,
    pub snapshot_id: String,
    pub architecture: String,
    pub disk_bytes: u64,
    pub sha256: String,
    pub vcpus: u8,
    pub memory_mib: u32,
}

#[derive(Debug, Clone)]
pub struct SnapshotImage {
    directory: PathBuf,
    info: SnapshotImageInfo,
}

impl SnapshotImage {
    pub fn open(directory: impl AsRef<Path>) -> Result<Self> {
        let directory = std::fs::canonicalize(directory)?;
        let info: SnapshotImageInfo =
            read_manifest(&directory.join("image.json"), MAX_MANIFEST_BYTES)?;
        info.validate()?;
        if regular_file(&directory.join("root.raw"))?.metadata()?.len() != info.disk_bytes {
            return Err(Error::Integrity);
        }
        Ok(Self { directory, info })
    }

    pub fn path(&self) -> &Path {
        &self.directory
    }

    pub fn info(&self) -> &SnapshotImageInfo {
        &self.info
    }
}

impl SnapshotImageInfo {
    fn validate(&self) -> Result<()> {
        if self.format_version != FORMAT_VERSION || self.guest_protocol != kumoba_protocol::VERSION
        {
            return Err(Error::InvalidInput(
                "unsupported snapshot image format or guest protocol".into(),
            ));
        }
        if !matches!(self.architecture.as_str(), "aarch64" | "x86_64")
            || uuid::Uuid::parse_str(&self.snapshot_id).is_err()
            || self.disk_bytes == 0
            || self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::InvalidInput(
                "invalid snapshot image metadata".into(),
            ));
        }
        self.config(PathBuf::new()).validate()
    }

    fn config(&self, image: PathBuf) -> SandboxConfig {
        let mut config = SandboxConfig::new(image);
        config.vcpus = self.vcpus;
        config.memory_mib = self.memory_mib;
        config
    }
}

impl Snapshot {
    pub async fn export(&self, destination: impl AsRef<Path>) -> Result<SnapshotImage> {
        self.export_with_copy(destination, |source, target| crate::disk_copy::copy_disk(source, target))
            .await
    }

    async fn export_with_copy(
        &self,
        destination: impl AsRef<Path>,
        copy: fn(&Path, &Path) -> std::io::Result<u64>,
    ) -> Result<SnapshotImage> {
        let source = self.directory.clone();
        let destination = destination.as_ref().to_path_buf();
        tokio::task::spawn_blocking(move || {
            let manifest: crate::runtime::SnapshotManifest = read_manifest(&source.join("snapshot.json"), 1024 * 1024)?;
            if manifest.version != 2 {
                return Err(Error::Integrity);
            }
            let architecture = manifest.architecture.ok_or_else(|| Error::InvalidInput(
                "legacy snapshot lacks architecture metadata; restore and snapshot it locally before export".into(),
            ))?;
            let file_name = destination.file_name().ok_or_else(|| Error::InvalidInput("snapshot destination needs a directory name".into()))?;
            let parent = destination.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
            let parent = std::fs::canonicalize(parent)?;
            let destination = parent.join(file_name);
            if destination.try_exists()? {
                return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "snapshot destination already exists").into());
            }
            let staging = tempfile::Builder::new().prefix(".snapshot-")
                .permissions(std::fs::Permissions::from_mode(0o700)).tempdir_in(&parent)?;
            let source_disk = source.join("root.raw");
            let disk_bytes = regular_file(&source_disk)?.metadata()?.len();
            let target = staging.path().join("root.raw");
            copy(&source_disk, &target)?;
            if crate::runtime::hash_file(&target)? != manifest.sha256 || std::fs::metadata(&target)?.len() != disk_bytes {
                return Err(Error::Integrity);
            }
            let info = SnapshotImageInfo {
                format_version: FORMAT_VERSION,
                guest_protocol: kumoba_protocol::VERSION,
                snapshot_id: source.file_name().and_then(|n| n.to_str()).unwrap_or_default().into(),
                architecture,
                disk_bytes,
                sha256: manifest.sha256,
                vcpus: manifest.config.vcpus,
                memory_mib: manifest.config.memory_mib,
            };
            info.validate()?;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o400))?;
            File::open(&target)?.sync_all()?;
            let mut file = OpenOptions::new().write(true).create_new(true).mode(0o400).open(staging.path().join("image.json"))?;
            serde_json::to_writer_pretty(&mut file, &info)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            File::open(staging.path())?.sync_all()?;
            publish(staging.path(), &destination)?;
            let _ = staging.keep();
            File::open(parent)?.sync_all()?;
            Ok(SnapshotImage { directory: destination, info })
        }).await?
    }
}

impl Runtime {
    pub async fn restore_image(&self, image: &SnapshotImage) -> Result<Sandbox> {
        self.restore_image_with_copy(image, |source, target| crate::disk_copy::copy_disk(source, target))
            .await
    }

    async fn restore_image_with_copy(
        &self,
        image: &SnapshotImage,
        copy: fn(&Path, &Path) -> std::io::Result<u64>,
    ) -> Result<Sandbox> {
        if image.info.architecture != std::env::consts::ARCH {
            return Err(Error::InvalidInput(
                "snapshot architecture does not match this host".into(),
            ));
        }
        let source = image.directory.join("root.raw");
        if regular_file(&source)?.metadata()?.len() != image.info.disk_bytes {
            return Err(Error::Integrity);
        }
        self.create_verified_with_copy(
            image.info.config(source),
            Some(image.info.sha256.clone()),
            copy,
        )
        .await
    }
}

fn regular_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(Error::InvalidInput(
            "snapshot members must be regular files".into(),
        ));
    }
    Ok(file)
}

fn read_manifest<T: serde::de::DeserializeOwned>(path: &Path, limit: u64) -> Result<T> {
    let file = regular_file(path)?;
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(Error::InvalidInput(format!(
            "snapshot manifest exceeds {limit} bytes"
        )));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn publish(source: &Path, destination: &Path) -> Result<()> {
    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| Error::InvalidInput("NUL in snapshot path".into()))?;
    let destination = CString::new(destination.as_os_str().as_bytes())
        .map_err(|_| Error::InvalidInput("NUL in snapshot path".into()))?;
    #[cfg(target_os = "macos")]
    let result =
        unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
