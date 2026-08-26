//! Command implementations: the orchestration between the CLI surface and the
//! credential store, the stash, and the API.
//!
//! Collaborators arrive through `Ctx` rather than being reached for, so each
//! command stays testable against substitutes.

use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::thread;

use anyhow::{Context as _, Result, bail};
use jiff::Timestamp;
use serde::Serialize;

use crate::api::{Api, refreshed_oauth};
use crate::creds::CredStore;
use crate::lock;
use crate::login;
use crate::model::{Account, CredsFile, Limit, Oauth, Stashed};
use crate::picker::{self, Outcome};
use crate::render::{self, Entry, Style, Table};
use crate::stash::{self, Stash};

pub struct Ctx<'a> {
    pub creds: &'a dyn CredStore,
    pub stash: &'a Stash,
    pub api: &'a Api,
    /// Directory whose lock guards credential writes.
    pub config_dir: &'a Path,
}

// ── probing ─────────────────────────────────────────────────────────────────

/// The outcome of asking one account how it is doing.
struct Probe {
    slug: String,
    /// Present when the access token had to be refreshed. The caller must
    /// persist it: the refresh may have rotated the refresh token, and a
    /// rotated token that is not written down costs an interactive re-login.
    refreshed: Option<Oauth>,
    limits: Result<Vec<Limit>, String>,
}

/// Ask one account for its limits, refreshing its access token first if the
/// token is spent. Network only — persistence is the caller's job.
fn probe(api: &Api, entry: &Stashed) -> Probe {
    let slug = entry.slug.clone();
    let refreshed = match freshen(api, &entry.account.oauth) {
        Ok(refreshed) => refreshed,
        Err(e) => return Probe { slug, refreshed: None, limits: Err(describe(&e)) },
    };
    let oauth = refreshed.as_ref().unwrap_or(&entry.account.oauth);
    let limits = api.usage(&oauth.access_token).map(|u| u.limits).map_err(|e| describe(&e));
    Probe { slug, refreshed, limits }
}

fn probe_all(api: &Api, accounts: &[Stashed]) -> Vec<Probe> {
    thread::scope(|scope| {
        let running: Vec<_> =
            accounts.iter().map(|entry| scope.spawn(move || probe(api, entry))).collect();
        running.into_iter().map(|h| h.join().expect("probe thread panicked")).collect()
    })
}

/// Give back a refreshed credential blob when the current one is spent.
fn freshen(api: &Api, oauth: &Oauth) -> Result<Option<Oauth>> {
    if !oauth.needs_refresh() {
        return Ok(None);
    }
    let next = api.refresh(&oauth.refresh_token, &oauth.scopes)?;
    Ok(Some(refreshed_oauth(oauth, &next)))
}

/// Write back every token a probe refreshed.
///
/// When the refreshed account is the one currently installed, the live
/// credentials are updated too: the refresh rotated the token running sessions
/// are holding, and leaving them behind would break them.
fn persist(ctx: &Ctx, accounts: &[Stashed], probes: &[Probe], live: Option<&str>) -> Result<()> {
    for probe in probes {
        let Some(oauth) = &probe.refreshed else { continue };
        let Some(entry) = accounts.iter().find(|a| a.slug == probe.slug) else { continue };
        ctx.stash.save(&probe.slug, &Account { oauth: oauth.clone(), ..entry.account.clone() })?;

        if live == Some(probe.slug.as_str()) {
            let _guard = lock::acquire(ctx.config_dir)?;
            let current = ctx.creds.read()?;
            ctx.creds.write(&merged(current, oauth))?;
        }
    }
    Ok(())
}

/// Which stashed account the live credentials belong to.
///
/// A token match is direct evidence and wins; the recorded pointer is the
/// fallback for the moment right after a refresh rotated one copy.
fn identify(ctx: &Ctx, accounts: &[Stashed], live: Option<&CredsFile>) -> Option<String> {
    let live = live?;
    let matched = accounts.iter().find(|a| {
        a.account.oauth.refresh_token == live.oauth.refresh_token
            || a.account.oauth.access_token == live.oauth.access_token
    });
    if let Some(entry) = matched {
        return Some(entry.slug.clone());
    }
    let recorded = ctx.stash.active()?;
    accounts.iter().any(|a| a.slug == recorded).then_some(recorded)
}

/// Put `oauth` into the live credentials, keeping every unrelated field the
/// existing file carries.
fn merged(current: Option<CredsFile>, oauth: &Oauth) -> CredsFile {
    match current {
        Some(file) => CredsFile { oauth: oauth.clone(), ..file },
        None => CredsFile::new(oauth.clone()),
    }
}

// ── commands ────────────────────────────────────────────────────────────────

pub fn list(ctx: &Ctx, json: bool) -> Result<()> {
    let accounts = ctx.stash.list()?;
    if accounts.is_empty() {
        bail!("no accounts stashed yet; log in with `claude` then run `ccs add`");
    }
    let (table, _) = survey(ctx, &accounts, Style::detect())?;
    if json {
        return emit_json(&table);
    }
    println!("{}", table.header());
    for index in 0..table.len() {
        println!("{}", table.row(index));
    }
    Ok(())
}

pub fn status(ctx: &Ctx, json: bool) -> Result<()> {
    let Some(live) = ctx.creds.read()? else {
        bail!("no credentials at {}; run `claude` and sign in first", ctx.creds.path().display());
    };
    let style = Style::detect();

    let refreshed = freshen(ctx.api, &live.oauth)?;
    if let Some(oauth) = &refreshed {
        let _guard = lock::acquire(ctx.config_dir)?;
        ctx.creds.write(&merged(Some(live.clone()), oauth))?;
    }
    // Identify against what was on disk, before the refreshed token replaces it.
    let accounts = ctx.stash.list()?;
    let slug = identify(ctx, &accounts, Some(&live)).unwrap_or_default();
    let oauth = refreshed.unwrap_or(live.oauth);

    let profile = ctx.api.profile(&oauth.access_token)?;
    let limits = ctx.api.usage(&oauth.access_token)?.limits;
    let plan = profile
        .organization
        .as_ref()
        .and_then(|o| o.rate_limit_tier.clone())
        .map(|t| t.trim_start_matches("default_claude_").replace('_', ""))
        .unwrap_or_else(|| "?".to_string());

    let entry =
        Entry { slug, email: profile.account.email, plan, active: true, limits: Ok(limits) };
    if json {
        println!("{}", serde_json::to_string_pretty(&view(&entry))?);
        return Ok(());
    }
    println!("{}  {}", style.bold(&entry.email), style.dim(&entry.plan));
    for line in render::detail(&entry, style) {
        println!("{line}");
    }
    Ok(())
}

/// Stash an account. Two ways in: log in to another one without disturbing the
/// account in use, or capture whichever account is in use right now.
pub fn add(ctx: &Ctx, name: Option<&str>, current: bool, options: &login::Options) -> Result<()> {
    let oauth = match current {
        true => capture_live(ctx)?,
        false => {
            println!("logging in to another account; the one in use is not affected");
            login::run(ctx.stash.root(), &login::binary(), options)?
        }
    };
    let recorded = record(ctx, oauth, name, Installed::from(current))?;
    let verb = if recorded.replaced { "refreshed" } else { "stashed" };
    let (email, slug) = (&recorded.stashed.account.email, &recorded.stashed.slug);

    match current {
        true => println!("{verb} the account in use, {email}, as {slug}"),
        false => println!("{verb} {email} as {slug}; `ccs use {slug}` to switch to it"),
    }
    Ok(())
}

/// Whether the credentials being stashed are the ones currently installed.
///
/// A login mints an account that is stashed but *not* in use. Recording it as
/// the installed one would make the next switch fold the live tokens into the
/// wrong entry, so the two paths have to stay distinguishable.
#[derive(PartialEq)]
enum Installed {
    Yes,
    No,
}

impl From<bool> for Installed {
    fn from(current: bool) -> Self {
        match current {
            true => Self::Yes,
            false => Self::No,
        }
    }
}

struct Recorded {
    stashed: Stashed,
    /// The slug already held an account, so this refreshed it in place rather
    /// than adding one.
    replaced: bool,
}

/// The credentials in use, refreshed if they were spent.
fn capture_live(ctx: &Ctx) -> Result<Oauth> {
    let Some(live) = ctx.creds.read()? else {
        bail!(
            "no credentials at {}; sign in with `claude` before `ccs add --current`",
            ctx.creds.path().display()
        );
    };
    // A refresh here rotates the token running sessions hold, so mirror it back
    // before doing anything else with it.
    let refreshed = freshen(ctx.api, &live.oauth)?;
    if let Some(oauth) = &refreshed {
        let _guard = lock::acquire(ctx.config_dir)?;
        ctx.creds.write(&merged(Some(live.clone()), oauth))?;
    }
    Ok(refreshed.unwrap_or(live.oauth))
}

/// Ask who some credentials belong to, then write them into the stash.
fn record(ctx: &Ctx, oauth: Oauth, name: Option<&str>, installed: Installed) -> Result<Recorded> {
    let profile = ctx.api.profile(&oauth.access_token)?;
    let slug = name.map(str::to_string).unwrap_or_else(|| stash::slugify(&profile.account.email));
    let replaced = ctx.stash.list()?.iter().any(|s| s.slug == slug);
    let organization = profile.organization.as_ref();

    let account = Account {
        email: profile.account.email,
        uuid: profile.account.uuid,
        plan: organization.and_then(|o| o.organization_type.clone()),
        rate_limit_tier: organization.and_then(|o| o.rate_limit_tier.clone()),
        added_at: Timestamp::now().to_string(),
        oauth,
    };
    ctx.stash.save(&slug, &account)?;
    if installed == Installed::Yes {
        ctx.stash.set_active(&slug)?;
    }
    Ok(Recorded { stashed: Stashed { slug, account }, replaced })
}

pub fn remove(ctx: &Ctx, needle: &str) -> Result<()> {
    let accounts = ctx.stash.list()?;
    let target = stash::resolve(&accounts, needle)?.clone();
    ctx.stash.remove(&target.slug)?;
    println!("forgot {} ({})", target.account.email, target.slug);
    Ok(())
}

pub fn use_account(ctx: &Ctx, needle: &str, force: bool) -> Result<()> {
    let accounts = ctx.stash.list()?;
    let target = stash::resolve(&accounts, needle)?.clone();

    let live = identify_live(ctx, &accounts)?;
    let probe = probe(ctx.api, &target);
    persist(ctx, &accounts, std::slice::from_ref(&probe), live.as_deref())?;

    guard_exhausted(&entry_of(&target, &probe, false), force)?;
    switch_to(ctx, &accounts, &target)
}

pub fn pick(ctx: &Ctx) -> Result<()> {
    let accounts = ctx.stash.list()?;
    if accounts.is_empty() {
        bail!("no accounts stashed yet; log in with `claude` then run `ccs add`");
    }
    let mut cursor = 0;
    loop {
        let (table, _) = survey(ctx, &accounts, Style::colored())?;
        cursor = cursor.min(table.len().saturating_sub(1));
        match picker::run(&table, cursor)? {
            Outcome::Quit => return Ok(()),
            Outcome::Refresh(at) => cursor = at,
            Outcome::Switch(at) => {
                let Some(entry) = table.entries().get(at) else { return Ok(()) };
                let Some(target) = accounts.iter().find(|a| a.slug == entry.slug) else {
                    return Ok(());
                };
                guard_exhausted(entry, false)?;
                return switch_to(ctx, &accounts, target);
            }
        }
    }
}

// ── shared steps ────────────────────────────────────────────────────────────

/// Probe every account, write back anything that got refreshed, and lay the
/// results out as a table.
fn survey(ctx: &Ctx, accounts: &[Stashed], style: Style) -> Result<(Table, Option<String>)> {
    let live = identify_live(ctx, accounts)?;
    let probes = probe_all(ctx.api, accounts);
    persist(ctx, accounts, &probes, live.as_deref())?;

    let entries = accounts
        .iter()
        .zip(&probes)
        .map(|(account, probe)| {
            entry_of(account, probe, live.as_deref() == Some(account.slug.as_str()))
        })
        .collect();
    Ok((Table::build(entries, style), live))
}

fn identify_live(ctx: &Ctx, accounts: &[Stashed]) -> Result<Option<String>> {
    let live = ctx.creds.read()?;
    Ok(identify(ctx, accounts, live.as_ref()))
}

/// Install `target` as the live credentials.
///
/// The outgoing account's tokens are folded back into the stash first: Claude
/// Code refreshes tokens in place, so the stashed copy goes stale the moment an
/// account is used. Capturing it on the way out is what keeps a stashed account
/// usable without a fresh login.
fn switch_to(ctx: &Ctx, accounts: &[Stashed], target: &Stashed) -> Result<()> {
    let _guard = lock::acquire(ctx.config_dir)?;
    let live = ctx.creds.read()?;
    if let Some(live) = &live {
        capture_outgoing(ctx, accounts, live, &target.slug)?;
    }
    ctx.creds.write(&merged(live, &target.account.oauth))?;
    ctx.stash.set_active(&target.slug)?;
    println!("switched to {} ({})", target.account.email, target.slug);
    println!("running sessions pick this up within a few seconds");
    Ok(())
}

fn capture_outgoing(
    ctx: &Ctx,
    accounts: &[Stashed],
    live: &CredsFile,
    incoming: &str,
) -> Result<()> {
    let Some(slug) = identify(ctx, accounts, Some(live)) else { return Ok(()) };
    if slug == incoming {
        return Ok(());
    }
    let Some(entry) = accounts.iter().find(|a| a.slug == slug) else { return Ok(()) };
    ctx.stash.save(&slug, &Account { oauth: live.oauth.clone(), ..entry.account.clone() })
}

/// Refuse to walk into an account with nothing left, unless told to.
fn guard_exhausted(entry: &Entry, force: bool) -> Result<()> {
    if force {
        return Ok(());
    }
    let spent = entry.exhausted();
    if spent.is_empty() {
        return Ok(());
    }
    let spent = spent.join(", ");
    if !confirm(&format!("{} has no {spent} left. Switch anyway?", entry.email))? {
        bail!("not switching; `ccs use <account> --force` overrides");
    }
    Ok(())
}

fn confirm(question: &str) -> Result<bool> {
    if !io::stdin().is_terminal() {
        return Ok(false);
    }
    print!("{question} [y/N] ");
    io::stdout().flush().context("prompting")?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).context("reading answer")?;
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
}

// ── machine-readable output ─────────────────────────────────────────────────

#[derive(Serialize)]
struct View<'a> {
    slug: &'a str,
    email: &'a str,
    plan: &'a str,
    active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
    limits: &'a [Limit],
}

fn view(entry: &Entry) -> View<'_> {
    View {
        slug: &entry.slug,
        email: &entry.email,
        plan: &entry.plan,
        active: entry.active,
        error: entry.limits.as_ref().err().map(String::as_str),
        limits: entry.known(),
    }
}

fn emit_json(table: &Table) -> Result<()> {
    let views: Vec<View> = table.entries().iter().map(view).collect();
    println!("{}", serde_json::to_string_pretty(&views)?);
    Ok(())
}

/// Lay one account and its probe out as a table entry. The single place that
/// mapping happens, so the picker, the table and the switch guard all agree.
fn entry_of(account: &Stashed, probe: &Probe, active: bool) -> Entry {
    Entry {
        slug: account.slug.clone(),
        email: account.account.email.clone(),
        plan: account.account.plan_label(),
        active,
        limits: probe.limits.clone(),
    }
}

fn describe(error: &anyhow::Error) -> String {
    error.chain().map(|c| c.to_string()).collect::<Vec<_>>().join(": ")
}
