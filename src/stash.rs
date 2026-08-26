//! The account stash: one file per pre-logged-in account, plus a pointer to
//! whichever one is currently installed as the live credentials.

use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::fsx::write_atomic;
use crate::model::{Account, Stashed};

/// Stash files hold refresh tokens: owner-only, like the credentials they mirror.
const FILE_MODE: u32 = 0o600;
const DIR_MODE: u32 = 0o700;

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active: Option<String>,
}

pub struct Stash {
    accounts: PathBuf,
    state: PathBuf,
}

impl Stash {
    pub fn open(config_dir: &Path) -> Result<Self> {
        let root = config_dir.join("ccs");
        let accounts = root.join("accounts");
        fs::create_dir_all(&accounts).with_context(|| format!("creating {}", accounts.display()))?;
        for dir in [&root, &accounts] {
            let _ = fs::set_permissions(dir, Permissions::from_mode(DIR_MODE));
        }
        Ok(Self { accounts, state: root.join("state.json") })
    }

    /// Every stashed account, ordered by email so the table is stable run to run.
    pub fn list(&self) -> Result<Vec<Stashed>> {
        let entries = fs::read_dir(&self.accounts)
            .with_context(|| format!("reading {}", self.accounts.display()))?;

        let mut out = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(slug) = path.file_stem().and_then(|s| s.to_str()) else { continue };
            let raw = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            let account: Account = serde_json::from_slice(&raw)
                .with_context(|| format!("parsing {}", path.display()))?;
            out.push(Stashed { slug: slug.to_string(), account });
        }
        out.sort_by(|a, b| a.account.email.cmp(&b.account.email));
        Ok(out)
    }

    pub fn save(&self, slug: &str, account: &Account) -> Result<()> {
        let body = serde_json::to_vec_pretty(account).context("serialising account")?;
        write_atomic(&self.accounts.join(format!("{slug}.json")), &body, FILE_MODE)
    }

    pub fn remove(&self, slug: &str) -> Result<()> {
        let path = self.accounts.join(format!("{slug}.json"));
        fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        if self.active().as_deref() == Some(slug) {
            self.write_state(&State { active: None })?;
        }
        Ok(())
    }

    /// The slug last installed by this tool, if any. Absence is normal — it
    /// just means the live account has not been identified yet.
    pub fn active(&self) -> Option<String> {
        let raw = fs::read(&self.state).ok()?;
        serde_json::from_slice::<State>(&raw).ok()?.active
    }

    pub fn set_active(&self, slug: &str) -> Result<()> {
        self.write_state(&State { active: Some(slug.to_string()) })
    }

    fn write_state(&self, state: &State) -> Result<()> {
        let body = serde_json::to_vec_pretty(state).context("serialising state")?;
        write_atomic(&self.state, &body, FILE_MODE)
    }
}

/// Filename-safe name for an account, readable enough to type back.
pub fn slugify(email: &str) -> String {
    email.to_lowercase().chars().fold(String::new(), |mut out, ch| {
        match ch {
            'a'..='z' | '0'..='9' | '.' | '-' | '_' => out.push(ch),
            '@' => out.push_str("_at_"),
            _ => out.push('-'),
        }
        out
    })
}

/// Resolve a user-supplied account reference: exact slug, exact email, 1-based
/// index as printed by `ccs ls`, or an unambiguous prefix of slug or email.
pub fn resolve<'a>(accounts: &'a [Stashed], needle: &str) -> Result<&'a Stashed> {
    if accounts.is_empty() {
        bail!("no accounts stashed yet; run `ccs add` while logged in to capture one");
    }
    let lowered = needle.to_lowercase();

    let exact = accounts
        .iter()
        .find(|s| s.slug == lowered || s.account.email.to_lowercase() == lowered);
    if let Some(hit) = exact {
        return Ok(hit);
    }

    if let Ok(index) = needle.parse::<usize>() {
        let Some(hit) = index.checked_sub(1).and_then(|i| accounts.get(i)) else {
            bail!("no account at index {index}; `ccs ls` shows {} of them", accounts.len());
        };
        return Ok(hit);
    }

    let matches: Vec<&Stashed> = accounts
        .iter()
        .filter(|s| s.slug.starts_with(&lowered) || s.account.email.to_lowercase().starts_with(&lowered))
        .collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => bail!("no stashed account matches {needle:?}; try `ccs ls`"),
        many => bail!(
            "{needle:?} is ambiguous between {}",
            many.iter().map(|s| s.account.email.as_str()).collect::<Vec<_>>().join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Oauth;
    use serde_json::Map;

    fn stashed(slug: &str, email: &str) -> Stashed {
        Stashed {
            slug: slug.into(),
            account: Account {
                email: email.into(),
                uuid: "u".into(),
                plan: None,
                rate_limit_tier: None,
                added_at: "2026-01-01T00:00:00Z".into(),
                oauth: Oauth {
                    access_token: "a".into(),
                    refresh_token: "r".into(),
                    expires_at: 0,
                    scopes: vec![],
                    subscription_type: None,
                    extra: Map::new(),
                },
            },
        }
    }

    fn two() -> Vec<Stashed> {
        vec![stashed("jesse_at_soob.co", "jesse@soob.co"), stashed("work_at_acme.com", "work@acme.com")]
    }

    #[test]
    fn slugify_keeps_an_email_readable() {
        assert_eq!(slugify("jesse@soob.co"), "jesse_at_soob.co");
    }

    #[test]
    fn slugify_normalises_case_and_awkward_characters() {
        assert_eq!(slugify("Jesse+CC@Soob.co"), "jesse-cc_at_soob.co");
    }

    #[test]
    fn resolve_matches_a_slug_or_an_email_exactly() {
        let accounts = two();
        assert_eq!(resolve(&accounts, "jesse_at_soob.co").unwrap().slug, "jesse_at_soob.co");
        assert_eq!(resolve(&accounts, "work@acme.com").unwrap().slug, "work_at_acme.com");
    }

    #[test]
    fn resolve_is_case_insensitive() {
        assert_eq!(resolve(&two(), "WORK@ACME.COM").unwrap().slug, "work_at_acme.com");
    }

    #[test]
    fn resolve_accepts_the_index_the_table_prints() {
        assert_eq!(resolve(&two(), "2").unwrap().slug, "work_at_acme.com");
    }

    #[test]
    fn resolve_rejects_an_index_past_the_end() {
        assert!(resolve(&two(), "3").is_err());
    }

    #[test]
    fn resolve_accepts_an_unambiguous_prefix() {
        assert_eq!(resolve(&two(), "wo").unwrap().slug, "work_at_acme.com");
    }

    #[test]
    fn resolve_refuses_an_ambiguous_prefix_rather_than_guessing() {
        let accounts = vec![stashed("a_at_x.com", "a@x.com"), stashed("a_at_y.com", "a@y.com")];
        let error = resolve(&accounts, "a").unwrap_err().to_string();
        assert!(error.contains("ambiguous"), "{error}");
    }

    #[test]
    fn resolve_says_so_when_the_stash_is_empty() {
        let error = resolve(&[], "anything").unwrap_err().to_string();
        assert!(error.contains("ccs add"), "{error}");
    }
}
