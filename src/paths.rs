//! Filesystem paths used by sidebar.
//!
//! Honors `SIDEBAR_HOME` for tests; defaults to `~/.sidebar`.

use std::path::PathBuf;

use anyhow::{Context, Result};

pub fn home() -> Result<PathBuf> {
    if let Ok(v) = std::env::var("SIDEBAR_HOME") {
        return Ok(PathBuf::from(v));
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".sidebar"))
}

pub fn socket() -> Result<PathBuf> {
    Ok(home()?.join("sidebar.sock"))
}

pub fn db() -> Result<PathBuf> {
    Ok(home()?.join("sidebar.db"))
}

/// Create the sidebar home directory if missing, and tighten its
/// permissions to `0700` on Unix so other users on the machine can't
/// `connect()` the unix socket or read the SQLite db.
pub fn ensure_home() -> Result<PathBuf> {
    let h = home()?;
    std::fs::create_dir_all(&h)
        .with_context(|| format!("creating sidebar home dir at {}", h.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o700);
        if let Err(e) = std::fs::set_permissions(&h, perms) {
            // Best-effort — surface but don't fail. Common case where this
            // fails: NFS or fuse mounts that ignore chmod.
            tracing::warn!(error = %e, path = %h.display(), "could not tighten sidebar home perms to 0700");
        }
    }
    Ok(h)
}
