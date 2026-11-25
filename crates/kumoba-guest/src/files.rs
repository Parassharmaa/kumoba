use base64::{Engine, engine::general_purpose::STANDARD};
use kumoba_protocol::{Entry, EntryKind, MAX_DATA};
use std::{
    io,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Account {
    pub uid: u32,
    pub gid: u32,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) struct User {
    pub account: Account,
    pub home: String,
    pub name: String,
    pub shell: String,
}

pub(crate) fn path_valid(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 4096
        && !path.contains('\0')
        && !path.split('/').any(|part| part == "..")
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_string())
}

#[cfg(target_os = "linux")]
pub(crate) fn resolve_user(name: &str) -> io::Result<User> {
    let passwd = std::fs::read_to_string("/etc/passwd")?;
    passwd
        .lines()
        .find_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            (fields.len() >= 7 && fields[0] == name).then(|| {
                Some(User {
                    account: Account {
                        uid: fields[2].parse().ok()?,
                        gid: fields[3].parse().ok()?,
                    },
                    home: fields[5].to_string(),
                    name: name.to_string(),
                    shell: fields[6].to_string(),
                })
            })?
        })
        .ok_or_else(|| invalid("unknown guest user"))
}

#[cfg(target_os = "macos")]
pub(crate) fn resolve_user(name: &str) -> io::Result<User> {
    use std::ffi::{CStr, CString};
    let wanted = CString::new(name).map_err(|_| invalid("unknown guest user"))?;
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buffer = vec![0 as libc::c_char; 16 * 1024];
    let mut found = std::ptr::null_mut();
    let status = unsafe {
        libc::getpwnam_r(
            wanted.as_ptr(),
            &raw mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &raw mut found,
        )
    };
    if status != 0 || found.is_null() {
        return Err(invalid("unknown guest user"));
    }
    let text = |value: *mut libc::c_char| {
        unsafe { CStr::from_ptr(value) }
            .to_string_lossy()
            .into_owned()
    };
    Ok(User {
        account: Account {
            uid: entry.pw_uid,
            gid: entry.pw_gid,
        },
        home: text(entry.pw_dir),
        name: name.to_string(),
        shell: text(entry.pw_shell),
    })
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn pthread_setugid_np(uid: libc::uid_t, gid: libc::gid_t) -> libc::c_int;
}

#[cfg(target_os = "macos")]
const KAUTH_ID_NONE: u32 = u32::MAX - 100;

fn as_account<T>(
    owner: Option<Account>,
    operation: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    let Some(owner) = owner else {
        return operation();
    };
    #[cfg(target_os = "linux")]
    unsafe {
        libc::setfsgid(owner.gid);
        libc::setfsuid(owner.uid);
    }
    #[cfg(target_os = "macos")]
    if unsafe { pthread_setugid_np(owner.uid, owner.gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = owner;
    let result = operation();
    #[cfg(target_os = "macos")]
    unsafe {
        pthread_setugid_np(KAUTH_ID_NONE, KAUTH_ID_NONE);
    }
    #[cfg(target_os = "linux")]
    unsafe {
        libc::setfsuid(0);
        libc::setfsgid(0);
    }
    result
}

fn create_parents(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => std::fs::create_dir_all(parent),
        _ => Ok(()),
    }
}

fn open_regular(
    options: &mut std::fs::OpenOptions,
    flags: libc::c_int,
    path: impl AsRef<Path>,
) -> io::Result<std::fs::File> {
    let not_regular = || invalid("only regular files are supported");
    let file = options
        .custom_flags(flags | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| match error.raw_os_error() {
            Some(libc::ENXIO) => not_regular(),
            _ => error,
        })?;
    if !file.metadata()?.is_file() {
        return Err(not_regular());
    }
    Ok(file)
}

pub(crate) async fn read_at(
    path: &str,
    offset: u64,
    len: usize,
    owner: Option<Account>,
) -> io::Result<(Vec<u8>, u64)> {
    if !path_valid(path) || len == 0 || len > MAX_DATA {
        return Err(invalid("invalid file read"));
    }
    let target = path.to_string();
    tokio::task::spawn_blocking(move || {
        as_account(owner, || {
            let mut file = open_regular(std::fs::OpenOptions::new().read(true), 0, &target)?;
            let metadata = file.metadata()?;
            use std::io::{Read, Seek};
            file.seek(io::SeekFrom::Start(offset))?;
            let mut data =
                Vec::with_capacity(len.min(
                    usize::try_from(metadata.len().saturating_sub(offset)).unwrap_or(usize::MAX),
                ));
            file.take(len as u64).read_to_end(&mut data)?;
            Ok((data, metadata.len()))
        })
    })
    .await
    .map_err(io::Error::other)?
}

pub(crate) async fn write_at(
    path: &str,
    offset: u64,
    encoded: &str,
    truncate: bool,
    owner: Option<Account>,
) -> io::Result<()> {
    if !path_valid(path) {
        return Err(invalid("invalid guest path"));
    }
    let data = STANDARD.decode(encoded).map_err(io::Error::other)?;
    if data.len() > MAX_DATA {
        return Err(invalid("file chunk exceeds transfer limit"));
    }
    let target = path.to_string();
    tokio::task::spawn_blocking(move || {
        as_account(owner, || {
            let target = Path::new(&target);
            create_parents(target)?;
            let mut file = open_regular(
                std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(truncate)
                    .mode(0o644),
                libc::O_NOFOLLOW,
                target,
            )?;
            use std::io::{Seek, Write};
            file.seek(io::SeekFrom::Start(offset))?;
            file.write_all(&data)?;
            file.flush()
        })
    })
    .await
    .map_err(io::Error::other)?
}

pub(crate) struct Upload {
    target: PathBuf,
    owner: Option<Account>,
    file: std::fs::File,
    staged: Option<tempfile::TempPath>,
    truncate: bool,
}

fn refuse_irregular(target: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(target) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(io::Error::from_raw_os_error(libc::ELOOP))
        }
        Ok(metadata) if !metadata.is_file() => Err(invalid("only regular files are supported")),
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

fn stage_beside(target: &Path, existing: Option<&std::fs::Metadata>) -> io::Result<Option<Upload>> {
    if existing.is_some_and(|metadata| metadata.nlink() > 1) {
        return Ok(None);
    }
    let parent = target
        .parent()
        .ok_or_else(|| invalid("invalid guest path"))?;
    let staged = match tempfile::Builder::new()
        .prefix(".kumoba-upload-")
        .permissions(std::fs::Permissions::from_mode(0o644))
        .tempfile_in(parent)
    {
        Ok(staged) => staged,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => return Ok(None),
        Err(error) => return Err(error),
    };
    if let Some(metadata) = existing {
        let current = staged.as_file().metadata()?;
        if (current.uid(), current.gid()) != (metadata.uid(), metadata.gid()) {
            match std::os::unix::fs::fchown(
                staged.as_file(),
                Some(metadata.uid()),
                Some(metadata.gid()),
            ) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::PermissionDenied => return Ok(None),
                Err(error) => return Err(error),
            }
        }
        staged
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(metadata.mode() & 0o7777))?;
    }
    let (file, path) = staged.into_parts();
    Ok(Some(Upload {
        target: target.to_path_buf(),
        owner: None,
        file,
        staged: Some(path),
        truncate: false,
    }))
}

impl Upload {
    pub(crate) async fn begin(path: &str, owner: Option<Account>) -> io::Result<Self> {
        if !path_valid(path) {
            return Err(invalid("invalid guest path"));
        }
        let target = PathBuf::from(path);
        tokio::task::spawn_blocking(move || {
            as_account(owner, || {
                create_parents(&target)?;
                let existing = match open_regular(
                    std::fs::OpenOptions::new().write(true),
                    libc::O_NOFOLLOW,
                    &target,
                ) {
                    Ok(file) => Some(file),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                    Err(error) => return Err(error),
                };
                let metadata = existing.as_ref().map(std::fs::File::metadata).transpose()?;
                if let Some(upload) = stage_beside(&target, metadata.as_ref())? {
                    return Ok(Self { owner, ..upload });
                }
                let file = match existing {
                    Some(file) => file,
                    None => open_regular(
                        std::fs::OpenOptions::new()
                            .write(true)
                            .create(true)
                            .truncate(false)
                            .mode(0o644),
                        libc::O_NOFOLLOW,
                        &target,
                    )?,
                };
                Ok(Self {
                    target,
                    owner,
                    file,
                    staged: None,
                    truncate: true,
                })
            })
        })
        .await
        .map_err(io::Error::other)?
    }

    pub(crate) async fn write(&mut self, encoded: &str) -> io::Result<()> {
        let data = STANDARD.decode(encoded).map_err(io::Error::other)?;
        if data.len() > MAX_DATA {
            return Err(invalid("file chunk exceeds transfer limit"));
        }
        let mut file = self.file.try_clone()?;
        let truncate = std::mem::take(&mut self.truncate);
        tokio::task::spawn_blocking(move || {
            if truncate {
                file.set_len(0)?;
            }
            io::Write::write_all(&mut file, &data)
        })
        .await
        .map_err(io::Error::other)?
    }

    pub(crate) async fn commit(self) -> io::Result<()> {
        tokio::task::spawn_blocking(move || {
            if self.truncate {
                self.file.set_len(0)?;
            }
            self.file.sync_all()?;
            let Some(staged) = self.staged else {
                return Ok(());
            };
            let target = self.target;
            as_account(self.owner, || {
                refuse_irregular(&target)?;
                let parent = std::fs::File::open(
                    target
                        .parent()
                        .ok_or_else(|| invalid("invalid guest path"))?,
                )?;
                staged.persist(&target).map_err(|error| error.error)?;
                parent.sync_all()
            })
        })
        .await
        .map_err(io::Error::other)?
    }
}

pub(crate) fn list(path: &str, owner: Option<Account>) -> io::Result<Vec<Entry>> {
    if !path_valid(path) {
        return Err(invalid("invalid guest path"));
    }
    as_account(owner, || list_entries(path))
}

fn list_entries(path: &str) -> io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let metadata = std::fs::metadata(entry.path()).or_else(|_| entry.metadata())?;
        entries.push(Entry {
            name: entry.file_name().to_string_lossy().into_owned(),
            kind: if metadata.is_dir() {
                EntryKind::Directory
            } else if metadata.is_file() {
                EntryKind::File
            } else {
                EntryKind::Other
            },
            size: metadata.size(),
        });
        if entries.len() > 100_000 {
            return Err(invalid("directory has too many entries"));
        }
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn chunked_write_then_read_round_trips_past_one_chunk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deeper/file.bin");
        let path = path.to_str().unwrap();
        let chunk = vec![7u8; MAX_DATA];
        write_at(path, 0, &STANDARD.encode(&chunk), true, None)
            .await
            .unwrap();
        write_at(
            path,
            MAX_DATA as u64,
            &STANDARD.encode(b"tail"),
            false,
            None,
        )
        .await
        .unwrap();
        let (head, size) = read_at(path, 0, MAX_DATA, None).await.unwrap();
        assert_eq!(size, MAX_DATA as u64 + 4);
        assert_eq!(head.len(), MAX_DATA);
        let (tail, _) = read_at(path, MAX_DATA as u64, MAX_DATA, None)
            .await
            .unwrap();
        assert_eq!(tail, b"tail");
    }

    fn fifo(dir: &tempfile::TempDir) -> String {
        let path = dir.path().join("out.log");
        let c_path = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) }, 0);
        path.to_str().unwrap().to_string()
    }

    fn open_other_end(path: &str, write: bool) -> io::Result<std::fs::File> {
        std::fs::OpenOptions::new()
            .read(!write)
            .write(write)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
    }

    #[tokio::test]
    async fn write_to_a_fifo_without_reader_is_rejected_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let path = fifo(&dir);
        let data = STANDARD.encode(b"data");
        for truncate in [true, false] {
            let write = write_at(&path, 0, &data, truncate, None);
            let Ok(result) = tokio::time::timeout(Duration::from_secs(5), write).await else {
                let _ = open_other_end(&path, false);
                panic!("write to a FIFO blocked");
            };
            assert_eq!(
                result.unwrap_err().to_string(),
                "only regular files are supported"
            );
        }
    }

    #[tokio::test]
    async fn write_to_a_fifo_with_reader_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = fifo(&dir);
        let mut reader = open_other_end(&path, false).unwrap();
        let data = STANDARD.encode(b"data");
        let write = write_at(&path, 0, &data, true, None);
        let result = tokio::time::timeout(Duration::from_secs(5), write)
            .await
            .unwrap();
        assert_eq!(
            result.unwrap_err().to_string(),
            "only regular files are supported"
        );
        let mut buffer = [0u8; 8];
        assert_eq!(io::Read::read(&mut reader, &mut buffer).unwrap_or(0), 0);
    }

    #[tokio::test]
    async fn read_of_a_fifo_without_writer_is_rejected_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let path = fifo(&dir);
        let read = read_at(&path, 0, 16, None);
        let Ok(result) = tokio::time::timeout(Duration::from_secs(5), read).await else {
            let _ = open_other_end(&path, true);
            panic!("read of a FIFO blocked");
        };
        assert_eq!(
            result.unwrap_err().to_string(),
            "only regular files are supported"
        );
    }

    #[tokio::test]
    async fn write_to_a_socket_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let data = STANDARD.encode(b"data");
        let write = write_at(path.to_str().unwrap(), 0, &data, true, None);
        let result = tokio::time::timeout(Duration::from_secs(5), write)
            .await
            .unwrap();
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn listing_a_fifo_fails_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let path = fifo(&dir);
        let target = path.clone();
        let listing = tokio::task::spawn_blocking(move || list(&target, None));
        let Ok(result) = tokio::time::timeout(Duration::from_secs(5), listing).await else {
            let _ = open_other_end(&path, true);
            panic!("listing a FIFO blocked");
        };
        assert!(result.unwrap().is_err());
    }

    #[tokio::test]
    async fn write_into_an_existing_file_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, b"hello world").unwrap();
        let path = path.to_str().unwrap();
        write_at(path, 6, &STANDARD.encode(b"there"), false, None)
            .await
            .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"hello there");
    }

    #[tokio::test]
    async fn write_through_a_symlink_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.txt");
        std::fs::write(&real, b"keep").unwrap();
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let data = STANDARD.encode(b"gone");
        assert!(
            write_at(link.to_str().unwrap(), 0, &data, true, None)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&real).unwrap(), b"keep");
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn upload_replaces_an_existing_file_only_on_commit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, b"original config").unwrap();
        let mut upload = Upload::begin(path.to_str().unwrap(), None).await.unwrap();
        upload
            .write(&STANDARD.encode(vec![1u8; MAX_DATA]))
            .await
            .unwrap();
        upload.write(&STANDARD.encode(b"tail")).await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"original config");
        upload.commit().await.unwrap();
        let written = std::fs::read(&path).unwrap();
        assert_eq!(written.len(), MAX_DATA + 4);
        assert!(written.ends_with(b"\x01tail"));
        assert_eq!(names_in(dir.path()), ["config.toml"]);
    }

    #[tokio::test]
    async fn abandoned_upload_keeps_the_original_and_its_staging_file_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, b"original config").unwrap();
        let mut upload = Upload::begin(path.to_str().unwrap(), None).await.unwrap();
        upload
            .write(&STANDARD.encode(vec![1u8; MAX_DATA]))
            .await
            .unwrap();
        assert_eq!(names_in(dir.path()).len(), 2);
        drop(upload);
        assert_eq!(std::fs::read(&path).unwrap(), b"original config");
        assert_eq!(names_in(dir.path()), ["config.toml"]);
    }

    #[tokio::test]
    async fn empty_upload_creates_an_empty_file_in_new_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/empty");
        let upload = Upload::begin(path.to_str().unwrap(), None).await.unwrap();
        upload.commit().await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"");
    }

    #[tokio::test]
    async fn upload_keeps_the_mode_of_the_file_it_replaces() {
        let dir = tempfile::tempdir().unwrap();
        for mode in [0o755, 0o600] {
            let path = dir.path().join(format!("file-{mode:o}"));
            std::fs::write(&path, b"old").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            let mut upload = Upload::begin(path.to_str().unwrap(), None).await.unwrap();
            upload.write(&STANDARD.encode(b"new")).await.unwrap();
            upload.commit().await.unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), b"new");
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
                mode
            );
        }
    }

    #[tokio::test]
    async fn upload_refuses_symlinks_fifos_and_sockets() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.txt");
        std::fs::write(&real, b"keep").unwrap();
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let socket = dir.path().join("agent.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let pipe = fifo(&dir);
        for target in [link.to_str().unwrap(), socket.to_str().unwrap(), &pipe] {
            let begun = tokio::time::timeout(Duration::from_secs(5), Upload::begin(target, None))
                .await
                .unwrap();
            assert!(begun.is_err(), "{target}");
        }
        assert_eq!(
            Upload::begin(&pipe, None).await.err().unwrap().to_string(),
            "only regular files are supported"
        );
        assert_eq!(std::fs::read(&real).unwrap(), b"keep");
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(names_in(dir.path()).len(), 4);
    }

    #[tokio::test]
    async fn commit_refuses_a_symlink_that_replaced_the_target_meanwhile() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.txt");
        std::fs::write(&real, b"keep").unwrap();
        let path = dir.path().join("config.toml");
        let mut upload = Upload::begin(path.to_str().unwrap(), None).await.unwrap();
        upload.write(&STANDARD.encode(b"new")).await.unwrap();
        std::os::unix::fs::symlink(&real, &path).unwrap();
        assert!(upload.commit().await.is_err());
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&real).unwrap(), b"keep");
        assert_eq!(names_in(dir.path()), ["config.toml", "real.txt"]);
    }

    #[tokio::test]
    async fn upload_over_a_hard_linked_file_updates_every_link() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let other = dir.path().join("alias.toml");
        std::fs::write(&path, b"original config").unwrap();
        std::fs::hard_link(&path, &other).unwrap();
        let mut upload = Upload::begin(path.to_str().unwrap(), None).await.unwrap();
        upload.write(&STANDARD.encode(b"new")).await.unwrap();
        upload.commit().await.unwrap();
        assert_eq!(std::fs::read(&other).unwrap(), b"new");
        assert_eq!(names_in(dir.path()), ["alias.toml", "config.toml"]);
    }

    #[tokio::test]
    async fn abandoned_in_place_upload_keeps_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, b"original config").unwrap();
        std::fs::hard_link(&path, dir.path().join("alias.toml")).unwrap();
        drop(Upload::begin(path.to_str().unwrap(), None).await.unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"original config");
        let upload = Upload::begin(path.to_str().unwrap(), None).await.unwrap();
        upload.commit().await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"");
    }

    #[tokio::test]
    async fn upload_over_a_file_the_account_cannot_write_is_refused() {
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("readonly.txt");
        std::fs::write(&path, b"keep").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let begun = Upload::begin(path.to_str().unwrap(), None).await;
        assert_eq!(begun.err().unwrap().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::read(&path).unwrap(), b"keep");
        assert_eq!(names_in(dir.path()), ["readonly.txt"]);
    }

    #[tokio::test]
    async fn upload_into_a_read_only_directory_writes_a_writable_file_in_place() {
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        let path = locked.join("config.toml");
        std::fs::write(&path, b"original config").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = async {
            let mut upload = Upload::begin(path.to_str().unwrap(), None).await?;
            upload.write(&STANDARD.encode(b"new")).await?;
            upload.commit().await
        }
        .await;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        result.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(names_in(&locked), ["config.toml"]);
    }

    #[tokio::test]
    async fn upload_by_root_keeps_the_owner_of_the_file_it_replaces() {
        if unsafe { libc::geteuid() } != 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, b"old").unwrap();
        std::os::unix::fs::chown(&path, Some(4242), Some(4343)).unwrap();
        let mut upload = Upload::begin(path.to_str().unwrap(), None).await.unwrap();
        upload.write(&STANDARD.encode(b"new")).await.unwrap();
        upload.commit().await.unwrap();
        let metadata = std::fs::metadata(&path).unwrap();
        assert_eq!((metadata.uid(), metadata.gid()), (4242, 4343));
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
    }

    #[tokio::test]
    async fn upload_by_an_account_that_cannot_keep_the_owner_writes_in_place() {
        if unsafe { libc::geteuid() } != 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        let path = dir.path().join("shared.txt");
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        let account = Some(Account {
            uid: 4242,
            gid: 4242,
        });
        let mut upload = Upload::begin(path.to_str().unwrap(), account)
            .await
            .unwrap();
        upload.write(&STANDARD.encode(b"new")).await.unwrap();
        upload.commit().await.unwrap();
        let metadata = std::fs::metadata(&path).unwrap();
        assert_eq!((metadata.uid(), metadata.mode() & 0o7777), (0, 0o666));
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(names_in(dir.path()), ["shared.txt"]);
    }

    #[tokio::test]
    async fn upload_by_an_account_creates_files_it_owns() {
        if unsafe { libc::geteuid() } != 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        let path = dir.path().join("new.txt");
        let account = Some(Account {
            uid: 4242,
            gid: 4242,
        });
        let mut upload = Upload::begin(path.to_str().unwrap(), account)
            .await
            .unwrap();
        upload.write(&STANDARD.encode(b"new")).await.unwrap();
        upload.commit().await.unwrap();
        let metadata = std::fs::metadata(&path).unwrap();
        assert_eq!((metadata.uid(), metadata.gid()), (4242, 4242));
        assert_eq!(names_in(dir.path()), ["new.txt"]);
    }

    #[test]
    fn list_reports_kinds_sorted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("b")).unwrap();
        std::fs::write(dir.path().join("a.txt"), b"abc").unwrap();
        let entries = list(dir.path().to_str().unwrap(), None).unwrap();
        assert_eq!(entries[0].name, "a.txt");
        assert_eq!(entries[0].kind, EntryKind::File);
        assert_eq!(entries[0].size, 3);
        assert_eq!(entries[1].kind, EntryKind::Directory);
    }

    #[test]
    fn rejects_parent_traversal() {
        assert!(!path_valid("/workspace/../etc/passwd"));
        assert!(list("relative", None).is_err());
    }
}
