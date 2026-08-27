//! The usage cache: the last limits read for each account, written down.
//!
//! Every poll this tool makes already fetches an account's whole standing and
//! then throws it away when the table is gone. Something drawn far more often
//! than a table is worth — a status line repainting every few seconds — cannot
//! afford the round trip that produced it, so the reading is left where
//! anything can pick it up without asking the network.
//!
//! Limits are stored exactly as the endpoint reported them. Which windows exist,
//! and which models carry one, is the endpoint's to decide; a reader that
//! renders whatever it finds keeps a newly scoped model from needing a change
//! here.

use std::fs::{self, Permissions};
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::fsx::write_atomic;
use crate::model::Limit;

/// Readings sit beside the stash, which is owner-only; what an account has
/// spent is nobody else's business either.
const FILE_MODE: u32 = 0o600;
const DIR_MODE: u32 = 0o700;

/// One account's standing as of the moment it was read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reading {
    /// When this was fetched, RFC 3339. A reader deciding whether to trust a
    /// reading has to be told its age, because nothing in the limits says.
    pub polled_at: String,
    pub limits: Vec<Limit>,
}

pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    pub fn open(root: &Path) -> Result<Self> {
        let dir = root.join("usage");
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        fs::set_permissions(&dir, Permissions::from_mode(DIR_MODE))
            .with_context(|| format!("securing {}", dir.display()))?;
        Ok(Self { dir })
    }

    /// Write down what an account was last seen to have left.
    ///
    /// One file per account rather than one file of accounts, so two runs
    /// polling at once record their own readings instead of landing on each
    /// other's.
    pub fn record(&self, slug: &str, limits: &[Limit]) -> Result<()> {
        let reading = Reading { polled_at: Timestamp::now().to_string(), limits: limits.to_vec() };
        let body = serde_json::to_vec_pretty(&reading).context("serialising a usage reading")?;
        write_atomic(&self.at(slug), &body, FILE_MODE)
    }

    /// Drop an account's reading. Having none is a resting state — an account
    /// can be forgotten before it was ever polled — so only a real failure to
    /// remove one is worth reporting.
    pub fn forget(&self, slug: &str) -> Result<()> {
        let path = self.at(slug);
        match fs::remove_file(&path) {
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            other => other.with_context(|| format!("removing {}", path.display())),
        }
    }

    fn at(&self, slug: &str) -> PathBuf {
        self.dir.join(format!("{slug}.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limit;

    /// A private root per test, so concurrently running tests never share a
    /// scratch path.
    struct Fixture {
        root: PathBuf,
        cache: Cache,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("ccs-usage-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("root");
            Self { cache: Cache::open(&root).expect("cache"), root }
        }

        fn read(&self, slug: &str) -> Reading {
            let path = self.root.join("usage").join(format!("{slug}.json"));
            let raw = fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            serde_json::from_slice(&raw).expect("parses")
        }

        fn exists(&self, slug: &str) -> bool {
            self.root.join("usage").join(format!("{slug}.json")).exists()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn a_recorded_reading_keeps_every_limit_as_it_was_reported() {
        let fixture = Fixture::new("whole");
        let limits = vec![
            limit!("session", 3.0, resets = "2026-01-01T00:00:00Z"),
            limit!("weekly_scoped", 100.0, model = "Fable", severity = "critical"),
        ];
        fixture.cache.record("you_at_example.com", &limits).expect("records");

        let back = fixture.read("you_at_example.com");
        assert_eq!(back.limits.len(), 2);
        assert_eq!(back.limits[0].resets_at.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(back.limits[1].model_name(), Some("Fable"));
        assert_eq!(back.limits[1].severity.as_deref(), Some("critical"));
    }

    #[test]
    fn a_reading_is_stamped_with_a_time_a_reader_can_parse() {
        let fixture = Fixture::new("stamp");
        fixture.cache.record("a", &[limit!("session", 3.0)]).expect("records");

        let stamped = fixture.read("a").polled_at;
        let parsed: Timestamp = stamped.parse().unwrap_or_else(|e| panic!("{stamped}: {e}"));
        assert!((Timestamp::now().as_second() - parsed.as_second()).abs() < 60);
    }

    #[test]
    fn recording_again_replaces_the_reading_rather_than_adding_one() {
        let fixture = Fixture::new("replace");
        fixture.cache.record("a", &[limit!("session", 3.0)]).expect("records");
        fixture.cache.record("a", &[limit!("session", 40.0)]).expect("records again");

        let back = fixture.read("a");
        assert_eq!(back.limits.len(), 1);
        assert_eq!(back.limits[0].percent, 40.0);
    }

    #[test]
    fn one_accounts_reading_never_lands_on_anothers() {
        let fixture = Fixture::new("apart");
        fixture.cache.record("a", &[limit!("session", 3.0)]).expect("records a");
        fixture.cache.record("b", &[limit!("session", 90.0)]).expect("records b");

        assert_eq!(fixture.read("a").limits[0].percent, 3.0);
        assert_eq!(fixture.read("b").limits[0].percent, 90.0);
    }

    #[test]
    fn a_reading_is_owner_only_like_the_stash_beside_it() {
        let fixture = Fixture::new("mode");
        fixture.cache.record("a", &[limit!("session", 3.0)]).expect("records");

        let path = fixture.root.join("usage/a.json");
        let mode = fs::metadata(&path).expect("exists").permissions().mode();
        assert_eq!(mode & 0o777, FILE_MODE);
    }

    #[test]
    fn forgetting_an_account_takes_its_reading_with_it() {
        let fixture = Fixture::new("forget");
        fixture.cache.record("a", &[limit!("session", 3.0)]).expect("records");
        fixture.cache.forget("a").expect("forgets");
        assert!(!fixture.exists("a"));
    }

    #[test]
    fn forgetting_an_account_that_was_never_polled_is_not_a_failure() {
        let fixture = Fixture::new("unpolled");
        assert!(fixture.cache.forget("never-seen").is_ok());
    }
}
