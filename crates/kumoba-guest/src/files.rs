use base64::{Engine, engine::general_purpose::STANDARD};
use kumoba_protocol::{Entry, EntryKind, MAX_DATA};
use std::{
    io,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
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
