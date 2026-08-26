//! Command implementations: the orchestration between the CLI surface and the
//! credential store, the stash, and the API.
//!
//! Collaborators arrive through `Ctx` rather than being reached for, so each
//! command stays testable against substitutes.

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::thread;

use anyhow::{Context as _, Result, bail};
use jiff::Timestamp;
use serde::Serialize;

use crate::api::{Api, refreshed_oauth};
use crate::creds::{self, CredStore, FileStore};
use crate::lock;
use crate::login;
use crate::model::{Account, CredsFile, Limit, Oauth, Stashed, plan_label};
use crate::pen;
use crate::picker::{self, Outcome};
use crate::render::{self, Entry, Style, Table, Verb};
use crate::stash::{self, Stash};

pub struct Ctx<'a> {
    pub creds: &'a dyn CredStore,
    pub stash: &'a Stash,
    pub api: &'a Api,
    /// The real configuration: where the stash lives and what pens are cut
    /// from. The same directory `creds` reads, unless this process is itself
    /// in a pen.
    pub home: &'a pen::Home,
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

/// Write back every token a probe refreshed: into the stash, into `accounts`
/// itself, and into the live credentials when the account is the installed one.
///
/// Updating `accounts` in place is what stops a later step installing the copy
/// that was read before the probe. A refresh can rotate the refresh token, and
/// the superseded one no longer buys anything — handing it to a session costs
/// that session an interactive re-login.
fn persist(
    ctx: &Ctx,
    accounts: &mut [Stashed],
    probes: &[Probe],
    live: Option<&str>,
) -> Result<()> {
    for probe in probes {
        let Some(oauth) = &probe.refreshed else { continue };
        let Some(entry) = accounts.iter_mut().find(|a| a.slug == probe.slug) else { continue };
        entry.account.oauth = oauth.clone();
        ctx.stash.save(&probe.slug, &entry.account)?;

        // The refresh rotated the token running sessions are holding, so they
        // cannot be left behind on it.
        if live == Some(probe.slug.as_str()) {
            install(ctx.creds, oauth)?;
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

/// Put `oauth` into a credentials file, keeping every unrelated field the
/// existing one carries.
fn merged(current: Option<CredsFile>, oauth: &Oauth) -> CredsFile {
    match current {
        Some(file) => CredsFile { oauth: oauth.clone(), ..file },
        None => CredsFile::new(oauth.clone()),
    }
}

/// The directory whose lock guards writes to a store's credentials, which is
/// the one it keeps them in.
fn guarded(store: &dyn CredStore) -> Result<&Path> {
    store.path().parent().context("credentials path has no directory")
}

/// Install `oauth` into a credential store, under that lock.
fn install(store: &dyn CredStore, oauth: &Oauth) -> Result<()> {
    let _guard = lock::acquire(guarded(store)?)?;
    let current = store.read()?;
    store.write(&merged(current, oauth))
}

/// The credentials in use, together with their freshest access token.
///
/// The file comes back as it was read rather than as it now stands, because
/// working out which stashed account it is has to match against the tokens the
/// stash still holds. A refresh here rotates the token running sessions are
/// holding, so it is mirrored back to them before anything else happens.
fn in_use(ctx: &Ctx, hint: &str) -> Result<(CredsFile, Oauth)> {
    let Some(file) = ctx.creds.read()? else {
        bail!("no credentials at {}; {hint}", ctx.creds.path().display());
    };
    let Some(oauth) = freshen(ctx.api, &file.oauth)? else {
        let oauth = file.oauth.clone();
        return Ok((file, oauth));
    };
    install(ctx.creds, &oauth)?;
    Ok((file, oauth))
}

// ── commands ────────────────────────────────────────────────────────────────

pub fn list(ctx: &Ctx, json: bool) -> Result<()> {
    let mut accounts = stashed(ctx)?;
    let (table, _) = survey(ctx, &mut accounts, Style::detect())?;
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
    let style = Style::detect();
    warn_overridden();

    let (file, oauth) = in_use(ctx, "run `claude` and sign in first")?;
    let accounts = ctx.stash.list()?;
    let slug = identify(ctx, &accounts, Some(&file)).unwrap_or_default();

    let profile = ctx.api.profile(&oauth.access_token)?;
    let limits = ctx.api.usage(&oauth.access_token)?.limits;
    let organization = profile.organization.as_ref();
    let plan = plan_label(
        organization.and_then(|o| o.rate_limit_tier.as_deref()),
        organization.and_then(|o| o.organization_type.as_deref()),
    );

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
        true => in_use(ctx, "sign in with `claude` before `ccs add --current`")?.1,
        false => {
            println!("logging in to another account; the one in use is not affected");
            login::run(ctx.stash.root(), &claude_binary(), options)?
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

/// Which Claude Code to drive, overridable for a non-standard install. Held by
/// the orchestrator because both the login and the pinned session are launched
/// from here.
fn claude_binary() -> String {
    std::env::var("CCS_CLAUDE_BINARY").unwrap_or_else(|_| "claude".to_string())
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
    if let Err(e) = pen::discard(ctx.stash.root(), &target.slug) {
        eprintln!("ccs: {}'s pen is still there: {}", target.slug, describe(&e));
    }
    println!("forgot {} ({})", target.account.email, target.slug);
    Ok(())
}

pub fn use_account(ctx: &Ctx, needle: &str, force: bool) -> Result<()> {
    let mut accounts = ctx.stash.list()?;
    let slug = stash::resolve(&accounts, needle)?.slug.clone();

    let live = identify_live(ctx, &accounts)?;
    let probe = probe(ctx.api, stash::resolve(&accounts, &slug)?);
    persist(ctx, &mut accounts, std::slice::from_ref(&probe), live.as_deref())?;

    // Read after the probe has been folded back in, never before it: the probe
    // may have refreshed this very account, and the copy taken beforehand
    // carries the tokens that refresh superseded.
    let target = stash::resolve(&accounts, &slug)?.clone();
    guard_exhausted(&entry_of(&target, &probe, false), force)?;
    switch_to(ctx, &accounts, &target)
}

pub fn pick(ctx: &Ctx) -> Result<()> {
    let mut accounts = stashed(ctx)?;
    let Some(target) = choose(ctx, &mut accounts, Verb::Switch)? else { return Ok(()) };
    switch_to(ctx, &accounts, &target)
}

/// Confine a session to one account: install that account's credentials in its
/// own pen and hand the process over to Claude Code there.
///
/// Nothing outside the pen is touched, so every other session stays on the
/// account in use and a later `ccs use` leaves this one where it is.
pub fn pin(ctx: &Ctx, needle: Option<&str>, args: &[String]) -> Result<()> {
    let mut accounts = stashed(ctx)?;

    // Named outright, the account is taken at its word and the session starts
    // without a round trip; the picker is where usage is shopped for.
    let target = match needle {
        Some(needle) => stash::resolve(&accounts, needle)?.clone(),
        None => {
            let Some(chosen) = choose(ctx, &mut accounts, Verb::Launch)? else { return Ok(()) };
            chosen
        }
    };

    let pen = pen_for(ctx, &target)?;
    warn_overridden();
    println!("{} is pinned to this session only", target.account.email);
    pen::launch(&pen, &claude_binary(), &labelled(args, &target.account.email))
}

/// Have the session say which account it is confined to, where Claude Code
/// shows a session's name. A launch that named itself keeps its own name.
fn labelled(args: &[String], email: &str) -> Vec<String> {
    if args.iter().any(|a| a == "--name" || a == "-n") {
        return args.to_vec();
    }
    let mut out = vec!["--name".to_string(), format!("pinned by ccs: {email}")];
    out.extend_from_slice(args);
    out
}

/// The account's pen, with its freshest credentials in place.
fn pen_for(ctx: &Ctx, target: &Stashed) -> Result<PathBuf> {
    let oauth = match freshen(ctx.api, &target.account.oauth)? {
        Some(oauth) => {
            let account = Account { oauth: oauth.clone(), ..target.account.clone() };
            ctx.stash.save(&target.slug, &account)?;
            oauth
        }
        None => target.account.oauth.clone(),
    };

    let pen = pen::prepare(ctx.home, ctx.stash.root(), &target.slug)?;
    install(&FileStore::new(&pen), &oauth)?;
    Ok(pen)
}

/// Say so when the environment answers for the credentials, because it decides
/// the account whatever any credentials file holds — the live one and a pen's
/// alike.
fn warn_overridden() {
    let set: Vec<&str> =
        creds::OVERRIDING.iter().copied().filter(|k| std::env::var_os(k).is_some()).collect();
    if set.is_empty() {
        return;
    }
    eprintln!(
        "ccs: {} takes precedence over the credentials file; a session that inherits it uses \
         that account instead",
        set.join(", ")
    );
}

/// Put the accounts on screen and wait for one to be picked. The picker owns
/// the whole screen while it is up, so it is always drawn in colour.
fn choose(ctx: &Ctx, accounts: &mut [Stashed], verb: Verb) -> Result<Option<Stashed>> {
    let outcome =
        picker::run(|| survey(ctx, accounts, Style::colored()).map(|(table, _)| table), verb)?;
    let Outcome::Chose(slug) = outcome else { return Ok(None) };
    Ok(accounts.iter().find(|a| a.slug == slug).cloned())
}

/// Every stashed account, refusing to go further when there are none.
fn stashed(ctx: &Ctx) -> Result<Vec<Stashed>> {
    let accounts = ctx.stash.list()?;
    if accounts.is_empty() {
        bail!("no accounts stashed yet; log in with `claude` then run `ccs add`");
    }
    Ok(accounts)
}

// ── shared steps ────────────────────────────────────────────────────────────

/// Probe every account, write back anything that got refreshed, and lay the
/// results out as a table.
fn survey(ctx: &Ctx, accounts: &mut [Stashed], style: Style) -> Result<(Table, Option<String>)> {
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
    let _guard = lock::acquire(guarded(ctx.creds)?)?;
    let live = ctx.creds.read()?;
    if let Some(live) = &live {
        capture_outgoing(ctx, accounts, live, &target.slug)?;
    }
    ctx.creds.write(&merged(live, &target.account.oauth))?;
    ctx.stash.set_active(&target.slug)?;
    println!("switched to {} ({})", target.account.email, target.slug);
    println!("running sessions pick this up on their next request");
    warn_overridden();
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
    if entry.exhausted().is_empty() {
        return Ok(());
    }
    if !confirm(&render::question(entry, Verb::Switch))? {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::fs;

    fn words(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    /// Credentials held in memory rather than on disk, so a test can see what
    /// a command installed.
    struct Recorder {
        path: PathBuf,
        held: RefCell<Option<CredsFile>>,
    }

    impl CredStore for Recorder {
        fn read(&self) -> Result<Option<CredsFile>> {
            Ok(self.held.borrow().clone())
        }

        fn write(&self, creds: &CredsFile) -> Result<()> {
            *self.held.borrow_mut() = Some(creds.clone());
            Ok(())
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    fn oauth(refresh_token: &str) -> Oauth {
        Oauth {
            access_token: format!("access-{refresh_token}"),
            refresh_token: refresh_token.to_string(),
            expires_at: 0,
            scopes: vec![],
            subscription_type: None,
            extra: serde_json::Map::new(),
        }
    }

    fn stashed(slug: &str, oauth: Oauth) -> Stashed {
        Stashed {
            slug: slug.into(),
            account: Account {
                email: format!("{slug}@example.com"),
                uuid: "u".into(),
                plan: None,
                rate_limit_tier: None,
                added_at: "2026-01-01T00:00:00Z".into(),
                oauth,
            },
        }
    }

    /// A rotated refresh token has to reach every copy at once: the stash, the
    /// live credentials, and the in-memory list the caller goes on to install
    /// from. A copy left on the superseded token buys nothing, and a session
    /// handed one is a session that has to log in again.
    #[test]
    fn a_refresh_reaches_every_copy_of_the_account_it_refreshed() {
        let dir = std::env::temp_dir().join(format!("ccs-cmd-persist-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("config directory");

        let creds = Recorder { path: dir.join(".credentials.json"), held: RefCell::new(None) };
        let stash = Stash::open(&dir).expect("stash");
        let api = Api::new();
        let home = pen::Home { config: dir.clone(), global: dir.join(".claude.json") };
        let ctx = Ctx { creds: &creds, stash: &stash, api: &api, home: &home };

        let mut accounts = vec![stashed("work", oauth("superseded"))];
        let probes = vec![Probe {
            slug: "work".into(),
            refreshed: Some(oauth("rotated")),
            limits: Ok(vec![]),
        }];
        persist(&ctx, &mut accounts, &probes, Some("work")).expect("persist");

        assert_eq!(accounts[0].account.oauth.refresh_token, "rotated", "the list the caller holds");
        let live = creds.read().expect("read").expect("credentials were installed");
        assert_eq!(live.oauth.refresh_token, "rotated", "the live credentials");
        let saved = stash.list().expect("list");
        assert_eq!(saved[0].account.oauth.refresh_token, "rotated", "the stash");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pinned_session_names_the_account_it_is_confined_to() {
        let args = labelled(&[], "work@example.com");
        assert_eq!(args, ["--name", "pinned by ccs: work@example.com"]);
    }

    #[test]
    fn the_label_goes_in_front_of_what_was_forwarded() {
        let args = labelled(&words(&["--continue"]), "work@example.com");
        assert_eq!(args.last().map(String::as_str), Some("--continue"));
    }

    #[test]
    fn a_launch_that_named_itself_keeps_its_own_name() {
        for flag in ["--name", "-n"] {
            let given = words(&[flag, "mine"]);
            assert_eq!(labelled(&given, "work@example.com"), given, "{flag} should win");
        }
    }
}
