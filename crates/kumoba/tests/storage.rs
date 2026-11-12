use kumoba::{Error, Runtime, RuntimeConfig, SandboxConfig};
use std::os::unix::fs::PermissionsExt;

fn config(root: &std::path::Path) -> RuntimeConfig {
    RuntimeConfig::new(
        std::env::current_exe().unwrap(),
        std::env::current_exe().unwrap(),
        std::env::current_exe().unwrap(),
        root,
    )
}

#[tokio::test]
async fn storage_recovery_and_integrity() -> Result<(), Box<dyn std::error::Error>> {
    let state = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()?;
    let image_dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()?;
    let image = image_dir.path().join("base.raw");
    std::fs::write(&image, b"immutable template")?;
    let runtime = Runtime::open(config(state.path())).await?;
    let mut sandbox = runtime.create(SandboxConfig::new(&image)).await?;
    let snapshot = sandbox.snapshot().await?;
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(snapshot.path().join("snapshot.json"))?)?;
    assert_eq!(
        manifest["sha256"],
        "2e111f6c83de3d19c93c87039cfbce9bbe55310773e97e04be9592fce4a525ad"
    );
    let restored = runtime.restore(&snapshot).await?;
    assert_eq!(std::fs::read(&image)?, b"immutable template");
    let disk = snapshot.path().join("root.raw");
    std::fs::set_permissions(&disk, std::fs::Permissions::from_mode(0o600))?;
    std::fs::write(&disk, b"corrupted snapshot")?;
    assert!(matches!(
        runtime.restore(&snapshot).await,
        Err(Error::Integrity)
    ));
    assert_eq!(
        runtime.list().await?.len(),
        2,
        "failed restore was not published"
    );
    let id = restored.id();
    drop(restored);
    let trash = state.path().join("trash").join(id.to_string());
    std::fs::rename(state.path().join("sandboxes").join(id.to_string()), &trash)?;
    std::fs::remove_file(trash.join("manifest.json"))?;
    sandbox.delete().await?;
    sandbox.delete().await?;
    let snapshot_trash = state
        .path()
        .join("trash")
        .join(snapshot.path().file_name().unwrap());
    std::fs::rename(snapshot.path(), &snapshot_trash)?;
    runtime.delete_snapshot(&snapshot).await?;
    assert!(
        !snapshot_trash.exists(),
        "retry must finish deletion without reopening runtime"
    );
    drop(sandbox);
    drop(runtime);
    let runtime = Runtime::open(config(state.path())).await?;
    assert!(!trash.exists());
    assert!(runtime.list().await?.is_empty());
    assert!(runtime.get(id).await.is_err());
    Ok(())
}

#[tokio::test]
async fn rejects_invalid_runtime_capacity_without_panicking() {
    let root = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    for capacity in [0, usize::MAX] {
        let mut config = config(root.path());
        config.max_running = capacity;
        assert!(matches!(
            Runtime::open(config).await,
            Err(Error::InvalidInput(_))
        ));
    }
}

#[tokio::test]
async fn old_protocol_disks_are_rejected_explicitly() -> Result<(), Box<dyn std::error::Error>> {
    let state = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()?;
    let input = tempfile::NamedTempFile::new()?;
    std::fs::write(input.path(), b"template")?;
    let runtime = Runtime::open(config(state.path())).await?;
    let sandbox = runtime.create(SandboxConfig::new(input.path())).await?;
    let id = sandbox.id();
    drop(sandbox);
    let path = state
        .path()
        .join("sandboxes")
        .join(id.to_string())
        .join("manifest.json");
    let mut manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    manifest["version"] = serde_json::json!(1);
    std::fs::write(&path, serde_json::to_vec(&manifest)?)?;
    assert!(
        matches!(runtime.get(id).await, Err(Error::InvalidInput(message)) if message.contains("rebuilt image"))
    );
    assert!(path.parent().unwrap().join("root.raw").exists());
    Ok(())
}
