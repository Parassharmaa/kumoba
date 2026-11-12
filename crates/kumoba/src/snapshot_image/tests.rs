use super::*;
use crate::RuntimeConfig;
use std::{ffi::OsString, io};

const SOURCE_BYTES: &[u8] = b"saved workspace bytes that must survive a failed copy";
const SENTINEL_BYTES: &[u8] = b"unrelated destination must remain unchanged";

fn entries(path: &Path) -> io::Result<Vec<OsString>> {
    let mut names = std::fs::read_dir(path)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<io::Result<Vec<_>>>()?;
    names.sort();
    Ok(names)
}

async fn runtime(path: &Path) -> Result<Runtime> {
    let executable = std::env::current_exe()?;
    Runtime::open(RuntimeConfig::new(
        &executable,
        &executable,
        &executable,
        path,
    ))
    .await
}

fn partial_copy_then_eio(source: &Path, target: &Path) -> io::Result<u64> {
    let bytes = std::fs::read(source).expect("read the small source fixture");
    let prefix = &bytes[..8];
    assert!(prefix.len() < bytes.len());
    let mut file = File::create_new(target).expect("create the partial disk in staging");
    file.write_all(prefix)
        .expect("write the partial disk bytes");
    drop(file);
    assert_eq!(
        std::fs::read(target).expect("read back partial disk"),
        prefix
    );
    assert_eq!(
        std::fs::metadata(target).expect("stat partial disk").len(),
        8
    );
    eprintln!("verified 8 partial disk bytes before injecting EIO");
    Err(io::Error::from_raw_os_error(libc::EIO))
}

#[tokio::test]
async fn export_partial_copy_error_removes_staging_and_allows_retry() -> Result<()> {
    let root = tempfile::tempdir()?;
    let runtime = runtime(&root.path().join("state")).await?;
    let base = root.path().join("base.raw");
    std::fs::write(&base, SOURCE_BYTES)?;
    let mut sandbox = runtime.create(SandboxConfig::new(&base)).await?;
    let snapshot = sandbox.snapshot().await?;
    let original_manifest = std::fs::read(snapshot.path().join("snapshot.json"))?;
    let exports = root.path().join("exports");
    let sentinel = exports.join("existing-image");
    std::fs::create_dir_all(&sentinel)?;
    std::fs::write(sentinel.join("root.raw"), SENTINEL_BYTES)?;
    let before = entries(&exports)?;
    assert_eq!(before, vec![OsString::from("existing-image")]);
    let destination = exports.join("saved-image");

    let result = snapshot
        .export_with_copy(&destination, partial_copy_then_eio)
        .await;
    assert!(matches!(result, Err(Error::Io(error)) if error.raw_os_error() == Some(libc::EIO)));
    assert_eq!(entries(&exports)?, before);
    assert!(!destination.try_exists()?);
    assert_eq!(std::fs::read(sentinel.join("root.raw"))?, SENTINEL_BYTES);
    assert_eq!(std::fs::read(&base)?, SOURCE_BYTES);
    assert_eq!(
        std::fs::read(snapshot.path().join("root.raw"))?,
        SOURCE_BYTES
    );
    assert_eq!(
        std::fs::read(snapshot.path().join("snapshot.json"))?,
        original_manifest
    );

    let image = snapshot.export(&destination).await?;
    assert_eq!(std::fs::read(image.path().join("root.raw"))?, SOURCE_BYTES);
    assert_eq!(
        entries(&exports)?,
        vec![
            OsString::from("existing-image"),
            OsString::from("saved-image")
        ]
    );
    assert_eq!(std::fs::read(sentinel.join("root.raw"))?, SENTINEL_BYTES);
    sandbox.delete().await?;
    runtime.delete_snapshot(&snapshot).await?;
    drop(sandbox);
    drop(runtime);
    root.close()?;
    Ok(())
}

#[tokio::test]
async fn restore_partial_copy_error_removes_staging_and_allows_retry() -> Result<()> {
    let root = tempfile::tempdir()?;
    let runtime = runtime(&root.path().join("state")).await?;
    let base = root.path().join("base.raw");
    std::fs::write(&base, SOURCE_BYTES)?;
    let mut sandbox = runtime.create(SandboxConfig::new(&base)).await?;
    let snapshot = sandbox.snapshot().await?;
    let image = snapshot.export(root.path().join("saved-image")).await?;
    let original_manifest = std::fs::read(image.path().join("image.json"))?;
    sandbox.delete().await?;
    runtime.delete_snapshot(&snapshot).await?;
    drop(sandbox);

    let parent = root.path().join("state/sandboxes");
    let sentinel = parent.join("unrelated");
    std::fs::create_dir(&sentinel)?;
    std::fs::write(sentinel.join("root.raw"), SENTINEL_BYTES)?;
    let before = entries(&parent)?;
    assert_eq!(before, vec![OsString::from("unrelated")]);
    let result = runtime
        .restore_image_with_copy(&image, partial_copy_then_eio)
        .await;
    assert!(matches!(result, Err(Error::Io(error)) if error.raw_os_error() == Some(libc::EIO)));
    assert_eq!(entries(&parent)?, before);
    assert_eq!(std::fs::read(sentinel.join("root.raw"))?, SENTINEL_BYTES);
    assert_eq!(std::fs::read(&base)?, SOURCE_BYTES);
    assert_eq!(std::fs::read(image.path().join("root.raw"))?, SOURCE_BYTES);
    assert_eq!(
        std::fs::read(image.path().join("image.json"))?,
        original_manifest
    );
    assert_eq!(
        entries(image.path())?,
        vec![OsString::from("image.json"), OsString::from("root.raw")]
    );

    let mut restored = runtime.restore_image(&image).await?;
    let restored_directory = parent.join(restored.id().to_string());
    assert_eq!(
        std::fs::read(restored_directory.join("root.raw"))?,
        SOURCE_BYTES
    );
    let mut expected = before.clone();
    expected.push(restored.id().to_string().into());
    expected.sort();
    assert_eq!(entries(&parent)?, expected);
    restored.delete().await?;
    assert_eq!(entries(&parent)?, before);
    assert_eq!(std::fs::read(sentinel.join("root.raw"))?, SENTINEL_BYTES);
    assert_eq!(std::fs::read(image.path().join("root.raw"))?, SOURCE_BYTES);
    drop(restored);
    drop(runtime);
    root.close()?;
    Ok(())
}
