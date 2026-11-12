use kumoba::{
    EgressEndpoint, Error, HttpConnectPolicy, Network, Runtime, RuntimeConfig, SandboxConfig,
    SecretStore, SecretValue, SnapshotImage,
};
use std::{
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};

fn config(path: &Path) -> RuntimeConfig {
    let exe = std::env::current_exe().unwrap();
    RuntimeConfig::new(&exe, &exe, &exe, path)
}
fn private_temp() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}
fn rewrite(path: &Path, bytes: &[u8]) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn directory_entries(path: &Path) -> Vec<std::ffi::OsString> {
    let mut names: Vec<_> = std::fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn image_survives_source_deletion_and_restores_without_grants()
-> Result<(), Box<dyn std::error::Error>> {
    let source = private_temp();
    let exports = private_temp();
    let destination = private_temp();
    let policy = HttpConnectPolicy::new([EgressEndpoint::new("example.com", 443)?])?;
    let store = SecretStore::new();
    let secret = store.create(
        "private-key",
        SecretValue::new("not-for-export-private-value")?,
        policy.clone(),
    )?;
    let mut host = config(&source.path().join("state"));
    host.secret_store = Some(store);
    let runtime = Runtime::open(host).await?;
    let base = source.path().join("private-origin.raw");
    std::fs::write(&base, b"saved project and installed toolchain")?;
    let mut settings = SandboxConfig::new(base);
    settings.vcpus = 2;
    settings.memory_mib = 1024;
    settings.network = Network::HttpConnect(policy);
    settings.secrets.insert("TOKEN".into(), secret.id);
    let mut sandbox = runtime.create(settings).await?;
    let original_id = sandbox.id();
    let snapshot = sandbox.snapshot().await?;
    let image = snapshot.export(exports.path().join("toolchain-v1")).await?;
    let json = std::fs::read_to_string(image.path().join("image.json"))?;
    for forbidden in [
        "private-origin",
        "not-for-export",
        "TOKEN",
        "example.com",
        "secrets",
    ] {
        assert!(!json.contains(forbidden), "manifest leaked {forbidden}");
    }
    sandbox.delete().await?;
    runtime.delete_snapshot(&snapshot).await?;
    drop(sandbox);
    drop(runtime);
    source.close()?;

    let image = SnapshotImage::open(image.path())?;
    let runtime = Runtime::open(config(destination.path())).await?;
    let mut restored = runtime.restore_image(&image).await?;
    assert_ne!(restored.id(), original_id);
    assert_eq!(restored.config().vcpus, 2);
    assert_eq!(restored.config().memory_mib, 1024);
    assert!(matches!(restored.config().network, Network::Disabled));
    assert!(restored.config().secrets.is_empty());
    let disk = destination
        .path()
        .join("sandboxes")
        .join(restored.id().to_string())
        .join("root.raw");
    assert_eq!(
        std::fs::read(&disk)?,
        b"saved project and installed toolchain"
    );
    std::fs::write(disk, b"independent mutation")?;
    assert_eq!(
        std::fs::read(image.path().join("root.raw"))?,
        b"saved project and installed toolchain"
    );
    restored.delete().await?;
    Ok(())
}

#[tokio::test]
async fn rejects_modified_foreign_and_unsafe_images_before_publication()
-> Result<(), Box<dyn std::error::Error>> {
    let root = private_temp();
    let runtime = Runtime::open(config(&root.path().join("state"))).await?;
    let base = root.path().join("base.raw");
    std::fs::write(&base, b"saved bytes")?;
    let mut sandbox = runtime.create(SandboxConfig::new(base)).await?;
    let snapshot = sandbox.snapshot().await?;
    let image = snapshot.export(root.path().join("v1")).await?;
    sandbox.delete().await?;
    let manifest_path = image.path().join("image.json");
    let manifest_bytes = std::fs::read(&manifest_path)?;
    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes)?;
    for (key, value) in [
        ("format_version", serde_json::json!(99)),
        ("guest_protocol", serde_json::json!(0)),
    ] {
        let mut changed = manifest.clone();
        changed[key] = value;
        rewrite(&manifest_path, &serde_json::to_vec(&changed)?);
        assert!(SnapshotImage::open(image.path()).is_err());
    }
    let mut foreign = manifest.clone();
    foreign["architecture"] = serde_json::json!(if std::env::consts::ARCH == "aarch64" {
        "x86_64"
    } else {
        "aarch64"
    });
    rewrite(&manifest_path, &serde_json::to_vec(&foreign)?);
    let foreign = SnapshotImage::open(image.path())?;
    assert!(matches!(
        runtime.restore_image(&foreign).await,
        Err(Error::InvalidInput(_))
    ));
    rewrite(&manifest_path, &vec![b' '; 16385]);
    assert!(SnapshotImage::open(image.path()).is_err());
    rewrite(&manifest_path, &manifest_bytes);
    let disk = image.path().join("root.raw");
    rewrite(&disk, b"wrong bytes"); // Same length; checksum, not size, must reject it.
    let sandbox_directory = root.path().join("state/sandboxes");
    let before = directory_entries(&sandbox_directory);
    assert!(matches!(
        runtime.restore_image(&image).await,
        Err(Error::Integrity)
    ));
    assert!(runtime.list().await?.is_empty());
    assert_eq!(directory_entries(&sandbox_directory), before);
    std::fs::remove_file(&disk)?;
    symlink("/dev/zero", &disk)?;
    assert!(SnapshotImage::open(image.path()).is_err());
    assert!(runtime.restore_image(&image).await.is_err());
    assert!(runtime.list().await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn export_never_replaces_existing_paths_even_when_racing()
-> Result<(), Box<dyn std::error::Error>> {
    let root = private_temp();
    let runtime = Runtime::open(config(&root.path().join("state"))).await?;
    let base = root.path().join("base.raw");
    std::fs::write(&base, b"template")?;
    let mut sandbox = runtime.create(SandboxConfig::new(base)).await?;
    let snapshot = sandbox.snapshot().await?;
    let occupied = root.path().join("occupied");
    std::fs::create_dir(&occupied)?;
    assert!(snapshot.export(&occupied).await.is_err());
    assert!(occupied.read_dir()?.next().is_none());
    let target = root.path().join("v1");
    let (a, b) = tokio::join!(snapshot.export(&target), snapshot.export(&target));
    assert_ne!(a.is_ok(), b.is_ok());
    let image = SnapshotImage::open(&target)?;
    let mut restored = runtime.restore_image(&image).await?;
    restored.delete().await?;
    assert!(root.path().read_dir()?.all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".snapshot-")
    }));
    Ok(())
}

#[tokio::test]
async fn legacy_local_snapshot_can_be_restored_and_resaved()
-> Result<(), Box<dyn std::error::Error>> {
    let root = private_temp();
    let runtime = Runtime::open(config(&root.path().join("state"))).await?;
    let base = root.path().join("base.raw");
    std::fs::write(&base, b"legacy image")?;
    let mut sandbox = runtime.create(SandboxConfig::new(base)).await?;
    let old = sandbox.snapshot().await?;
    let path = old.path().join("snapshot.json");
    let mut manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    manifest.as_object_mut().unwrap().remove("architecture");
    rewrite(&path, &serde_json::to_vec(&manifest)?);
    assert!(matches!(
        old.export(root.path().join("old-export")).await,
        Err(Error::InvalidInput(_))
    ));
    let mut restored = runtime.restore(&old).await?;
    let upgraded = restored.snapshot().await?;
    let image = upgraded.export(root.path().join("upgraded")).await?;
    assert_eq!(image.info().architecture, std::env::consts::ARCH);
    rewrite(&upgraded.path().join("root.raw"), b"tampered");
    let before = directory_entries(root.path());
    assert!(matches!(
        upgraded.export(root.path().join("corrupt")).await,
        Err(Error::Integrity)
    ));
    assert!(!root.path().join("corrupt").exists());
    assert_eq!(directory_entries(root.path()), before);
    Ok(())
}
