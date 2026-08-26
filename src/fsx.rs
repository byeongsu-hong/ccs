//! Filesystem helpers shared by the credential store and the account stash.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use anyhow::{Context, Result};

/// Write `bytes` to `path` atomically, leaving `mode` on the result.
///
/// Staged in a sibling temp file and renamed, so a concurrent reader sees
/// either the old contents or the new and never a partial write. The rename
/// also freshens the path's mtime, which is the signal running Claude Code
/// sessions poll to reload credentials.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let dir = path.parent().context("path has no parent directory")?;
    let name = path.file_name().context("path has no file name")?.to_string_lossy();
    let tmp = dir.join(format!(".{}.ccs-{}.tmp", name.trim_start_matches('.'), std::process::id()));

    let staged = || -> Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)
            .with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
        Ok(())
    };
    if let Err(e) = staged() {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    fs::rename(&tmp, path).with_context(|| format!("installing {}", path.display()))?;
    // Durability of the rename itself. Best-effort: it has already taken effect
    // for every reader by this point.
    let _ = File::open(dir).and_then(|d| d.sync_all());
    Ok(())
}
