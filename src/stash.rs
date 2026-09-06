//! The account stash: one file per pre-logged-in account, plus a pointer to
//! whichever one is currently installed as the live credentials.

use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::fsx::write_atomic;
use crate::model::{Account, Provider, Stashed};

/// Stash files hold refresh tokens: owner-only, like the credentials they mirror.
const FILE_MODE: u32 = 0o600;
const DIR_MODE: u32 = 0o700;

/// Which account is installed in each provider's live slot. The Claude
/// pointer keeps the name it had when it was the only one.
#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    codex: Option<String>,
}

impl State {
    fn slot(&mut self, provider: Provider) -> &mut Option<String> {
        match provider {
            Provider::Claude => &mut self.active,
            Provider::Codex => &mut self.codex,
        }
    }
}

pub struct Stash {
    root: PathBuf,
    accounts: PathBuf,
    state: PathBuf,
}

impl Stash {
    pub fn open(config_dir: &Path) -> Result<Self> {
        let root = config_dir.join("ccs");
        let accounts = root.join("accounts");
        fs::create_dir_all(&accounts)
            .with_context(|| format!("creating {}", accounts.display()))?;
        for dir in [&root, &accounts] {
            fs::set_permissions(dir, Permissions::from_mode(DIR_MODE))
                .with_context(|| format!("securing {}", dir.display()))?;
        }
        Ok(Self { state: root.join("state.json"), accounts, root })
    }

    /// The directory the stash owns. A login's throwaway config directory is
    /// made here, alongside the accounts rather than inside them.
    pub fn root(&self) -> &Path {
        &self.root
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
        let mut state = self.state();
        let mut changed = false;
        for provider in Provider::ALL {
            if state.slot(provider).as_deref() == Some(slug) {
                *state.slot(provider) = None;
                changed = true;
            }
        }
        if changed {
            self.write_state(&state)?;
        }
        Ok(())
    }

    /// The slug last installed by this tool in `provider`'s live slot, if
    /// any. Absence is normal — it just means the live account has not been
    /// identified yet.
    pub fn active(&self, provider: Provider) -> Option<String> {
        self.state().slot(provider).clone()
    }

    pub fn set_active(&self, provider: Provider, slug: &str) -> Result<()> {
        let mut state = self.state();
        *state.slot(provider) = Some(slug.to_string());
        self.write_state(&state)
    }

    fn state(&self) -> State {
        let Ok(raw) = fs::read(&self.state) else { return State::default() };
        serde_json::from_slice(&raw).unwrap_or_default()
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

    let exact =
        accounts.iter().find(|s| s.slug == lowered || s.account.email.to_lowercase() == lowered);
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
        .filter(|s| {
            s.slug.starts_with(&lowered) || s.account.email.to_lowercase().starts_with(&lowered)
        })
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
                provider: Provider::Claude,
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
        vec![
            stashed("work_at_example.org", "work@example.org"),
            stashed("you_at_example.com", "you@example.com"),
        ]
    }

    #[test]
    fn slugify_keeps_an_email_readable() {
        assert_eq!(slugify("you@example.com"), "you_at_example.com");
    }

    #[test]
    fn slugify_normalises_case_and_awkward_characters() {
        assert_eq!(slugify("You+CC@Example.CO"), "you-cc_at_example.co");
    }

    #[test]
    fn resolve_matches_a_slug_or_an_email_exactly() {
        let accounts = two();
        assert_eq!(resolve(&accounts, "work_at_example.org").unwrap().slug, "work_at_example.org");
        assert_eq!(resolve(&accounts, "you@example.com").unwrap().slug, "you_at_example.com");
    }

    #[test]
    fn resolve_is_case_insensitive() {
        assert_eq!(resolve(&two(), "YOU@EXAMPLE.COM").unwrap().slug, "you_at_example.com");
    }

    #[test]
    fn resolve_accepts_the_index_the_table_prints() {
        assert_eq!(resolve(&two(), "2").unwrap().slug, "you_at_example.com");
    }

    #[test]
    fn resolve_rejects_an_index_past_the_end() {
        assert!(resolve(&two(), "3").is_err());
    }

    #[test]
    fn resolve_accepts_an_unambiguous_prefix() {
        assert_eq!(resolve(&two(), "wo").unwrap().slug, "work_at_example.org");
    }

    #[test]
    fn resolve_refuses_an_ambiguous_prefix_rather_than_guessing() {
        let accounts = vec![stashed("a_at_x.com", "a@x.com"), stashed("a_at_y.com", "a@y.com")];
        let error = resolve(&accounts, "a").unwrap_err().to_string();
        assert!(error.contains("ambiguous"), "{error}");
    }

    fn temp_stash(name: &str) -> (PathBuf, Stash) {
        let dir = std::env::temp_dir().join(format!("ccs-stash-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("dir");
        let stash = Stash::open(&dir).expect("stash");
        (dir, stash)
    }

    /// Each provider has a live slot of its own, so each has a pointer of
    /// its own; installing a Codex account leaves the Claude pointer alone.
    #[test]
    fn each_provider_keeps_its_own_active_pointer() {
        let (dir, stash) = temp_stash("pointers");
        stash.set_active(Provider::Claude, "you").expect("claude");
        stash.set_active(Provider::Codex, "gpt").expect("codex");
        assert_eq!(stash.active(Provider::Claude).as_deref(), Some("you"));
        assert_eq!(stash.active(Provider::Codex).as_deref(), Some("gpt"));
        stash.set_active(Provider::Codex, "gpt2").expect("codex again");
        assert_eq!(stash.active(Provider::Claude).as_deref(), Some("you"));
        let _ = fs::remove_dir_all(&dir);
    }

    /// A state file written before there was a second provider holds the
    /// Claude pointer under its old name.
    #[test]
    fn an_old_state_file_still_names_the_claude_account() {
        let (dir, stash) = temp_stash("old-state");
        fs::write(dir.join("ccs/state.json"), r#"{"active":"you"}"#).expect("write");
        assert_eq!(stash.active(Provider::Claude).as_deref(), Some("you"));
        assert_eq!(stash.active(Provider::Codex), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn forgetting_the_active_account_clears_only_its_providers_pointer() {
        let (dir, stash) = temp_stash("forget-pointer");
        let claude = stashed("you", "you@x");
        let mut codex = stashed("gpt", "gpt@x");
        codex.account.provider = Provider::Codex;
        stash.save("you", &claude.account).expect("save");
        stash.save("gpt", &codex.account).expect("save");
        stash.set_active(Provider::Claude, "you").expect("claude");
        stash.set_active(Provider::Codex, "gpt").expect("codex");
        stash.remove("gpt").expect("remove");
        assert_eq!(stash.active(Provider::Claude).as_deref(), Some("you"));
        assert_eq!(stash.active(Provider::Codex), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_says_so_when_the_stash_is_empty() {
        let error = resolve(&[], "anything").unwrap_err().to_string();
        assert!(error.contains("ccs add"), "{error}");
    }
}
