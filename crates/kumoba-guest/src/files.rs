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

#[cfg(target_os = "linux")]
pub(crate) struct User {
    pub account: Account,
    pub home: String,
    pub name: String,
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
                })
            })?
        })
        .ok_or_else(|| invalid("unknown guest user"))
}

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
    #[cfg(not(target_os = "linux"))]
    let _ = owner;
    let result = operation();
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
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&target)?;
            let metadata = file.metadata()?;
            if !metadata.is_file() {
                return Err(invalid("only regular files are supported"));
            }
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
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(truncate)
                .mode(0o644)
                .custom_flags(libc::O_NOFOLLOW)
                .open(target)?;
            if !file.metadata()?.is_file() {
                return Err(invalid("only regular files are supported"));
            }
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
