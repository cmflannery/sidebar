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

pub fn ensure_home() -> Result<PathBuf> {
    let h = home()?;
    std::fs::create_dir_all(&h)
        .with_context(|| format!("creating sidebar home dir at {}", h.display()))?;
    Ok(h)
}
