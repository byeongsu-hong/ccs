//! Reading and replacing Claude Code's live credentials.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::fsx::write_atomic;
use crate::model::CredsFile;

/// Credentials are secrets: owner read/write only, matching what Claude Code
/// itself writes.
const MODE: u32 = 0o600;

/// Environment Claude Code reads in preference to the credentials file.
/// Anything set here decides the account whatever the file holds — which makes
/// it something a login has to clear, and something a pinned session has to be
/// warned about.
pub const OVERRIDING: [&str; 5] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
    "CLAUDE_CODE_HOST_CREDS_FILE",
];

/// Where Claude Code's live credentials live.
///
/// Only the plain-file backend exists today. The trait is the seam a Keychain
/// backend drops into without reshaping callers.
pub trait CredStore {
    /// The live credentials, or `None` when nobody is logged in.
    fn read(&self) -> Result<Option<CredsFile>>;
    /// Replace the live credentials atomically.
    fn write(&self, creds: &CredsFile) -> Result<()>;
    /// Where this store keeps them, for saying so in errors.
    fn path(&self) -> &Path;
}

pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    pub fn new(config_dir: &Path) -> Self {
        Self { path: config_dir.join(".credentials.json") }
    }
}

impl CredStore for FileStore {
    fn read(&self) -> Result<Option<CredsFile>> {
        let raw = match fs::read(&self.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading {}", self.path.display())),
        };
        serde_json::from_slice(&raw)
            .with_context(|| format!("parsing {}", self.path.display()))
            .map(Some)
    }

    fn write(&self, creds: &CredsFile) -> Result<()> {
        let body = serde_json::to_vec_pretty(creds).context("serialising credentials")?;
        write_atomic(&self.path, &body, MODE)
    }

    fn path(&self) -> &Path {
        &self.path
    }
}
