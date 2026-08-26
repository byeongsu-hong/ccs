//! Cooperative lock interoperating with the one Claude Code takes around
//! credential writes.
//!
//! Claude Code guards credential updates with a directory-style lock beside the
//! credentials file, treating a lock older than a fixed staleness window as
//! abandoned. Taking the same lock keeps an account switch from interleaving
//! with a token refresh, where one writer's update would otherwise be lost.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};

/// A lock older than this is treated as abandoned by a dead process. Matches
/// the window Claude Code applies to the same lock.
const STALE: Duration = Duration::from_secs(15);

/// How long to keep trying before giving up on a contended lock.
const TIMEOUT: Duration = Duration::from_secs(10);

const RETRY: Duration = Duration::from_millis(100);

/// A held lock, released on drop.
pub struct Guard {
    path: PathBuf,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.path);
    }
}

/// Take the credential write lock in `dir`, waiting out a contended lock and
/// breaking one that has gone stale.
///
/// The lock is a directory because `mkdir` is atomic on every filesystem that
/// matters here, and because that is the shape the other holder already uses.
pub fn acquire(dir: &Path) -> Result<Guard> {
    let path = dir.join(".storage-write.lock");
    let deadline = SystemTime::now() + TIMEOUT;
    loop {
        match fs::create_dir(&path) {
            Ok(()) => return Ok(Guard { path }),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e).with_context(|| format!("taking lock {}", path.display())),
        }
        if SystemTime::now() >= deadline {
            bail!(
                "another process holds the credential lock at {}; retry shortly",
                path.display()
            );
        }
        if stale(&path) {
            let _ = fs::remove_dir(&path);
        }
        thread::sleep(RETRY);
    }
}

fn stale(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else { return false };
    let Ok(modified) = meta.modified() else { return false };
    let Ok(age) = SystemTime::now().duration_since(modified) else { return false };
    age > STALE
}
