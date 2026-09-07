//! Everything a command borrows, owned in one place.
//!
//! `cmd::Ctx` borrows its collaborators so each command stays testable against
//! substitutes. A process that runs one command builds them on the stack and
//! is done. A process that lives — the app, with its gateway and watcher
//! threads — needs them owned, shared, and built from the same environment
//! the command line reads. That is this.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::api::Api;
use crate::cmd::Ctx;
use crate::codex;
use crate::creds::{Backend, CredStore};
use crate::pen;
use crate::stash::Stash;
use crate::usage;

pub struct Env {
    backend: Backend,
    creds: Box<dyn CredStore>,
    stash: Stash,
    usage: usage::Cache,
    api: Api,
    codex: codex::Store,
    codex_api: codex::Client,
    home: pen::Home,
}

impl Env {
    /// Built from the environment the command line reads: `HOME`,
    /// `CLAUDE_CONFIG_DIR` (honoured the way Claude Code honours it, so a
    /// process confined to a pen acts inside that pen) and `CODEX_HOME`.
    pub fn open() -> Result<Self> {
        let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
        let config_dir = match std::env::var_os("CLAUDE_CONFIG_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => home.join(".claude"),
        };
        let codex_home =
            std::env::var_os("CODEX_HOME").filter(|d| !d.is_empty()).map(PathBuf::from);
        Self::open_at(&home, &config_dir, codex_home.as_deref())
    }

    /// Built for a home, a configuration directory and, when Codex was told
    /// one, its own home.
    pub fn open_at(home_dir: &Path, config_dir: &Path, codex_home: Option<&Path>) -> Result<Self> {
        // Where Claude Code resolves its global configuration file: beside the
        // home directory, until `CLAUDE_CONFIG_DIR` moves it in.
        let global = match config_dir == home_dir.join(".claude") {
            true => home_dir.join(pen::GLOBAL),
            false => config_dir.join(pen::GLOBAL),
        };
        // A pen records the configuration it was cut from; anywhere else, this is it.
        let here = pen::Home { config: config_dir.to_path_buf(), global };
        let home = pen::home_of(config_dir).unwrap_or(here);

        let backend = Backend::detect();
        let stash = Stash::open(&home.config)?;
        Ok(Self {
            backend,
            creds: backend.live(config_dir),
            usage: usage::Cache::open(stash.root())?,
            stash,
            api: Api::new(),
            codex: codex::Store::at(home_dir, codex_home),
            codex_api: codex::Client::new(),
            home,
        })
    }

    /// The borrowed view every command takes.
    pub fn ctx(&self) -> Ctx<'_> {
        Ctx {
            creds: self.creds.as_ref(),
            backend: self.backend,
            stash: &self.stash,
            usage: &self.usage,
            api: &self.api,
            codex: &self.codex,
            codex_api: &self.codex_api,
            home: &self.home,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ccs-env-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    /// The whole point of owning the collaborators: they can be shared with
    /// the threads a living process runs.
    #[test]
    fn an_env_can_be_shared_across_threads() {
        fn shareable<T: Send + Sync>() {}
        shareable::<Env>();
    }

    #[test]
    fn an_env_is_built_from_a_home_and_hands_out_a_context() {
        let home = temp("home");
        let env = Env::open_at(&home, &home.join(".claude"), None).expect("opens");
        let ctx = env.ctx();
        assert!(ctx.stash.list().expect("lists").is_empty());
        assert_eq!(ctx.creds.dir(), home.join(".claude"));
        assert_eq!(ctx.codex.dir(), home.join(".codex"));
        assert_eq!(ctx.home.config, home.join(".claude"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_codex_home_of_its_own_is_honoured() {
        let home = temp("codex-home");
        let env =
            Env::open_at(&home, &home.join(".claude"), Some(&home.join("cx"))).expect("opens");
        assert_eq!(env.ctx().codex.dir(), home.join("cx"));
        let _ = std::fs::remove_dir_all(&home);
    }
}
