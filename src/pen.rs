//! Pens: per-session configuration directories that differ from the real one in
//! exactly one file.
//!
//! Claude Code resolves everything it needs beneath `CLAUDE_CONFIG_DIR` — the
//! settings, the skills, the plugins, the global configuration file and the
//! credentials. A pen is a directory of symbolic links back to the real
//! configuration with one real file, `.credentials.json`, so a session launched
//! against it carries the whole environment and only the account differs. A
//! switch elsewhere rewrites the real credentials and leaves the pen's standing.

use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::fsx::write_atomic;

const DIR_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;

/// Records the configuration a pen was cut from. Named for this tool because it
/// sits in a directory Claude Code reads.
const MARKER: &str = ".ccs-pen.json";

/// Claude Code's global configuration file, which it resolves beside the home
/// directory normally and inside `CLAUDE_CONFIG_DIR` when that is set.
pub const GLOBAL: &str = ".claude.json";

/// Names the mirror never links, because the pen owns them: the credentials are
/// the whole point of the pen, the stash would make the pen's copy a mirror of
/// itself, the write lock is held state rather than configuration, and the
/// marker belongs to the pen alone.
const PRIVATE: [&str; 4] = [".credentials.json", "ccs", ".storage-write.lock", MARKER];

/// Where the real configuration lives: the directory Claude Code reads, and the
/// global configuration file that sits outside it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Home {
    pub config: PathBuf,
    pub global: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct Marker {
    home: Home,
    account: String,
}

/// The configuration `config_dir` was cut from, when it is a pen.
///
/// Answering this is what keeps the stash reachable from inside a pen: the pen
/// deliberately does not mirror it, so a `ccs` that took the pen for the real
/// configuration would find no accounts at all.
pub fn home_of(config_dir: &Path) -> Option<Home> {
    let raw = fs::read(config_dir.join(MARKER)).ok()?;
    Some(serde_json::from_slice::<Marker>(&raw).ok()?.home)
}

/// Build, or bring up to date, the pen belonging to `slug`.
pub fn prepare(home: &Home, root: &Path, slug: &str) -> Result<PathBuf> {
    let pen = root.join("pens").join(slug);
    fs::create_dir_all(&pen).with_context(|| format!("creating {}", pen.display()))?;
    fs::set_permissions(&pen, Permissions::from_mode(DIR_MODE))
        .with_context(|| format!("securing {}", pen.display()))?;

    let marker = Marker { home: home.clone(), account: slug.to_string() };
    let body = serde_json::to_vec_pretty(&marker).context("serialising the pen marker")?;
    write_atomic(&pen.join(MARKER), &body, FILE_MODE)?;

    mirror(&pen, home)?;
    Ok(pen)
}

/// Take a pen away, along with the credentials it holds.
pub fn discard(root: &Path, slug: &str) -> Result<()> {
    let pen = root.join("pens").join(slug);
    match pen.exists() {
        true => fs::remove_dir_all(&pen).with_context(|| format!("removing {}", pen.display())),
        false => Ok(()),
    }
}

/// Hand the process over to Claude Code inside `pen`.
///
/// The image is replaced rather than a child being waited on, so nothing of
/// this tool outlives the launch and the terminal, the signals and the exit
/// status all belong to the session.
///
/// Only ever returns on failure to launch.
pub fn launch(pen: &Path, binary: &str, args: &[String]) -> Result<()> {
    let error = Command::new(binary).args(args).env("CLAUDE_CONFIG_DIR", pen).exec();
    Err(error)
        .with_context(|| format!("running `{binary}`; set CCS_CLAUDE_BINARY if it is not on PATH"))
}

/// Point every name in the real configuration at itself from inside the pen.
///
/// Only missing names are linked: whatever the pen already holds stays, so a
/// link Claude Code has replaced with a file of its own remains the pen's.
fn mirror(pen: &Path, home: &Home) -> Result<()> {
    prune(pen);

    let entries =
        fs::read_dir(&home.config).with_context(|| format!("reading {}", home.config.display()))?;
    for entry in entries {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if PRIVATE.contains(&name) {
            continue;
        }
        link(&path, &pen.join(name))?;
    }

    // The global configuration file lives outside the configuration directory
    // and moves inside `CLAUDE_CONFIG_DIR` when that is set, so a pen that did
    // not carry it would start a session with no onboarding and no project it
    // has ever been trusted in.
    link(&home.global, &pen.join(GLOBAL))
}

fn link(target: &Path, at: &Path) -> Result<()> {
    if at.symlink_metadata().is_ok() {
        return Ok(());
    }
    std::os::unix::fs::symlink(target, at)
        .with_context(|| format!("linking {} to {}", at.display(), target.display()))
}

/// Drop links whose target has gone, so a name the real configuration lost and
/// regained is linked afresh rather than resolving to nothing.
fn prune(pen: &Path) {
    let Ok(entries) = fs::read_dir(pen) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = path.symlink_metadata() else { continue };
        if meta.is_symlink() && !path.exists() {
            let _ = fs::remove_file(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A private root per test, so concurrently running tests never share a path.
    struct Fixture {
        root: PathBuf,
        home: Home,
    }

    impl Fixture {
        /// A real configuration holding one of everything a pen has to reason
        /// about: an ordinary file, a directory, a dotfile, and the two names
        /// the pen owns rather than mirrors.
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("ccs-pen-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&root);

            let config = root.join("claude");
            fs::create_dir_all(config.join("skills")).expect("skills");
            fs::create_dir_all(config.join("ccs")).expect("stash");
            fs::create_dir_all(config.join(".storage-write.lock")).expect("lock");
            fs::write(config.join("settings.json"), b"{}").expect("settings");
            fs::write(config.join(".last-cleanup"), b"").expect("dotfile");
            fs::write(config.join(".credentials.json"), b"{}").expect("credentials");

            let global = root.join(GLOBAL);
            fs::write(&global, b"{}").expect("global");

            Self { root, home: Home { config, global } }
        }

        fn prepare(&self) -> PathBuf {
            prepare(&self.home, &self.root, "someone_at_example.com").expect("prepare")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn links_to(pen: &Path, name: &str) -> Option<PathBuf> {
        fs::read_link(pen.join(name)).ok()
    }

    #[test]
    fn a_pen_links_every_name_the_real_configuration_has() {
        let fixture = Fixture::new("mirror");
        let pen = fixture.prepare();

        for name in ["skills", "settings.json", ".last-cleanup"] {
            assert_eq!(
                links_to(&pen, name).as_deref(),
                Some(fixture.home.config.join(name).as_path()),
                "{name} should be linked into the pen"
            );
        }
    }

    #[test]
    fn a_pen_owns_its_credentials_rather_than_borrowing_them() {
        let pen = Fixture::new("private").prepare();
        for name in [".credentials.json", "ccs", ".storage-write.lock"] {
            assert!(!pen.join(name).exists(), "{name} must not be mirrored into a pen");
        }
    }

    #[test]
    fn a_pen_carries_the_global_configuration_file() {
        let fixture = Fixture::new("global");
        let pen = fixture.prepare();
        assert_eq!(links_to(&pen, GLOBAL).as_deref(), Some(fixture.home.global.as_path()));
    }

    #[test]
    fn a_pen_says_which_configuration_it_was_cut_from() {
        let fixture = Fixture::new("marker");
        let pen = fixture.prepare();
        assert_eq!(home_of(&pen), Some(fixture.home.clone()));
    }

    #[test]
    fn a_directory_that_is_not_a_pen_claims_no_home() {
        let fixture = Fixture::new("unmarked");
        assert_eq!(home_of(&fixture.home.config), None);
    }

    #[test]
    fn preparing_a_pen_a_second_time_leaves_it_as_it_was() {
        let fixture = Fixture::new("idempotent");
        let pen = fixture.prepare();
        let before = names(&pen);
        assert_eq!(fixture.prepare(), pen);
        assert_eq!(names(&pen), before);
    }

    #[test]
    fn a_file_the_pen_has_made_its_own_is_not_replaced_by_a_link() {
        let fixture = Fixture::new("owned");
        let pen = fixture.prepare();

        fs::remove_file(pen.join("settings.json")).expect("unlink");
        fs::write(pen.join("settings.json"), b"{\"mine\":true}").expect("own it");
        fixture.prepare();

        assert_eq!(links_to(&pen, "settings.json"), None, "the pen's own file should survive");
        assert_eq!(fs::read(pen.join("settings.json")).expect("read"), b"{\"mine\":true}");
    }

    #[test]
    fn a_link_whose_target_has_gone_is_dropped() {
        let fixture = Fixture::new("dangling");
        let pen = fixture.prepare();

        fs::remove_file(fixture.home.config.join("settings.json")).expect("remove target");
        fixture.prepare();
        assert!(pen.join("settings.json").symlink_metadata().is_err(), "a dead link should go");
    }

    #[test]
    fn a_name_that_comes_back_is_linked_afresh() {
        let fixture = Fixture::new("returning");
        let pen = fixture.prepare();
        let target = fixture.home.config.join("settings.json");

        fs::remove_file(&target).expect("remove target");
        fixture.prepare();
        fs::write(&target, b"{}").expect("bring it back");
        fixture.prepare();

        assert_eq!(links_to(&pen, "settings.json").as_deref(), Some(target.as_path()));
    }

    #[test]
    fn discarding_takes_the_credentials_with_it() {
        let fixture = Fixture::new("discard");
        let pen = fixture.prepare();
        fs::write(pen.join(".credentials.json"), b"{}").expect("credentials");

        discard(&fixture.root, "someone_at_example.com").expect("discard");
        assert!(!pen.exists());
    }

    #[test]
    fn discarding_a_pen_that_was_never_built_is_not_an_error() {
        let fixture = Fixture::new("absent");
        assert!(discard(&fixture.root, "nobody").is_ok());
    }

    fn names(pen: &Path) -> Vec<String> {
        let mut out: Vec<String> = fs::read_dir(pen)
            .expect("read pen")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }
}
