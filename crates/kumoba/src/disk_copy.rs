use std::{fs::File, io, path::Path};

pub fn copy_disk(source: &Path, target: &Path) -> io::Result<u64> {
    #[cfg(target_os = "linux")]
    {
        linux::copy_disk(source, target)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = File::open(source)?;
        std::fs::copy(source, target)
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::os::unix::io::AsRawFd;

    const FICLONE: libc::c_ulong = 0x4004_9409;

    pub fn copy_disk(source: &Path, target: &Path) -> io::Result<u64> {
        let input = File::open(source)?;
        let length = input.metadata()?.len();
        let output = File::create(target)?;
        if unsafe { libc::ioctl(output.as_raw_fd(), FICLONE as _, input.as_raw_fd()) } == 0 {
            return Ok(length);
        }
        copy_extents(&input, &output, length)?;
        output.set_len(length)?;
        output.sync_all()?;
        Ok(length)
    }

    fn seek(file: &File, offset: u64, whence: libc::c_int) -> io::Result<Option<u64>> {
        let position = unsafe { libc::lseek(file.as_raw_fd(), offset as libc::off_t, whence) };
        if position >= 0 {
            return Ok(Some(position as u64));
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENXIO) {
            return Ok(None);
        }
        Err(error)
    }

    fn copy_extents(input: &File, output: &File, length: u64) -> io::Result<()> {
        let mut offset = 0;
        while offset < length {
            let Some(data_start) = seek(input, offset, libc::SEEK_DATA)? else {
                break;
            };
            let data_end = seek(input, data_start, libc::SEEK_HOLE)?.unwrap_or(length);
            let mut cursor = data_start;
            while cursor < data_end {
                let mut from = cursor as libc::loff_t;
                let mut to = cursor as libc::loff_t;
                let copied = unsafe {
                    libc::copy_file_range(
                        input.as_raw_fd(),
                        &mut from,
                        output.as_raw_fd(),
                        &mut to,
                        (data_end - cursor) as usize,
                        0,
                    )
                };
                if copied < 0 {
                    return Err(io::Error::last_os_error());
                }
                if copied == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "disk image shrank while copying",
                    ));
                }
                cursor += copied as u64;
            }
            offset = data_end;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};

    #[test]
    fn preserves_holes_and_data() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.raw");
        let target = dir.path().join("target.raw");
        let mut file = File::create(&source).unwrap();
        file.seek(SeekFrom::Start(1 << 20)).unwrap();
        file.write_all(b"middle").unwrap();
        file.seek(SeekFrom::Start(64 << 20)).unwrap();
        file.write_all(b"tail").unwrap();
        file.set_len(256 << 20).unwrap();
        drop(file);

        assert_eq!(copy_disk(&source, &target).unwrap(), 256 << 20);
        let copied = std::fs::read(&target).unwrap();
        assert_eq!(copied.len(), 256 << 20);
        assert_eq!(&copied[(1 << 20)..(1 << 20) + 6], b"middle");
        assert_eq!(&copied[(64 << 20)..(64 << 20) + 4], b"tail");
        assert!(copied[..1 << 20].iter().all(|byte| *byte == 0));
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::MetadataExt;
            let blocks = std::fs::metadata(&target).unwrap().blocks();
            assert!(blocks * 512 < 8 << 20, "copy is not sparse: {blocks} blocks");
        }
    }
}
