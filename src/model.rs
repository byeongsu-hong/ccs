//! Domain types: the credential blob Claude Code stores on disk, the OAuth
//! API's responses, and the health verdicts derived from them.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// How long before genuine expiry an access token is treated as spent. A token
/// that expires mid-flight costs a retry, so buy it back cheaply up front.
const REFRESH_SKEW_MS: i64 = 60_000;

/// Percentage at or above which a limit has nothing left to give.
const EXHAUSTED_PCT: f64 = 100.0;

/// Percentage at or above which a limit is worth warning about.
const WARN_PCT: f64 = 80.0;

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

// ── credentials on disk ──────────────────────────────────────────────────────

/// The `claudeAiOauth` object inside Claude Code's credentials file.
///
/// Fields this tool has no opinion about are captured in `extra` and written
/// back verbatim, so a round-trip through the stash never drops state belonging
/// to a newer Claude Code than the one this was built against.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Oauth {
    #[serde(rename = "accessToken")]
    pub access_token: String,
    #[serde(rename = "refreshToken")]
    pub refresh_token: String,
    /// Unix epoch milliseconds.
    #[serde(rename = "expiresAt")]
    pub expires_at: i64,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(rename = "subscriptionType", default, skip_serializing_if = "Option::is_none")]
    pub subscription_type: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Oauth {
    /// Whether the access token is spent, or close enough that refreshing now
    /// beats discovering it mid-request.
    pub fn needs_refresh(&self) -> bool {
        self.expires_at - now_ms() < REFRESH_SKEW_MS
    }

    /// Claude Code records the plan tier here on some versions and in the
    /// profile on others; prefer whichever is present.
    pub fn rate_limit_tier(&self) -> Option<&str> {
        self.extra.get("rateLimitTier").and_then(Value::as_str)
    }
}

/// Claude Code's credentials file as a whole.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredsFile {
    #[serde(rename = "claudeAiOauth")]
    pub oauth: Oauth,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl CredsFile {
    pub fn new(oauth: Oauth) -> Self {
        Self { oauth, extra: Map::new() }
    }
}

// ── stash entries ────────────────────────────────────────────────────────────

/// One stashed account: the identity used to label it, plus the credentials
/// needed to become it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub email: String,
    pub uuid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_tier: Option<String>,
    pub added_at: String,
    pub oauth: Oauth,
}

impl Account {
    pub fn plan_label(&self) -> String {
        let tier = self.rate_limit_tier.as_deref().or_else(|| self.oauth.rate_limit_tier());
        plan_label(tier, self.plan.as_deref())
    }
}

/// Short plan label for a column of them: the rate limit tier reads better than
/// the subscription type ("max20x" over "max"), so prefer it, and fall back to
/// whatever names the plan at all.
pub fn plan_label(tier: Option<&str>, plan: Option<&str>) -> String {
    match tier {
        Some(t) => t.trim_start_matches("default_claude_").replace('_', ""),
        None => plan.unwrap_or("?").to_string(),
    }
}

/// A stashed account together with the slug naming its file.
#[derive(Debug, Clone)]
pub struct Stashed {
    pub slug: String,
    pub account: Account,
}

// ── API responses ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub account: ProfileAccount,
    #[serde(default)]
    pub organization: Option<ProfileOrg>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileAccount {
    pub uuid: String,
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileOrg {
    #[serde(default)]
    pub organization_type: Option<String>,
    #[serde(default)]
    pub rate_limit_tier: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageResponse {
    #[serde(default)]
    pub limits: Vec<Limit>,
}

/// One rate limit as the API reports it. The set is self-describing rather
/// than fixed, so new limit families and newly scoped models surface without
/// a code change here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Limit {
    pub kind: String,
    #[serde(default)]
    pub percent: f64,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub resets_at: Option<String>,
    #[serde(default)]
    pub scope: Option<LimitScope>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitScope {
    #[serde(default)]
    pub model: Option<LimitModel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitModel {
    #[serde(default)]
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Ok,
    Warn,
    Critical,
}

impl Limit {
    /// The model this limit is scoped to, if it is scoped to one at all.
    pub fn model_name(&self) -> Option<&str> {
        self.scope.as_ref()?.model.as_ref()?.display_name.as_deref()
    }

    /// The column this limit belongs under.
    pub fn column(&self) -> String {
        if let Some(model) = self.model_name() {
            return model.to_string();
        }
        match self.kind.as_str() {
            "session" => "session".to_string(),
            "weekly_all" => "weekly".to_string(),
            other => other.replace('_', " "),
        }
    }

    /// Nothing left on this limit until it resets.
    pub fn exhausted(&self) -> bool {
        self.percent >= EXHAUSTED_PCT
    }

    /// The API's own severity is authoritative where it is reported; the
    /// percentage is the fallback for limit families that omit it.
    pub fn health(&self) -> Health {
        if self.exhausted() {
            return Health::Critical;
        }
        match self.severity.as_deref() {
            Some("critical") | Some("warning") => Health::Warn,
            Some("normal") => Health::Ok,
            _ if self.percent >= WARN_PCT => Health::Warn,
            _ => Health::Ok,
        }
    }
}

/// Build a `Limit` fixture. The field-by-field literal is unreadable repeated
/// a dozen times over, and every test wants a different two fields of it:
/// `limit!("session", 3.0)`, `limit!("weekly_scoped", 100.0, model = "Fable")`.
#[cfg(test)]
#[macro_export]
macro_rules! limit {
    ($kind:expr, $percent:expr $(, model = $model:expr)? $(, severity = $severity:expr)?
        $(, resets = $resets:expr)?) => {{
        #[allow(unused_mut)]
        let mut built = $crate::model::Limit {
            kind: $kind.to_string(),
            percent: $percent,
            severity: None,
            resets_at: None,
            scope: None,
        };
        $(
            built.scope = Some($crate::model::LimitScope {
                model: Some($crate::model::LimitModel {
                    display_name: Some($model.to_string()),
                }),
            });
        )?
        $( built.severity = Some($severity.to_string()); )?
        $( built.resets_at = Some($resets.to_string()); )?
        built
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oauth(expires_at: i64) -> Oauth {
        Oauth {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at,
            scopes: vec!["user:inference".into()],
            subscription_type: None,
            extra: Map::new(),
        }
    }

    #[test]
    fn column_names_the_model_for_scoped_limits() {
        assert_eq!(limit!("weekly_scoped", 100.0, model = "Fable").column(), "Fable");
    }

    #[test]
    fn column_folds_the_two_unscoped_families_to_short_names() {
        assert_eq!(limit!("session", 3.0).column(), "session");
        assert_eq!(limit!("weekly_all", 53.0).column(), "weekly");
    }

    #[test]
    fn column_falls_back_to_a_readable_form_of_an_unknown_kind() {
        assert_eq!(limit!("monthly_scoped", 1.0).column(), "monthly scoped");
    }

    #[test]
    fn exhausted_only_at_the_full_hundred() {
        assert!(!limit!("session", 99.9).exhausted());
        assert!(limit!("session", 100.0).exhausted());
        assert!(limit!("session", 140.0).exhausted());
    }

    #[test]
    fn health_treats_a_spent_limit_as_critical_whatever_the_severity() {
        assert_eq!(limit!("session", 100.0, severity = "normal").health(), Health::Critical);
    }

    #[test]
    fn health_defers_to_the_reported_severity_below_the_cap() {
        assert_eq!(limit!("weekly_all", 53.0, severity = "normal").health(), Health::Ok);
        assert_eq!(limit!("weekly_all", 12.0, severity = "critical").health(), Health::Warn);
    }

    #[test]
    fn health_falls_back_to_the_percentage_when_severity_is_absent() {
        assert_eq!(limit!("session", 85.0).health(), Health::Warn);
        assert_eq!(limit!("session", 20.0).health(), Health::Ok);
    }

    #[test]
    fn a_token_expiring_inside_the_skew_needs_refreshing() {
        assert!(oauth(now_ms() + 5_000).needs_refresh());
        assert!(oauth(now_ms() - 1).needs_refresh());
        assert!(!oauth(now_ms() + 10 * 60 * 1000).needs_refresh());
    }

    #[test]
    fn a_plan_label_prefers_the_tier_and_reads_it_short() {
        assert_eq!(plan_label(Some("default_claude_max_20x"), Some("max")), "max20x");
    }

    #[test]
    fn a_plan_label_falls_back_to_the_plan_then_to_a_question_mark() {
        assert_eq!(plan_label(None, Some("pro")), "pro");
        assert_eq!(plan_label(None, None), "?");
    }

    #[test]
    fn unknown_credential_fields_survive_a_round_trip() {
        let raw = r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r","expiresAt":1,
            "scopes":["s"],"subscriptionType":"max","rateLimitTier":"default_claude_max_20x",
            "refreshTokenExpiresAt":9},"somethingNewer":{"x":1}}"#;
        let parsed: CredsFile = serde_json::from_str(raw).expect("parses");
        let back = serde_json::to_value(&parsed).expect("serialises");

        assert_eq!(back["claudeAiOauth"]["rateLimitTier"], "default_claude_max_20x");
        assert_eq!(back["claudeAiOauth"]["refreshTokenExpiresAt"], 9);
        assert_eq!(back["somethingNewer"]["x"], 1);
    }
}
