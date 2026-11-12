use crate::{Error, GuestPath, HostMount, Result};

pub fn validate_host_mounts(mounts: &[HostMount]) -> Result<()> {
    if mounts.is_empty() {
        return Ok(());
    }
    if !cfg!(target_os = "macos") {
        return Err(Error::InvalidInput(
            "host folder mounts currently require macOS confinement; Linux support is not enabled"
                .into(),
        ));
    }
    if mounts.len() > 8 {
        return Err(Error::InvalidInput(
            "at most 8 host mounts are supported".into(),
        ));
    }
    for (index, mount) in mounts.iter().enumerate() {
        GuestPath::new(&mount.target)?;
        if !mount.target.starts_with("/workspace/")
            || mount.target.ends_with('/')
            || mount.target.split('/').any(|p| p == ".")
        {
            return Err(Error::InvalidInput(
                "host mounts must target a directory below /workspace".into(),
            ));
        }
        let source = std::fs::canonicalize(&mount.source)?;
        if source != mount.source
            || !source.is_dir()
            || source.parent().is_none()
            || source.as_os_str().as_encoded_bytes().len() > 4096
        {
            return Err(Error::InvalidInput(
                "mount source must be a canonical host directory other than root".into(),
            ));
        }
        for other in &mounts[..index] {
            if std::path::Path::new(&mount.target).starts_with(&other.target)
                || std::path::Path::new(&other.target).starts_with(&mount.target)
                || source.starts_with(&other.source)
                || other.source.starts_with(&source)
            {
                return Err(Error::InvalidInput(
                    "mount sources and targets must not overlap".into(),
                ));
            }
        }
    }
    Ok(())
}
