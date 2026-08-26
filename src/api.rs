//! Anthropic OAuth client: identity, usage, and token refresh.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::model::{Oauth, Profile, UsageResponse, now_ms};

const API_BASE: &str = "https://api.anthropic.com";
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";

/// Claude Code's own OAuth client. The tokens in the credentials file were
/// minted for it, so a refresh has to present the same id to be honoured.
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// The beta gating the OAuth surface.
const BETA: &str = "oauth-2025-04-20";

const TIMEOUT: Duration = Duration::from_secs(15);

/// Lifetime assumed when a refresh response declines to state one. Deliberately
/// short: guessing low costs an extra refresh, guessing high costs a failed
/// request at the worst moment.
const FALLBACK_LIFETIME_MS: i64 = 60 * 60 * 1000;

pub struct Api {
    agent: ureq::Agent,
}

impl Default for Api {
    fn default() -> Self {
        Self::new()
    }
}

/// Tokens as the refresh endpoint returns them.
#[derive(Debug, Clone, Deserialize)]
pub struct Refreshed {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Seconds.
    #[serde(default)]
    pub expires_in: Option<i64>,
}

impl Api {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .user_agent(concat!("ccs/", env!("CARGO_PKG_VERSION")))
            // Read statuses rather than have them raised as transport errors:
            // a 401 here means a specific, actionable thing worth saying.
            .http_status_as_error(false)
            .build();
        Self { agent: ureq::Agent::new_with_config(config) }
    }

    /// Who a token belongs to.
    pub fn profile(&self, token: &str) -> Result<Profile> {
        self.get_json(&format!("{API_BASE}/api/oauth/profile"), token)
    }

    /// Current rate limit standing for a token.
    pub fn usage(&self, token: &str) -> Result<UsageResponse> {
        self.get_json(&format!("{API_BASE}/api/oauth/usage"), token)
    }

    /// Trade a refresh token for a fresh access token.
    ///
    /// The response may rotate the refresh token, and the caller must persist
    /// whatever comes back before doing anything else: losing a rotated refresh
    /// token costs an interactive re-login.
    pub fn refresh(&self, refresh_token: &str, scopes: &[String]) -> Result<Refreshed> {
        let body = json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": CLIENT_ID,
            "scope": scopes.join(" "),
        });
        let mut resp = self
            .agent
            .post(TOKEN_URL)
            .header("Content-Type", "application/json")
            .send_json(&body)
            .with_context(|| format!("POST {TOKEN_URL}"))?;

        let status = resp.status().as_u16();
        if status != 200 {
            let detail = resp.body_mut().read_to_string().unwrap_or_default();
            bail!("token refresh failed ({status}): {}", snippet(&detail));
        }
        resp.body_mut().read_json().context("parsing refresh response")
    }

    fn get_json<T: DeserializeOwned>(&self, url: &str, token: &str) -> Result<T> {
        let mut resp = self
            .agent
            .get(url)
            .header("Authorization", &format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .header("anthropic-beta", BETA)
            .call()
            .with_context(|| format!("GET {url}"))?;

        let status = resp.status().as_u16();
        if status == 401 {
            bail!(
                "token rejected (401); this account needs a fresh `claude /login` then `ccs add`"
            );
        }
        if status != 200 {
            let detail = resp.body_mut().read_to_string().unwrap_or_default();
            bail!("{url} returned {status}: {}", snippet(&detail));
        }
        resp.body_mut().read_json().with_context(|| format!("parsing response from {url}"))
    }
}

/// Fold refreshed tokens into a credential blob, preserving every field the
/// refresh response does not speak to.
pub fn refreshed_oauth(prev: &Oauth, next: &Refreshed) -> Oauth {
    Oauth {
        access_token: next.access_token.clone(),
        refresh_token: next.refresh_token.clone().unwrap_or_else(|| prev.refresh_token.clone()),
        expires_at: now_ms() + next.expires_in.map_or(FALLBACK_LIFETIME_MS, |s| s * 1000),
        ..prev.clone()
    }
}

/// Error bodies are occasionally enormous; one line of it is enough to act on.
fn snippet(body: &str) -> String {
    let line = body.trim().lines().next().unwrap_or_default();
    match line.char_indices().nth(200) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    fn previous() -> Oauth {
        let mut extra = Map::new();
        extra.insert("rateLimitTier".into(), "default_claude_max_20x".into());
        Oauth {
            access_token: "old-access".into(),
            refresh_token: "old-refresh".into(),
            expires_at: 0,
            scopes: vec!["user:inference".into()],
            subscription_type: Some("max".into()),
            extra,
        }
    }

    #[test]
    fn a_rotated_refresh_token_replaces_the_old_one() {
        let next = Refreshed {
            access_token: "new-access".into(),
            refresh_token: Some("new-refresh".into()),
            expires_in: Some(3600),
        };
        let folded = refreshed_oauth(&previous(), &next);
        assert_eq!(folded.access_token, "new-access");
        assert_eq!(folded.refresh_token, "new-refresh");
    }

    #[test]
    fn an_unrotated_refresh_token_is_kept_rather_than_blanked() {
        let next = Refreshed {
            access_token: "new-access".into(),
            refresh_token: None,
            expires_in: Some(60),
        };
        assert_eq!(refreshed_oauth(&previous(), &next).refresh_token, "old-refresh");
    }

    #[test]
    fn refreshing_preserves_every_field_the_response_is_silent_about() {
        let next =
            Refreshed { access_token: "new".into(), refresh_token: None, expires_in: Some(60) };
        let folded = refreshed_oauth(&previous(), &next);
        assert_eq!(folded.scopes, ["user:inference"]);
        assert_eq!(folded.subscription_type.as_deref(), Some("max"));
        assert_eq!(folded.rate_limit_tier(), Some("default_claude_max_20x"));
    }

    #[test]
    fn a_stated_lifetime_is_honoured() {
        let next =
            Refreshed { access_token: "new".into(), refresh_token: None, expires_in: Some(3600) };
        let slack = (refreshed_oauth(&previous(), &next).expires_at - now_ms() - 3_600_000).abs();
        assert!(slack < 5_000, "expiry should track the stated lifetime, off by {slack}ms");
    }

    #[test]
    fn a_missing_lifetime_falls_back_short_rather_than_optimistic() {
        let next = Refreshed { access_token: "new".into(), refresh_token: None, expires_in: None };
        let folded = refreshed_oauth(&previous(), &next);
        assert!(folded.expires_at <= now_ms() + FALLBACK_LIFETIME_MS);
        assert!(folded.expires_at > now_ms());
    }
}
