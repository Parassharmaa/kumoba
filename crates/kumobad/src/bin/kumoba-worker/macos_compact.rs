use std::{
    fs::{File, OpenOptions},
    io,
    os::{
        fd::AsRawFd,
        unix::fs::{FileExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

const CHUNK: u64 = 256 << 20;
const BUFFER: usize = 8 << 20;
const RESERVE: u64 = 1 << 30;
const BLOCK: u64 = 4096;

#[derive(serde::Serialize, serde::Deserialize)]
struct Progress {
    moved: u64,
}

struct Paths {
    target: PathBuf,
    progress: PathBuf,
}

impl Paths {
    fn new(disk: &Path) -> Self {
        Self {
            target: disk.with_extension("compact"),
            progress: disk.with_extension("compact.json"),
        }
    }
}

pub fn compact(disk: &Path) -> io::Result<()> {
    compact_chunks(disk, CHUNK, usize::MAX).map(|_| ())
}

pub fn started(disk: &Path) -> bool {
    Paths::new(disk).progress.exists()
}

pub fn needed(disk: &Path) -> io::Result<u64> {
    Ok(std::fs::metadata(disk)?.blocks() * 512 + RESERVE)
}

pub fn fits(disk: &Path) -> io::Result<bool> {
    Ok(free_bytes(directory(disk))? >= needed(disk)?)
}

pub fn directory(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

fn compact_chunks(disk: &Path, chunk: u64, budget: usize) -> io::Result<bool> {
    let paths = Paths::new(disk);
    let source = OpenOptions::new().read(true).write(true).open(disk)?;
    source.sync_all()?;
    let size = source.metadata()?.len();
    let (target, mut moved) = match std::fs::read(&paths.progress) {
        Ok(bytes) => {
            let progress: Progress = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            match OpenOptions::new().write(true).open(&paths.target) {
                Ok(target) => (target, progress.moved),
                Err(error) if error.kind() == io::ErrorKind::NotFound && progress.moved >= size => {
                    std::fs::remove_file(&paths.progress)?;
                    return Ok(true);
                }
                Err(error) => return Err(error),
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if std::fs::metadata(&paths.target).is_ok_and(|target| target.blocks() > 0) {
                return Err(io::Error::other(format!(
                    "{} holds data but {} is missing; remove it if no compaction is running",
                    paths.target.display(),
                    paths.progress.display()
                )));
            }
            let target = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&paths.target)?;
            target.set_len(size)?;
            target.sync_all()?;
            save(&paths.progress, 0)?;
            (target, 0)
        }
        Err(error) => return Err(error),
    };
    let mut buffer = vec![0; BUFFER];
    let mut copied = 0;
    while moved < size {
        if copied == budget {
            return Ok(false);
        }
        let end = (moved + chunk).min(size);
        ensure_space(disk, end - moved)?;
        for (start, stop) in data_runs(&source, moved, end)? {
            let mut offset = start;
            while offset < stop {
                let length = usize::try_from(stop - offset)
                    .unwrap_or(usize::MAX)
                    .min(buffer.len());
                source.read_exact_at(&mut buffer[..length], offset)?;
                target.write_all_at(&buffer[..length], offset)?;
                offset += length as u64;
            }
        }
        target.sync_all()?;
        save(&paths.progress, end)?;
        punch(&source, moved, (end - moved) / BLOCK * BLOCK)?;
        moved = end;
        copied += 1;
    }
    std::fs::rename(&paths.target, disk)?;
    File::open(directory(disk))?.sync_all()?;
    std::fs::remove_file(&paths.progress)?;
    Ok(true)
}

fn save(path: &Path, moved: u64) -> io::Result<()> {
    let temporary = path.with_extension("json.next");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    serde_json::to_writer(&mut file, &Progress { moved }).map_err(io::Error::other)?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)
}

pub fn free_bytes(path: &Path) -> io::Result<u64> {
    let path =
        std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).map_err(io::Error::other)?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &raw mut stats) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(u64::from(stats.f_bavail) * stats.f_frsize)
}

fn ensure_space(disk: &Path, needed: u64) -> io::Result<()> {
    let available = free_bytes(directory(disk))?;
    if available < needed + RESERVE {
        return Err(io::Error::other(format!(
            "compacting {} needs {} MiB free on its volume and {} MiB are; free some space and run the command again",
            disk.display(),
            (needed + RESERVE) >> 20,
            available >> 20
        )));
    }
    Ok(())
}

fn seek(file: &File, offset: u64, whence: libc::c_int) -> io::Result<Option<u64>> {
    let offset = libc::off_t::try_from(offset).map_err(io::Error::other)?;
    let found = unsafe { libc::lseek(file.as_raw_fd(), offset, whence) };
    if found < 0 {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ENXIO) {
            Ok(None)
        } else {
            Err(error)
        };
    }
    u64::try_from(found).map(Some).map_err(io::Error::other)
}

fn data_runs(file: &File, start: u64, end: u64) -> io::Result<Vec<(u64, u64)>> {
    let mut runs = Vec::new();
    let mut position = start;
    while position < end {
        let Some(data) = seek(file, position, libc::SEEK_DATA)? else {
            break;
        };
        if data >= end {
            break;
        }
        let hole = seek(file, data, libc::SEEK_HOLE)?.unwrap_or(end);
        runs.push((data, hole.min(end)));
        position = hole;
    }
    Ok(runs)
}

fn punch(file: &File, offset: u64, length: u64) -> io::Result<()> {
    if length == 0 {
        return Ok(());
    }
    let region = libc::fpunchhole_t {
        fp_flags: 0,
        reserved: 0,
        fp_offset: libc::off_t::try_from(offset).map_err(io::Error::other)?,
        fp_length: libc::off_t::try_from(length).map_err(io::Error::other)?,
    };
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_PUNCHHOLE, &region) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(C, packed(4))]
    struct Log2Phys {
        flags: libc::c_uint,
        contiguous: libc::off_t,
        device_offset: libc::off_t,
    }

    fn physical_extents(path: &Path) -> usize {
        let file = File::open(path).unwrap();
        let size = file.metadata().unwrap().len();
        let mut count = 0;
        for (start, stop) in data_runs(&file, 0, size).unwrap() {
            let mut offset = start;
            while offset < stop {
                let mut query = Log2Phys {
                    flags: 0,
                    contiguous: libc::off_t::try_from(stop - offset).unwrap(),
                    device_offset: libc::off_t::try_from(offset).unwrap(),
                };
                let result =
                    unsafe { libc::fcntl(file.as_raw_fd(), libc::F_LOG2PHYS_EXT, &mut query) };
                assert!(result >= 0, "{}", io::Error::last_os_error());
                count += 1;
                offset += u64::try_from(query.contiguous).unwrap().max(BLOCK);
            }
        }
        count
    }

    fn fragmented(path: &Path, blocks: u64) -> Vec<u8> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .unwrap();
        file.set_len((blocks + 64) * BLOCK).unwrap();
        let mut order: Vec<u64> = (0..blocks).filter(|block| block % 7 != 3).collect();
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        for index in (1..order.len()).rev() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            order.swap(index, usize::try_from(state % (index as u64 + 1)).unwrap());
        }
        for block in order {
            let fill = (block % 251) as u8 + 1;
            file.write_all_at(&vec![fill; usize::try_from(BLOCK).unwrap()], block * BLOCK)
                .unwrap();
        }
        file.sync_all().unwrap();
        std::fs::read(path).unwrap()
    }

    #[test]
    fn compaction_keeps_bytes_and_holes_and_defragments() {
        let directory = tempfile::tempdir().unwrap();
        let disk = directory.path().join("root.raw");
        let original = fragmented(&disk, 8192);
        let allocated = std::fs::metadata(&disk).unwrap().blocks();
        let before = physical_extents(&disk);
        let size = std::fs::metadata(&disk).unwrap().len();
        let layout = data_runs(&File::open(&disk).unwrap(), 0, size).unwrap();
        compact_chunks(&disk, 4 << 20, usize::MAX).unwrap();
        assert_eq!(std::fs::read(&disk).unwrap(), original);
        assert_eq!(
            data_runs(&File::open(&disk).unwrap(), 0, size).unwrap(),
            layout
        );
        assert!(std::fs::metadata(&disk).unwrap().blocks() <= allocated);
        let after = physical_extents(&disk);
        assert!(after < before, "{before} extents before, {after} after");
        assert!(!Paths::new(&disk).target.exists());
        assert!(!Paths::new(&disk).progress.exists());
    }

    #[test]
    fn interrupted_compaction_resumes_to_the_same_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let disk = directory.path().join("root.raw");
        let original = fragmented(&disk, 4096);
        assert!(!compact_chunks(&disk, 1 << 20, 3).unwrap());
        assert!(Paths::new(&disk).progress.exists());
        compact_chunks(&disk, 1 << 20, usize::MAX).unwrap();
        assert_eq!(std::fs::read(&disk).unwrap(), original);
        assert!(!Paths::new(&disk).progress.exists());
    }

    #[test]
    fn a_rename_that_finished_before_the_progress_file_was_removed_completes() {
        let directory = tempfile::tempdir().unwrap();
        let disk = directory.path().join("root.raw");
        let original = fragmented(&disk, 256);
        let size = std::fs::metadata(&disk).unwrap().len();
        save(&Paths::new(&disk).progress, size).unwrap();
        assert!(compact_chunks(&disk, 1 << 20, usize::MAX).unwrap());
        assert_eq!(std::fs::read(&disk).unwrap(), original);
        assert!(!Paths::new(&disk).progress.exists());
    }

    #[test]
    fn a_target_with_data_and_no_progress_file_is_left_alone() {
        let directory = tempfile::tempdir().unwrap();
        let disk = directory.path().join("root.raw");
        let original = fragmented(&disk, 256);
        let target = Paths::new(&disk).target;
        std::fs::write(&target, [7; 8192]).unwrap();
        assert!(compact_chunks(&disk, 1 << 20, usize::MAX).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), [7; 8192]);
        assert_eq!(std::fs::read(&disk).unwrap(), original);
    }
}
