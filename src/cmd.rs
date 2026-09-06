//! Command implementations: the orchestration between the CLI surface and the
//! credential store, the stash, and the API.
//!
//! Collaborators arrive through `Ctx` rather than being reached for, so each
//! command stays testable against substitutes.

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use jiff::Timestamp;
use serde::Serialize;

use crate::api::{Api, refreshed_oauth};
use crate::codex;
use crate::creds::{self, Backend, CredStore};
use crate::lock;
use crate::login;
use crate::model::{Account, CredsFile, Limit, Oauth, Provider, Stashed, plan_label};
use crate::notify;
use crate::pen;
use crate::picker::{self, Act, Outcome};
use crate::render::{self, Entry, Style, Table, Verb};
use crate::serve::{self, Grant};
use crate::stash::{self, Stash};
use crate::usage;
use crate::watch;

pub struct Ctx<'a> {
    pub creds: &'a dyn CredStore,
    /// Where credentials are kept on this machine, for the stores this reaches
    /// for itself: a pen's, and the one a login is captured from.
    pub backend: Backend,
    pub stash: &'a Stash,
    /// Where a poll's findings are left for readers that cannot make one.
    pub usage: &'a usage::Cache,
    pub api: &'a Api,
    /// Codex's live slot and the client that speaks to its endpoints.
    pub codex: &'a dyn codex::Creds,
    pub codex_api: &'a codex::Client,
    /// The real configuration: where the stash lives and what pens are cut
    /// from. The same directory `creds` reads, unless this process is itself
    /// in a pen.
    pub home: &'a pen::Home,
}

impl Ctx<'_> {
    fn apis(&self) -> Apis<'_> {
        Apis { claude: self.api, codex: self.codex_api }
    }
}

/// The clients each provider is spoken to with. Copyable so a probe can run
/// on a thread of its own without the rest of the context going with it.
#[derive(Clone, Copy)]
struct Apis<'a> {
    claude: &'a Api,
    codex: &'a codex::Client,
}

/// Which stashed account each provider's live slot holds, when it is known.
#[derive(Debug, Clone, Default, PartialEq)]
struct Live {
    claude: Option<String>,
    codex: Option<String>,
}

impl Live {
    fn of(&self, provider: Provider) -> Option<&str> {
        match provider {
            Provider::Claude => self.claude.as_deref(),
            Provider::Codex => self.codex.as_deref(),
        }
    }

    fn set(&mut self, provider: Provider, slug: Option<String>) {
        match provider {
            Provider::Claude => self.claude = slug,
            Provider::Codex => self.codex = slug,
        }
    }

    /// Whether `entry` is the account installed in its provider's slot.
    fn holds(&self, entry: &Stashed) -> bool {
        self.of(entry.account.provider) == Some(entry.slug.as_str())
    }
}

/// What each provider's live slot holds right now, read off disk.
struct Slots {
    claude: Option<Oauth>,
    codex: Option<Oauth>,
}

impl Slots {
    fn of(&self, provider: Provider) -> Option<&Oauth> {
        match provider {
            Provider::Claude => self.claude.as_ref(),
            Provider::Codex => self.codex.as_ref(),
        }
    }
}

fn slot(ctx: &Ctx, provider: Provider) -> Result<Option<Oauth>> {
    Ok(match provider {
        Provider::Claude => ctx.creds.read()?.map(|f| f.oauth),
        Provider::Codex => ctx.codex.read()?.and_then(|f| f.oauth()),
    })
}

fn slots(ctx: &Ctx) -> Result<Slots> {
    Ok(Slots { claude: slot(ctx, Provider::Claude)?, codex: slot(ctx, Provider::Codex)? })
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
fn probe(apis: Apis, entry: &Stashed) -> Probe {
    let slug = entry.slug.clone();
    let provider = entry.account.provider;
    let refreshed = match freshen(apis, provider, &entry.account.oauth) {
        Ok(refreshed) => refreshed,
        Err(e) => return Probe { slug, refreshed: None, limits: Err(describe(&e)) },
    };
    let oauth = refreshed.as_ref().unwrap_or(&entry.account.oauth);
    let limits = read_limits(apis, provider, oauth).map_err(|e| describe(&e));
    Probe { slug, refreshed, limits }
}

/// What an account has left, asked of its provider.
fn read_limits(apis: Apis, provider: Provider, oauth: &Oauth) -> Result<Vec<Limit>> {
    match provider {
        Provider::Claude => Ok(apis.claude.usage(&oauth.access_token)?.limits),
        Provider::Codex => {
            let account = oauth.account_id().context("a Codex login that names no account")?;
            Ok(codex::limits(&apis.codex.usage(&oauth.access_token, account)?))
        }
    }
}

fn probe_all(apis: Apis, accounts: &[Stashed]) -> Vec<Probe> {
    thread::scope(|scope| {
        let running: Vec<_> =
            accounts.iter().map(|entry| scope.spawn(move || probe(apis, entry))).collect();
        running.into_iter().map(|h| h.join().expect("probe thread panicked")).collect()
    })
}

/// Give back a refreshed credential blob when the current one is spent.
fn freshen(apis: Apis, provider: Provider, oauth: &Oauth) -> Result<Option<Oauth>> {
    if !oauth.needs_refresh() {
        return Ok(None);
    }
    Ok(Some(match provider {
        Provider::Claude => {
            refreshed_oauth(oauth, &apis.claude.refresh(&oauth.refresh_token, &oauth.scopes)?)
        }
        Provider::Codex => codex::refreshed(oauth, &apis.codex.refresh(&oauth.refresh_token)?),
    }))
}

// ── credential copies ───────────────────────────────────────────────────────
//
// One account's credentials are kept in more than one place: its stash entry,
// the live credentials while it is the installed account, and its pen while a
// session is confined to it. Claude Code refreshes whichever of them it is
// pointed at, and a refresh spends the token it presents — so a copy this tool
// was not the last writer of holds a token the server will not honour again.
// Keeping an account's copies in step is what stops that being discovered as a
// failed refresh.

/// Every copy of one account's credentials there is to read.
///
/// `installed` is what its provider's live slot holds, and belongs here only
/// when that is this account. Pens are Claude Code's, so only a Claude
/// account has one to read.
fn copies(ctx: &Ctx, entry: &Stashed, installed: Option<&Oauth>) -> Result<Vec<Oauth>> {
    // Only where there is a pen to read: a pen that was never built holds no
    // copy, and asking after one costs a keychain lookup on the machines that
    // keep credentials there.
    let pen = match entry.account.provider {
        Provider::Claude => {
            let pen_dir = pen::at(ctx.stash.root(), &entry.slug);
            match pen_dir.is_dir() {
                true => ctx.backend.confined(&pen_dir).read()?.map(|file| file.oauth),
                false => None,
            }
        }
        Provider::Codex => None,
    };
    Ok([Some(entry.account.oauth.clone()), installed.cloned(), pen].into_iter().flatten().collect())
}

/// The most recently minted of some copies of one account's credentials.
///
/// A refresh mints an access token that outlives the one it replaces, so the
/// furthest expiry marks the copy carrying the refresh token the server still
/// honours; the rest were spent producing it.
fn newest(copies: impl IntoIterator<Item = Oauth>) -> Option<Oauth> {
    copies.into_iter().max_by_key(|oauth| oauth.expires_at)
}

/// Hand `oauth` to every copy of this account's credentials: the stash entry,
/// `entry` itself, the live credentials when it is the installed account, and
/// its pen when it has one.
///
/// Updating `entry` in place is what stops a later step installing the copy
/// that was read beforehand. A copy left on a superseded token buys nothing,
/// and a session holding one is a session that has to log in again.
fn propagate(ctx: &Ctx, entry: &mut Stashed, oauth: &Oauth, live: &Live) -> Result<()> {
    entry.account.oauth = oauth.clone();
    ctx.stash.save(&entry.slug, &entry.account)?;

    if live.holds(entry) {
        install_live(ctx, entry.account.provider, oauth)?;
    }
    if entry.account.provider == Provider::Claude {
        let pen = pen::at(ctx.stash.root(), &entry.slug);
        if pen.is_dir() {
            install(ctx.backend.confined(&pen).as_ref(), oauth)?;
        }
    }
    Ok(())
}

/// Install `oauth` into its provider's live slot.
fn install_live(ctx: &Ctx, provider: Provider, oauth: &Oauth) -> Result<()> {
    match provider {
        Provider::Claude => install(ctx.creds, oauth),
        Provider::Codex => install_codex(ctx.codex, oauth),
    }
}

/// Install `oauth` into Codex's login, keeping every other field the file
/// has, under a lock of this tool's own in the same directory.
fn install_codex(store: &dyn codex::Creds, oauth: &Oauth) -> Result<()> {
    std::fs::create_dir_all(store.dir())
        .with_context(|| format!("creating {}", store.dir().display()))?;
    let _guard = lock::acquire(store.dir())?;
    let current = store.read()?.unwrap_or_default();
    store.write(&current.with(oauth))
}

/// Bring every copy of every account onto the newest credentials on disk for
/// it, before any of them is presented to the server.
///
/// This is the standing repair for the copies this tool does not write: a
/// session refreshing its own credentials leaves every other copy of that
/// account behind, and nothing says so until one of them is used.
fn reconcile(ctx: &Ctx, accounts: &mut [Stashed], live: &Live) -> Result<()> {
    let installed = slots(ctx)?;
    for entry in accounts.iter_mut() {
        let mine = live.holds(entry).then(|| installed.of(entry.account.provider)).flatten();
        let held = copies(ctx, entry, mine)?;
        let Some(newest) = newest(held.iter().cloned()) else { continue };
        if held.iter().all(|oauth| oauth.refresh_token == newest.refresh_token) {
            continue;
        }
        propagate(ctx, entry, &newest, live)?;
    }
    Ok(())
}

/// Write back every token a probe refreshed.
fn persist(ctx: &Ctx, accounts: &mut [Stashed], probes: &[Probe], live: &Live) -> Result<()> {
    for probe in probes {
        let Some(oauth) = &probe.refreshed else { continue };
        let Some(entry) = accounts.iter_mut().find(|a| a.slug == probe.slug) else { continue };
        propagate(ctx, entry, oauth, live)?;
    }
    Ok(())
}

/// Write down what each probe found, so something that cannot afford a poll
/// can still say where an account stands.
///
/// A probe that failed leaves the last good reading alone: to a reader, stale
/// is worth more than absent, and the stamp on it says how stale.
fn remember(ctx: &Ctx, probes: &[Probe]) -> Result<()> {
    for probe in probes {
        let Ok(limits) = &probe.limits else { continue };
        ctx.usage.record(&probe.slug, limits)?;
    }
    Ok(())
}

/// Which stashed account of `provider` its live slot holds.
///
/// A token match is direct evidence and wins; the recorded pointer is the
/// fallback for the moment right after a refresh rotated one copy.
fn identify(
    ctx: &Ctx,
    accounts: &[Stashed],
    provider: Provider,
    live: Option<&Oauth>,
) -> Option<String> {
    let live = live?;
    let matched = accounts.iter().filter(|a| a.account.provider == provider).find(|a| {
        a.account.oauth.refresh_token == live.refresh_token
            || a.account.oauth.access_token == live.access_token
    });
    if let Some(entry) = matched {
        return Some(entry.slug.clone());
    }
    let recorded = ctx.stash.active(provider)?;
    accounts
        .iter()
        .any(|a| a.slug == recorded && a.account.provider == provider)
        .then_some(recorded)
}

/// Put `oauth` into a credentials file, keeping every unrelated field the
/// existing one carries.
fn merged(current: Option<CredsFile>, oauth: &Oauth) -> CredsFile {
    match current {
        Some(file) => CredsFile { oauth: oauth.clone(), ..file },
        None => CredsFile::new(oauth.clone()),
    }
}

/// Install `oauth` into a credential store, under the lock that guards the
/// configuration directory it answers for.
fn install(store: &dyn CredStore, oauth: &Oauth) -> Result<()> {
    let _guard = lock::acquire(store.dir())?;
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
        bail!("no credentials in {}; {hint}", ctx.creds.describe());
    };
    let Some(oauth) = freshen(ctx.apis(), Provider::Claude, &file.oauth)? else {
        let oauth = file.oauth.clone();
        return Ok((file, oauth));
    };
    install(ctx.creds, &oauth)?;
    Ok((file, oauth))
}

/// Codex's login in use, with its freshest access token; a refresh here is
/// mirrored back to the file Codex reads.
fn codex_in_use(ctx: &Ctx, hint: &str) -> Result<Oauth> {
    let Some(file) = ctx.codex.read()? else {
        bail!("no Codex login in {}; {hint}", ctx.codex.describe());
    };
    let Some(oauth) = file.oauth() else {
        bail!("the login in {} is an API key, not a ChatGPT account; {hint}", ctx.codex.describe());
    };
    match freshen(ctx.apis(), Provider::Codex, &oauth)? {
        None => Ok(oauth),
        Some(fresh) => {
            install_codex(ctx.codex, &fresh)?;
            Ok(fresh)
        }
    }
}

// ── commands ────────────────────────────────────────────────────────────────

pub fn list(ctx: &Ctx, json: bool, cached: bool) -> Result<()> {
    if cached {
        let readings = cached_entries(ctx)?;
        if json {
            let views: Vec<View> =
                readings.iter().map(|r| view(&r.entry, r.polled_at.as_deref())).collect();
            println!("{}", serde_json::to_string_pretty(&views)?);
            return Ok(());
        }
        let table = Table::build(readings.into_iter().map(|r| r.entry).collect(), Style::detect());
        println!("{}", table.header());
        for index in 0..table.len() {
            println!("{}", table.row(index));
        }
        return Ok(());
    }
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

/// An entry as the last poll left it, and when that was.
struct Cached {
    entry: Entry,
    polled_at: Option<String>,
}

/// Every account with what the last poll wrote down for it: no network, no
/// refresh, nothing spent. The source of truth for anything that asks more
/// often than a poll can be afforded, with `ccs watch` keeping it current.
/// The account in use is whichever the stash's pointer names; identifying it
/// against the live credentials would cost a keychain read per call.
fn cached_entries(ctx: &Ctx) -> Result<Vec<Cached>> {
    let accounts = stashed(ctx)?;
    accounts
        .iter()
        .map(|account| {
            let live = ctx.stash.active(account.account.provider);
            let reading = ctx.usage.read(&account.slug)?;
            let (limits, polled_at) = match reading {
                Some(reading) => (Ok(reading.limits), Some(reading.polled_at)),
                None => (Err("not polled yet; `ccs watch` polls".to_string()), None),
            };
            let entry = Entry {
                provider: account.account.provider,
                slug: account.slug.clone(),
                email: account.account.email.clone(),
                plan: account.account.plan_label(),
                active: live.as_deref() == Some(account.slug.as_str()),
                limits,
            };
            Ok(Cached { entry, polled_at })
        })
        .collect()
}

/// The limits of every account in use: the one in Claude Code's slot, the
/// one in Codex's, whichever are logged in.
pub fn status(ctx: &Ctx, json: bool) -> Result<()> {
    let style = Style::detect();
    warn_overridden();
    let accounts = ctx.stash.list()?;
    let mut entries = Vec::new();

    // Each slot is read once: on a Mac the Claude read is a keychain call.
    let slots = slots(ctx)?;
    if let Some(held) = slots.claude {
        let oauth = match freshen(ctx.apis(), Provider::Claude, &held)? {
            None => held.clone(),
            Some(fresh) => {
                install(ctx.creds, &fresh)?;
                fresh
            }
        };
        let identified = identify(ctx, &accounts, Provider::Claude, Some(&held));
        let profile = ctx.api.profile(&oauth.access_token)?;
        let limits = ctx.api.usage(&oauth.access_token)?.limits;
        // An account nothing in the stash answers for has nowhere to be
        // recorded; the reading is keyed by slug, and there is no slug.
        if let Some(slug) = &identified {
            ctx.usage.record(slug, &limits)?;
        }
        let organization = profile.organization.as_ref();
        let plan = plan_label(
            organization.and_then(|o| o.rate_limit_tier.as_deref()),
            organization.and_then(|o| o.organization_type.as_deref()),
        );
        entries.push(Entry {
            provider: Provider::Claude,
            slug: identified.unwrap_or_default(),
            email: profile.account.email,
            plan,
            active: true,
            limits: Ok(limits),
        });
    }
    if let Some(held) = slots.codex {
        let oauth = match freshen(ctx.apis(), Provider::Codex, &held)? {
            None => held.clone(),
            Some(fresh) => {
                install_codex(ctx.codex, &fresh)?;
                fresh
            }
        };
        let identified = identify(ctx, &accounts, Provider::Codex, Some(&held));
        let who = codex::identity(oauth.id_token().unwrap_or_default())
            .with_context(|| format!("reading the login in {}", ctx.codex.describe()))?;
        let limits = read_limits(ctx.apis(), Provider::Codex, &oauth)?;
        if let Some(slug) = &identified {
            ctx.usage.record(slug, &limits)?;
        }
        entries.push(Entry {
            provider: Provider::Codex,
            slug: identified.unwrap_or_default(),
            email: who.email,
            plan: format!("codex {}", who.plan),
            active: true,
            limits: Ok(limits),
        });
    }
    if entries.is_empty() {
        bail!(
            "nothing is logged in: no credentials in {} and no login in {}",
            ctx.creds.describe(),
            ctx.codex.describe()
        );
    }

    if json {
        // The shape it always had — the Claude account's fields at the top —
        // with the Codex account, when there is one, under `codex`. With
        // only a Codex login, that one is at the top and says so in
        // `provider`, so nothing reading this ever meets a list.
        let mut top = serde_json::to_value(view(&entries[0], None))?;
        if let (Some(codex), serde_json::Value::Object(map)) = (entries.get(1), &mut top) {
            map.insert("codex".into(), serde_json::to_value(view(codex, None))?);
        }
        println!("{}", serde_json::to_string_pretty(&top)?);
        return Ok(());
    }
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            println!();
        }
        println!("{}  {}", style.bold(&entry.email), style.dim(&entry.plan));
        for line in render::detail(entry, style) {
            println!("{line}");
        }
    }
    Ok(())
}

/// Stash an account. Two ways in: log in to another one without disturbing the
/// account in use, or capture whichever account is in use right now.
pub fn add(
    ctx: &Ctx,
    provider: Provider,
    name: Option<&str>,
    current: bool,
    options: &login::Options,
) -> Result<()> {
    let oauth = match (provider, current) {
        (Provider::Claude, true) => {
            in_use(ctx, "sign in with `claude` before `ccs add --current`")?.1
        }
        (Provider::Claude, false) => {
            println!("logging in to another account; the one in use is not affected");
            login::run(ctx.backend, ctx.stash.root(), &claude_binary(), options)?
        }
        (Provider::Codex, true) => {
            codex_in_use(ctx, "sign in with `codex login` before `ccs add --current --codex`")?
        }
        (Provider::Codex, false) => {
            println!("logging in to another Codex account; the one in use is not affected");
            login::run_codex(ctx.backend, ctx.stash.root(), &codex_binary())?
        }
    };
    let recorded = record(ctx, provider, oauth, name, Installed::from(current))?;
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

#[derive(Debug)]
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

fn codex_binary() -> String {
    std::env::var("CCS_CODEX_BINARY").unwrap_or_else(|_| "codex".to_string())
}

/// Ask who some credentials belong to, then write them into the stash.
///
/// Claude is asked over the network; a Codex account names itself in its
/// identity token. A slug already used by the other provider — the same
/// email signed up twice — gets the provider's name in front.
fn record(
    ctx: &Ctx,
    provider: Provider,
    oauth: Oauth,
    name: Option<&str>,
    installed: Installed,
) -> Result<Recorded> {
    let (email, uuid, plan, rate_limit_tier) = match provider {
        Provider::Claude => {
            let profile = ctx.api.profile(&oauth.access_token)?;
            let organization = profile.organization.as_ref();
            (
                profile.account.email.clone(),
                profile.account.uuid.clone(),
                organization.and_then(|o| o.organization_type.clone()),
                organization.and_then(|o| o.rate_limit_tier.clone()),
            )
        }
        Provider::Codex => {
            let token = oauth.id_token().context("a Codex login without an identity token")?;
            let who = codex::identity(token)?;
            (who.email, who.account_id, Some(who.plan), None)
        }
    };
    let held = ctx.stash.list()?;
    let slug = match name {
        Some(name) => {
            if let Some(other) =
                held.iter().find(|s| s.slug == name && s.account.provider != provider)
            {
                bail!(
                    "{name} is a {} account, {}; pick another name",
                    other.account.provider,
                    other.account.email
                );
            }
            name.to_string()
        }
        None => {
            let plain = stash::slugify(&email);
            let taken_by_other =
                held.iter().any(|s| s.slug == plain && s.account.provider != provider);
            match taken_by_other {
                true => format!("{provider}-{plain}"),
                false => plain,
            }
        }
    };
    let replaced = held.iter().any(|s| s.slug == slug);

    let account = Account {
        provider,
        email,
        uuid,
        plan,
        rate_limit_tier,
        added_at: Timestamp::now().to_string(),
        oauth,
    };
    ctx.stash.save(&slug, &account)?;
    if installed == Installed::Yes {
        ctx.stash.set_active(provider, &slug)?;
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
    // The pen's credentials may not have been inside it, and what is left of
    // them outlives the directory without a word.
    if let Err(e) = ctx.backend.forget(&pen::at(ctx.stash.root(), &target.slug)) {
        eprintln!("ccs: {}'s pinned credentials are still there: {}", target.slug, describe(&e));
    }
    if let Err(e) = ctx.usage.forget(&target.slug) {
        eprintln!("ccs: {}'s last usage reading is still there: {}", target.slug, describe(&e));
    }
    println!("forgot {} ({})", target.account.email, target.slug);
    Ok(())
}

pub fn use_account(ctx: &Ctx, needle: &str, force: bool) -> Result<()> {
    let mut accounts = ctx.stash.list()?;
    let slug = stash::resolve(&accounts, needle)?.slug.clone();

    let live = identify_live(ctx, &accounts)?;
    reconcile(ctx, &mut accounts, &live)?;
    let probe = probe(ctx.apis(), stash::resolve(&accounts, &slug)?);
    persist(ctx, &mut accounts, std::slice::from_ref(&probe), &live)?;
    remember(ctx, std::slice::from_ref(&probe))?;

    // Read after the probe has been folded back in, never before it: the probe
    // may have refreshed this very account, and the copy taken beforehand
    // carries the tokens that refresh superseded.
    let target = stash::resolve(&accounts, &slug)?.clone();
    guard_exhausted(&entry_of(&target, &probe, false), force)?;
    let told = switch_to(ctx, &mut accounts, &target)?;
    report_switch(&target, told);
    Ok(())
}

/// Subscribe (or unsubscribe) the calling Claude Code session to notices.
pub fn notify(ctx: &Ctx, off: bool, kinds: &[String], bypass: bool) -> Result<()> {
    notify::subscribe(ctx.stash.root(), !off, kinds, bypass)
}

/// Poll every account on an interval and raise a notice for whatever changed.
/// Runs until killed; each notice is echoed here as well as delivered.
pub fn watch(ctx: &Ctx, every: Duration, high: f64, rotate: &[String]) -> Result<()> {
    let pool: Vec<String> = {
        let accounts = stashed(ctx)?;
        rotate
            .iter()
            .map(|n| stash::resolve(&accounts, n).map(|s| s.slug.clone()))
            .collect::<Result<_>>()?
    };
    if !pool.is_empty() {
        println!("{} rotating between {}", stamp(), pool.join(", "));
    }
    let mut before = watch::Snapshot::new();
    loop {
        let mut accounts = stashed(ctx)?;
        match survey(ctx, &mut accounts, Style::detect()) {
            Ok((table, _)) => {
                for event in watch::diff(&before, table.entries(), high) {
                    let told = notify::broadcast(
                        ctx.creds.dir(),
                        ctx.stash.root(),
                        event.kind,
                        &event.text,
                    );
                    println!("{} {}: {}{}", stamp(), event.kind, event.text, heard(told));
                }
                before = watch::snapshot(table.entries());
                for next in watch::rotations(table.entries(), &pool, high) {
                    match stash::resolve(&accounts, &next.slug).cloned() {
                        Ok(target) => match switch_to(ctx, &mut accounts, &target) {
                            Ok(told) => println!(
                                "{} rotate: switched to {} ({}){}",
                                stamp(),
                                target.account.email,
                                target.slug,
                                heard(told)
                            ),
                            Err(e) => eprintln!("{} rotate failed: {}", stamp(), describe(&e)),
                        },
                        Err(e) => eprintln!("{} rotate failed: {}", stamp(), describe(&e)),
                    }
                }
            }
            Err(e) => eprintln!("{} poll failed: {}", stamp(), describe(&e)),
        }
        thread::sleep(every);
    }
}

fn stamp() -> String {
    Timestamp::now().strftime("%H:%M:%S").to_string()
}

// ── the gateway ─────────────────────────────────────────────────────────────

/// Serve the API on loopback as the account in use, until killed.
///
/// The connections are answered on threads of their own; this thread keeps
/// the stash, and answers their questions about which account to send as.
pub fn serve(ctx: &Ctx, port: u16, rotate: &[String]) -> Result<()> {
    let pool: Vec<String> = {
        let accounts = stashed(ctx)?;
        rotate
            .iter()
            .map(|n| stash::resolve(&accounts, n).map(|s| s.slug.clone()))
            .collect::<Result<_>>()?
    };
    let keys = serve::Keys {
        claude: serve::key(ctx.stash.root())?,
        codex: serve::codex_key(ctx.stash.root())?,
    };
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("listening on 127.0.0.1:{port}"))?;

    println!("{} serving the API on http://127.0.0.1:{port} as the account in use", stamp());
    if !pool.is_empty() {
        println!("{} falling over to {} when it is limited", stamp(), pool.join(", "));
    }
    println!("paste into pi's models.json:\n{}", serve::pi_config(port));

    let (asks, inbox) = std::sync::mpsc::channel();
    serve::listen(listener, keys, asks);
    let desk = Desk { ctx, pool: &pool };
    for ask in inbox {
        ask.answer(&desk);
    }
    Ok(())
}

/// Print a provider's gateway key, for pi's `"apiKey": "!ccs serve --key"`.
pub fn serve_key(ctx: &Ctx, provider: Provider) -> Result<()> {
    let key = match provider {
        Provider::Claude => serve::key(ctx.stash.root())?,
        Provider::Codex => serve::codex_key(ctx.stash.root())?,
    };
    println!("{key}");
    Ok(())
}

/// The stash as the gateway sees it: which account a request goes out as.
struct Desk<'a> {
    ctx: &'a Ctx<'a>,
    /// Accounts to fall over to, in order, when the one in use is limited.
    pool: &'a [String],
}

impl Desk<'_> {
    /// Which stashed account is in use. The recorded pointer is what every
    /// switch writes, and is cheap; the live credentials are read only when
    /// it points nowhere.
    fn live(&self, accounts: &[Stashed]) -> Result<Live> {
        let mut live = Live::default();
        for provider in Provider::ALL {
            let recorded = self.ctx.stash.active(provider).filter(|s| {
                accounts.iter().any(|a| a.slug == *s && a.account.provider == provider)
            });
            let slug = match recorded {
                Some(slug) => Some(slug),
                None => identify(self.ctx, accounts, provider, slot(self.ctx, provider)?.as_ref()),
            };
            live.set(provider, slug);
        }
        Ok(live)
    }

    /// The account of `provider` a request goes out as: the one in use,
    /// unless it is among `avoid`, and then the first pooled account of that
    /// provider that is not.
    fn choose(
        &self,
        accounts: &[Stashed],
        provider: Provider,
        live: Option<&str>,
        avoid: &[String],
    ) -> Result<String> {
        let usable = |slug: &str| {
            accounts.iter().any(|a| a.slug == slug && a.account.provider == provider)
                && !avoid.iter().any(|a| a == slug)
        };
        if let Some(slug) = live.filter(|s| usable(s)) {
            return Ok(slug.to_string());
        }
        if let Some(slug) = self.pool.iter().find(|s| usable(s)) {
            return Ok(slug.clone());
        }
        match (live, avoid.is_empty()) {
            (None, true) => bail!("no {provider} account is in use; `ccs use` one"),
            _ => bail!("every account is limited: {}", avoid.join(", ")),
        }
    }

    /// `slug`'s credentials, refreshed if they are spent.
    ///
    /// A session may have refreshed them already, leaving the stash behind on
    /// a spent refresh token; the copies are brought level before one is
    /// presented to the server.
    ///
    /// A refresh that fails may still be a race lost to such a session, one
    /// that presented the same refresh token a moment earlier: the copies
    /// are levelled once more, and only a copy that is still spent is a
    /// failure.
    fn hand_out(&self, slug: &str, accounts: &mut [Stashed], live: &Live) -> Result<Grant> {
        let position = accounts.iter().position(|a| a.slug == slug).context("no such account")?;
        if !accounts[position].account.oauth.needs_refresh() {
            return Ok(grant_of(&accounts[position]));
        }
        reconcile(self.ctx, accounts, live)?;
        let provider = accounts[position].account.provider;
        let refreshed = freshen(self.ctx.apis(), provider, &accounts[position].account.oauth);
        match refreshed {
            Ok(Some(oauth)) => propagate(self.ctx, &mut accounts[position], &oauth, live)?,
            Ok(None) => {}
            Err(e) => {
                reconcile(self.ctx, accounts, live)?;
                if accounts[position].account.oauth.needs_refresh() {
                    return Err(e);
                }
            }
        }
        Ok(grant_of(&accounts[position]))
    }
}

fn grant_of(entry: &Stashed) -> Grant {
    Grant {
        provider: entry.account.provider,
        slug: entry.slug.clone(),
        email: entry.account.email.clone(),
        token: entry.account.oauth.access_token.clone(),
        account_id: entry.account.oauth.account_id().map(String::from),
    }
}

impl serve::Accounts for Desk<'_> {
    fn grant(&self, provider: Provider, avoid: &[String]) -> Result<Grant, String> {
        let granted = || -> Result<Grant> {
            let mut accounts = self.ctx.stash.list()?;
            let live = self.live(&accounts)?;
            let slug = self.choose(&accounts, provider, live.of(provider), avoid)?;
            self.hand_out(&slug, &mut accounts, &live)
        };
        granted().map_err(|e| describe(&e))
    }

    fn stale(&self, grant: &Grant) -> Result<Option<Grant>, String> {
        let renewed = || -> Result<Option<Grant>> {
            let mut accounts = self.ctx.stash.list()?;
            let live = self.live(&accounts)?;
            reconcile(self.ctx, &mut accounts, &live)?;
            let entry =
                accounts.iter().find(|a| a.slug == grant.slug).context("no such account")?;
            Ok((entry.account.oauth.access_token != grant.token).then(|| grant_of(entry)))
        };
        renewed().map_err(|e| describe(&e))
    }
}

pub fn pick(ctx: &Ctx) -> Result<()> {
    let mut accounts = stashed(ctx)?;
    // Switching is done from inside the picker, which stays up for it, so
    // nothing is left over here to act on.
    choose(ctx, &mut accounts, Verb::Switch).map(|_| ())
}

/// Confine a session to one account: install that account's credentials in its
/// own pen and hand the process over to Claude Code there.
///
/// Nothing outside the pen is touched, so every other session stays on the
/// account in use and a later `ccs use` leaves this one where it is.
pub fn pin(ctx: &Ctx, needle: Option<&str>, args: &[String]) -> Result<()> {
    let mut accounts = stashed(ctx)?;
    let live = identify_live(ctx, &accounts)?;
    reconcile(ctx, &mut accounts, &live)?;

    // Named outright, the account is taken at its word and the session starts
    // without a round trip; the picker is where usage is shopped for.
    let target = match needle {
        Some(needle) => stash::resolve(&accounts, needle)?.clone(),
        None => {
            let Some(chosen) = choose(ctx, &mut accounts, Verb::Launch)? else { return Ok(()) };
            chosen
        }
    };
    if target.account.provider != Provider::Claude {
        bail!(
            "{} is a {} account; a pin is a Claude Code session",
            target.account.email,
            target.account.provider
        );
    }

    let pen = pen_for(ctx, &target, &live)?;
    warn_overridden();
    println!("{} is pinned to this session only", target.account.email);
    pen::launch(&pen, &claude_binary(), args)
}

/// The account's pen, with its freshest credentials in place.
///
/// The pen is built before the credentials are settled so that folding them out
/// finds it, which is what leaves the pen and every other copy of the account on
/// the one token a refresh here has not spent.
fn pen_for(ctx: &Ctx, target: &Stashed, live: &Live) -> Result<PathBuf> {
    let pen = pen::prepare(ctx.home, ctx.stash.root(), &target.slug)?;

    let mut entry = target.clone();
    let oauth = freshen(ctx.apis(), Provider::Claude, &entry.account.oauth)?
        .unwrap_or_else(|| entry.account.oauth.clone());
    propagate(ctx, &mut entry, &oauth, live)?;
    Ok(pen)
}

/// The environment variables answering for the credentials right now. Set, they
/// decide the account whatever any credentials file holds — the live one and a
/// pen's alike, which makes them worth saying wherever a switch is reported.
fn overriding() -> Vec<&'static str> {
    creds::OVERRIDING.iter().copied().filter(|k| std::env::var_os(k).is_some()).collect()
}

/// Say so when the environment answers for the credentials.
fn warn_overridden() {
    let set = overriding();
    if set.is_empty() {
        return;
    }
    eprintln!(
        "ccs: {} takes precedence over the credentials file; a session that inherits it uses \
         that account instead",
        set.join(", ")
    );
}

/// The stash as the picker sees it: something to poll, and something to act on.
///
/// One collaborator rather than two closures because both halves reach the same
/// accounts, and only one of them can borrow them at a time.
struct Deck<'a, 'b> {
    ctx: &'a Ctx<'b>,
    accounts: &'a mut [Stashed],
    verb: Verb,
    /// The account most recently installed from inside the picker, so the
    /// terminal the picker was left in still says what happened in it.
    switched: Option<(Stashed, usize)>,
}

impl picker::Accounts for Deck<'_, '_> {
    fn poll(&mut self) -> Result<Table> {
        survey(self.ctx, self.accounts, Style::colored()).map(|(table, _)| table)
    }

    /// A launch is handed back because it takes the process itself; a switch is
    /// done here and now, leaving the picker up.
    ///
    /// The copies are brought into step first, exactly as `ccs use` does. A
    /// picker that stays up can switch twice, and between the two the account it
    /// left may have had its live tokens refreshed underneath it by the sessions
    /// that followed the first switch.
    fn act(&mut self, slug: &str) -> Result<Act> {
        if matches!(self.verb, Verb::Launch) {
            return Ok(Act::Handed);
        }
        let live = identify_live(self.ctx, self.accounts)?;
        reconcile(self.ctx, self.accounts, &live)?;

        let target = stash::resolve(self.accounts, slug)?.clone();
        let told = switch_to(self.ctx, self.accounts, &target)?;

        let said = match overriding().as_slice() {
            [] => format!("switched to {}{}", target.account.email, heard(told)),
            set => {
                format!("switched to {}, but {} overrides it", target.account.email, set.join(", "))
            }
        };
        self.switched = Some((target, told));
        Ok(Act::Installed(said))
    }
}

/// Put the accounts on screen and wait. A switch happens inside the picker,
/// which stays up for it; only an act the picker cannot survive comes back, so
/// what this returns is the account a launch still has to be given. The picker
/// owns the whole screen while it is up, so it is always drawn in colour.
fn choose(ctx: &Ctx, accounts: &mut [Stashed], verb: Verb) -> Result<Option<Stashed>> {
    let mut deck = Deck { ctx, accounts, verb, switched: None };
    let outcome = picker::run(&mut deck, verb);
    let switched = deck.switched.take();

    // Reported after the screen is down, and whether or not the picker itself
    // came back cleanly: the switch already happened on disk either way.
    if let Some((target, told)) = &switched {
        report_switch(target, *told);
    }
    let Outcome::Chose(slug) = outcome? else { return Ok(None) };
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
fn survey(ctx: &Ctx, accounts: &mut [Stashed], style: Style) -> Result<(Table, Live)> {
    let live = identify_live(ctx, accounts)?;
    reconcile(ctx, accounts, &live)?;
    let probes = probe_all(ctx.apis(), accounts);
    persist(ctx, accounts, &probes, &live)?;
    remember(ctx, &probes)?;

    let entries = accounts
        .iter()
        .zip(&probes)
        .map(|(account, probe)| entry_of(account, probe, live.holds(account)))
        .collect();
    Ok((Table::build(entries, style), live))
}

/// Which stashed account each provider's live slot holds.
fn identify_live(ctx: &Ctx, accounts: &[Stashed]) -> Result<Live> {
    let installed = slots(ctx)?;
    let mut live = Live::default();
    for provider in Provider::ALL {
        live.set(provider, identify(ctx, accounts, provider, installed.of(provider)));
    }
    Ok(live)
}

/// Install `target` as the live credentials.
///
/// The outgoing account's tokens are folded back into the stash first: Claude
/// Code refreshes tokens in place, so the stashed copy goes stale the moment an
/// account is used. Capturing it on the way out is what keeps a stashed account
/// usable without a fresh login.
fn switch_to(ctx: &Ctx, accounts: &mut [Stashed], target: &Stashed) -> Result<usize> {
    if target.account.provider == Provider::Codex {
        return switch_codex(ctx, accounts, target);
    }
    let _guard = lock::acquire(ctx.creds.dir())?;
    let live = ctx.creds.read()?;
    if let Some(live) = &live {
        capture_outgoing(ctx, accounts, Provider::Claude, &live.oauth, &target.slug)?;
    }
    ctx.creds.write(&merged(live, &target.account.oauth))?;
    ctx.stash.set_active(target.account.provider, &target.slug)?;
    drop(_guard);
    let told = notify::broadcast(
        ctx.creds.dir(),
        ctx.stash.root(),
        "switch",
        &format!(
            "this session's account is now {} ({}). \
             The API prompt cache is per account, so the next request re-prefills everything.",
            target.account.email, target.slug
        ),
    );
    Ok(told)
}

/// What a switch leaves behind in the terminal it happened in. The picker says
/// its own piece in the footer while it is up; this is the record that outlives
/// the screen.
fn report_switch(target: &Stashed, told: usize) {
    println!("switched to {} ({})", target.account.email, target.slug);
    println!("running sessions pick this up on their next request{}", heard(told));
    warn_overridden();
}

/// How many subscribed sessions heard about it, for the tail of a report.
fn heard(told: usize) -> String {
    match told {
        0 => String::new(),
        1 => "; told 1 subscribed session".into(),
        n => format!("; told {n} subscribed sessions"),
    }
}

/// Install `target` as Codex's login. Codex CLI reloads the file when it
/// changes, so running sessions follow; there is no inbox to tell.
fn switch_codex(ctx: &Ctx, accounts: &mut [Stashed], target: &Stashed) -> Result<usize> {
    std::fs::create_dir_all(ctx.codex.dir())
        .with_context(|| format!("creating {}", ctx.codex.dir().display()))?;
    let _guard = lock::acquire(ctx.codex.dir())?;
    let current = ctx.codex.read()?;
    if let Some(live) = current.as_ref().and_then(|f| f.oauth()) {
        capture_outgoing(ctx, accounts, Provider::Codex, &live, &target.slug)?;
    } else if current.as_ref().is_some_and(|f| f.is_api_key()) {
        // An API-key login is not an account the stash can hold, so it is
        // not captured on the way out; said, so it is not a surprise.
        eprintln!("ccs: {} held an API-key login; it is replaced", ctx.codex.describe());
    }
    ctx.codex.write(&current.unwrap_or_default().with(&target.account.oauth))?;
    ctx.stash.set_active(Provider::Codex, &target.slug)?;
    Ok(0)
}

/// Fold the outgoing account's live credentials into its stash entry.
///
/// The stash is the only copy written here, and that is enough: every copy of
/// that account was brought into step before the switch began, so the live file
/// about to be replaced is the only one that can have moved since. It is also
/// the only write this can do — the caller holds the credential lock, which
/// installing anything would try to take again.
///
/// The entry in hand is moved onto those credentials too. Leaving it behind
/// would leave the caller holding a token the file it just wrote has superseded,
/// which is the whole failure this exists to prevent.
fn capture_outgoing(
    ctx: &Ctx,
    accounts: &mut [Stashed],
    provider: Provider,
    live: &Oauth,
    incoming: &str,
) -> Result<()> {
    let Some(slug) = identify(ctx, accounts, provider, Some(live)) else { return Ok(()) };
    if slug == incoming {
        return Ok(());
    }
    let Some(entry) = accounts.iter_mut().find(|a| a.slug == slug) else { return Ok(()) };
    entry.account.oauth = live.clone();
    ctx.stash.save(&slug, &entry.account)
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
    provider: Provider,
    slug: &'a str,
    email: &'a str,
    plan: &'a str,
    active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
    /// When the limits were read, for a listing that did not read them now.
    #[serde(skip_serializing_if = "Option::is_none")]
    polled_at: Option<&'a str>,
    limits: &'a [Limit],
}

fn view<'a>(entry: &'a Entry, polled_at: Option<&'a str>) -> View<'a> {
    View {
        provider: entry.provider,
        slug: &entry.slug,
        email: &entry.email,
        plan: &entry.plan,
        active: entry.active,
        error: entry.limits.as_ref().err().map(String::as_str),
        polled_at,
        limits: entry.known(),
    }
}

fn emit_json(table: &Table) -> Result<()> {
    let views: Vec<View> = table.entries().iter().map(|e| view(e, None)).collect();
    println!("{}", serde_json::to_string_pretty(&views)?);
    Ok(())
}

/// Lay one account and its probe out as a table entry. The single place that
/// mapping happens, so the picker, the table and the switch guard all agree.
fn entry_of(account: &Stashed, probe: &Probe, active: bool) -> Entry {
    Entry {
        provider: account.account.provider,
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
    use crate::limit;
    use crate::picker::Accounts as _;
    use std::cell::RefCell;
    use std::fs;
    use std::path::Path;

    /// Credentials held in memory rather than on disk, so a test can see what
    /// a command installed.
    struct Recorder {
        dir: PathBuf,
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

        fn dir(&self) -> &Path {
            &self.dir
        }

        fn describe(&self) -> String {
            format!("{} (held in memory)", self.dir.display())
        }
    }

    /// Credentials identified by their refresh token, expiring when told to.
    /// The expiry is what orders the copies of one account, so a test that
    /// cares which copy wins sets it deliberately.
    fn oauth(refresh_token: &str, expires_at: i64) -> Oauth {
        Oauth {
            access_token: format!("access-{refresh_token}"),
            refresh_token: refresh_token.to_string(),
            expires_at,
            scopes: vec![],
            subscription_type: None,
            extra: serde_json::Map::new(),
        }
    }

    /// Codex's login held in memory, as `Recorder` holds Claude's.
    struct CodexRecorder {
        dir: PathBuf,
        held: RefCell<Option<codex::AuthFile>>,
    }

    impl codex::Creds for CodexRecorder {
        fn read(&self) -> Result<Option<codex::AuthFile>> {
            Ok(self.held.borrow().clone())
        }

        fn write(&self, file: &codex::AuthFile) -> Result<()> {
            *self.held.borrow_mut() = Some(file.clone());
            Ok(())
        }

        fn dir(&self) -> &Path {
            &self.dir
        }

        fn describe(&self) -> String {
            "the codex recorder".into()
        }
    }

    /// A Codex account's credentials: the pair, plus the identity token and
    /// account id every Codex request carries.
    fn codex_oauth(refresh_token: &str, expires_at: i64, email: &str) -> Oauth {
        let mut oauth = oauth(refresh_token, expires_at);
        let claims = serde_json::json!({
            "email": email,
            "https://api.openai.com/auth": {"chatgpt_account_id": format!("acct-{email}"), "chatgpt_plan_type": "pro"}
        });
        let token = format!("h.{}.s", codex::base64url(claims.to_string().as_bytes()));
        // Codex access tokens are JWTs, and the expiry is read off them.
        let access = serde_json::json!({"exp": expires_at / 1000, "jti": refresh_token});
        oauth.access_token = format!("h.{}.s", codex::base64url(access.to_string().as_bytes()));
        oauth.extra.insert("idToken".into(), serde_json::Value::String(token));
        oauth.extra.insert("accountId".into(), serde_json::Value::String(format!("acct-{email}")));
        oauth
    }

    /// The live slots with a Claude account in the Claude one.
    fn claude_live(slug: &str) -> Live {
        Live { claude: Some(slug.to_string()), codex: None }
    }

    fn codex_stashed(slug: &str, oauth: Oauth) -> Stashed {
        let mut entry = stashed(slug, oauth);
        entry.account.provider = Provider::Codex;
        entry
    }

    /// A configuration directory with a stash in it, private to one test so
    /// concurrently running tests never share a path.
    struct Fixture {
        dir: PathBuf,
        creds: Recorder,
        codex: CodexRecorder,
        codex_api: codex::Client,
        stash: Stash,
        usage: usage::Cache,
        api: Api,
        home: pen::Home,
        /// Accounts a gateway desk may fall over to.
        pool: Vec<String>,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("ccs-cmd-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("config directory");
            let stash = Stash::open(&dir).expect("stash");
            Self {
                creds: Recorder { dir: dir.clone(), held: RefCell::new(None) },
                codex: CodexRecorder { dir: dir.join("codex"), held: RefCell::new(None) },
                codex_api: codex::Client::new(),
                usage: usage::Cache::open(stash.root()).expect("usage cache"),
                stash,
                api: Api::new(),
                home: pen::Home { config: dir.clone(), global: dir.join(".claude.json") },
                dir,
                pool: Vec::new(),
            }
        }

        fn ctx(&self) -> Ctx<'_> {
            Ctx {
                creds: &self.creds,
                // A test writes credentials where it can look at them, never
                // into the keychain of whoever is running it.
                backend: Backend::File,
                stash: &self.stash,
                usage: &self.usage,
                api: &self.api,
                codex: &self.codex,
                codex_api: &self.codex_api,
                home: &self.home,
            }
        }

        /// The refresh token in Codex's live slot, if anything is there.
        fn codex_live(&self) -> Option<String> {
            self.codex.read().expect("read codex").and_then(|f| f.oauth()).map(|o| o.refresh_token)
        }

        /// Put credentials where a session confined to `slug` would keep them.
        fn pin(&self, slug: &str, oauth: &Oauth) {
            fs::create_dir_all(pen::at(self.stash.root(), slug)).expect("pen");
            self.pen(slug).write(&CredsFile::new(oauth.clone())).expect("pen credentials");
        }

        fn pen(&self, slug: &str) -> Box<dyn CredStore> {
            Backend::File.confined(&pen::at(self.stash.root(), slug))
        }

        /// The usage reading recorded for `slug`, if one was.
        fn reading(&self, slug: &str) -> Option<usage::Reading> {
            let raw =
                fs::read(self.stash.root().join("usage").join(format!("{slug}.json"))).ok()?;
            Some(serde_json::from_slice(&raw).expect("parses"))
        }

        /// The refresh token each copy of `slug` is holding: stash, live, pen.
        fn tokens(&self, slug: &str) -> (String, Option<String>, Option<String>) {
            let accounts = self.stash.list().expect("list");
            let stashed = accounts.iter().find(|s| s.slug == slug).expect("stash entry");
            let token = |file: Option<CredsFile>| file.map(|f| f.oauth.refresh_token);
            (
                stashed.account.oauth.refresh_token.clone(),
                token(self.creds.read().expect("read live")),
                token(self.pen(slug).read().expect("read pen")),
            )
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn stashed(slug: &str, oauth: Oauth) -> Stashed {
        Stashed {
            slug: slug.into(),
            account: Account {
                provider: Provider::Claude,
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
    /// live credentials, the pen a session is confined in, and the in-memory
    /// list the caller goes on to install from. A copy left on the superseded
    /// token buys nothing, and a session handed one has to log in again.
    #[test]
    fn a_refresh_reaches_every_copy_of_the_account_it_refreshed() {
        let fixture = Fixture::new("persist");
        fixture.pin("work", &oauth("superseded", 1));

        let mut accounts = vec![stashed("work", oauth("superseded", 1))];
        let probes = vec![Probe {
            slug: "work".into(),
            refreshed: Some(oauth("rotated", 2)),
            limits: Ok(vec![]),
        }];
        persist(&fixture.ctx(), &mut accounts, &probes, &claude_live("work")).expect("persist");

        assert_eq!(accounts[0].account.oauth.refresh_token, "rotated", "the list the caller holds");
        let (stashed, live, pen) = fixture.tokens("work");
        assert_eq!(stashed, "rotated", "the stash");
        assert_eq!(live.as_deref(), Some("rotated"), "the live credentials");
        assert_eq!(pen.as_deref(), Some("rotated"), "the pen");
    }

    #[test]
    fn the_furthest_expiry_is_the_copy_that_was_minted_last() {
        let copies = [oauth("spent", 10), oauth("current", 30), oauth("older", 20)];
        assert_eq!(newest(copies).expect("a copy").refresh_token, "current");
    }

    #[test]
    fn newest_of_nothing_is_nothing() {
        assert!(newest([]).is_none());
    }

    /// The live session refreshes the credentials this tool installed for it,
    /// which spends the token the stash is holding. Reading the stash entry
    /// afterwards is reading a token the server will refuse.
    #[test]
    fn credentials_the_live_session_refreshed_are_taken_back_into_the_stash() {
        let fixture = Fixture::new("live");
        fixture.creds.write(&CredsFile::new(oauth("rotated", 2))).expect("live credentials");

        let mut accounts = vec![stashed("work", oauth("superseded", 1))];
        reconcile(&fixture.ctx(), &mut accounts, &claude_live("work")).expect("reconcile");

        assert_eq!(accounts[0].account.oauth.refresh_token, "rotated", "the list the caller holds");
        assert_eq!(fixture.tokens("work").0, "rotated", "the stash");
    }

    /// Same for a pinned session: it refreshes inside its pen, and nothing else
    /// hears about it.
    #[test]
    fn credentials_a_pinned_session_refreshed_are_taken_back_into_the_stash() {
        let fixture = Fixture::new("pen");
        fixture.pin("work", &oauth("rotated", 2));
        fixture.creds.write(&CredsFile::new(oauth("elsewhere", 5))).expect("live credentials");

        let mut accounts = vec![stashed("work", oauth("superseded", 1))];
        reconcile(&fixture.ctx(), &mut accounts, &claude_live("other")).expect("reconcile");

        assert_eq!(accounts[0].account.oauth.refresh_token, "rotated", "the list the caller holds");
        let (stashed, live, _) = fixture.tokens("work");
        assert_eq!(stashed, "rotated", "the stash");
        assert_eq!(live.as_deref(), Some("elsewhere"), "another account's live credentials");
    }

    /// The newest copy goes to the laggards whichever copy it is, so an account
    /// refreshed here while a session was pinned to it leaves no dead pen.
    #[test]
    fn the_newest_copy_reaches_the_ones_that_fell_behind() {
        let fixture = Fixture::new("laggard");
        fixture.pin("work", &oauth("superseded", 1));

        let mut accounts = vec![stashed("work", oauth("rotated", 2))];
        reconcile(&fixture.ctx(), &mut accounts, &claude_live("work")).expect("reconcile");

        let (stashed, live, pen) = fixture.tokens("work");
        assert_eq!(stashed, "rotated", "the stash");
        assert_eq!(live.as_deref(), Some("rotated"), "the live credentials");
        assert_eq!(pen.as_deref(), Some("rotated"), "the pen");
    }

    #[test]
    fn copies_that_already_agree_are_left_alone() {
        let fixture = Fixture::new("agree");
        fixture.pin("work", &oauth("current", 2));

        let mut accounts = vec![stashed("work", oauth("current", 2))];
        reconcile(&fixture.ctx(), &mut accounts, &claude_live("work")).expect("reconcile");

        assert!(
            fixture.creds.read().expect("read").is_none(),
            "nothing to install, so nothing was"
        );
    }

    /// The picker's view of a stash, wired the way `choose` wires it.
    fn deck<'a, 'b>(ctx: &'a Ctx<'b>, accounts: &'a mut [Stashed], verb: Verb) -> Deck<'a, 'b> {
        Deck { ctx, accounts, verb, switched: None }
    }

    fn stash_all(fixture: &Fixture, accounts: &[Stashed]) {
        for entry in accounts {
            fixture.stash.save(&entry.slug, &entry.account).expect("stash");
        }
    }

    #[test]
    fn a_switch_is_made_from_inside_the_picker_rather_than_handed_back() {
        let fixture = Fixture::new("installed");
        let mut accounts = vec![stashed("a", oauth("a1", 1)), stashed("b", oauth("b1", 1))];
        stash_all(&fixture, &accounts);

        let ctx = fixture.ctx();
        let mut deck = deck(&ctx, &mut accounts, Verb::Switch);
        let Act::Installed(said) = deck.act("b").expect("switches") else {
            panic!("a switch should leave the picker up")
        };

        assert!(said.contains("b@example.com"), "{said}");
        assert_eq!(deck.switched.expect("recorded for the terminal").0.slug, "b");
        assert_eq!(fixture.tokens("b").1.as_deref(), Some("b1"));
        assert_eq!(fixture.stash.active(Provider::Claude).as_deref(), Some("b"));
    }

    #[test]
    fn a_launch_leaves_the_picker_and_installs_nothing() {
        let fixture = Fixture::new("handed");
        let mut accounts = vec![stashed("a", oauth("a1", 1))];
        stash_all(&fixture, &accounts);

        let ctx = fixture.ctx();
        let mut deck = deck(&ctx, &mut accounts, Verb::Launch);
        assert!(matches!(deck.act("a").expect("hands back"), Act::Handed));
        assert!(deck.switched.is_none());
        assert!(fixture.creds.read().expect("read").is_none(), "a launch installs nothing");
    }

    #[test]
    fn switching_away_and_back_without_leaving_the_picker_installs_the_live_token() {
        let fixture = Fixture::new("round-trip");
        let mut accounts = vec![stashed("a", oauth("a-old", 1)), stashed("b", oauth("b1", 1))];
        stash_all(&fixture, &accounts);
        fixture.stash.set_active(Provider::Claude, "a").expect("active");
        // A session on `a` refreshed the live credentials after the picker last
        // polled, so what the picker is holding for `a` is already spent.
        fixture.creds.write(&CredsFile::new(oauth("a-new", 2))).expect("live");

        let ctx = fixture.ctx();
        let mut deck = deck(&ctx, &mut accounts, Verb::Switch);
        deck.act("b").expect("switches to b");
        deck.act("a").expect("switches back to a");

        let installed = fixture.creds.read().expect("read").expect("installed");
        assert_eq!(
            installed.oauth.refresh_token, "a-new",
            "switching twice in one picker put back a token the first switch had superseded"
        );
    }

    #[test]
    fn a_poll_leaves_its_findings_where_something_that_cannot_poll_can_read_them() {
        let fixture = Fixture::new("recorded");
        let probe = Probe {
            slug: "a".into(),
            refreshed: None,
            limits: Ok(vec![limit!("weekly_scoped", 61.0, model = "Fable")]),
        };
        remember(&fixture.ctx(), std::slice::from_ref(&probe)).expect("records");

        let reading = fixture.reading("a").expect("recorded");
        assert_eq!(reading.limits[0].model_name(), Some("Fable"));
        assert_eq!(reading.limits[0].percent, 61.0);
    }

    #[test]
    fn a_probe_that_failed_leaves_the_last_good_reading_standing() {
        let fixture = Fixture::new("kept");
        let good =
            Probe { slug: "a".into(), refreshed: None, limits: Ok(vec![limit!("session", 12.0)]) };
        remember(&fixture.ctx(), std::slice::from_ref(&good)).expect("records");

        let failed =
            Probe { slug: "a".into(), refreshed: None, limits: Err("token rejected".into()) };
        remember(&fixture.ctx(), std::slice::from_ref(&failed)).expect("records nothing");

        assert_eq!(fixture.reading("a").expect("still there").limits[0].percent, 12.0);
    }

    /// A reader that cannot afford a poll — a menu bar repainting every few
    /// seconds — takes what the last poll wrote down, and is told how old it
    /// is; an account nothing has polled yet is said to be unread, not broken.
    #[test]
    fn a_cached_listing_is_the_last_readings_without_a_poll() {
        let fixture = Fixture::new("cached");
        for slug in ["a", "b"] {
            let entry = stashed(slug, oauth(&format!("r-{slug}"), 0));
            fixture.stash.save(&entry.slug, &entry.account).expect("stash");
        }
        fixture.stash.set_active(Provider::Claude, "b").expect("active");
        fixture.usage.record("a", &[limit!("session", 42.0)]).expect("records");

        let entries = cached_entries(&fixture.ctx()).expect("lists");

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].entry.slug, "a");
        assert_eq!(entries[0].entry.known()[0].percent, 42.0);
        assert!(entries[0].polled_at.is_some());
        assert!(!entries[0].entry.active);
        assert!(entries[1].entry.active);
        assert!(entries[1].entry.limits.as_ref().unwrap_err().contains("not polled"));
        assert!(entries[1].polled_at.is_none());
    }

    #[test]
    fn forgetting_an_account_takes_its_usage_reading_with_it() {
        let fixture = Fixture::new("forgotten");
        let entry = stashed("gone", oauth("r", 0));
        fixture.stash.save(&entry.slug, &entry.account).expect("stash");
        fixture.usage.record("gone", &[limit!("session", 5.0)]).expect("records");

        remove(&fixture.ctx(), "gone").expect("removes");
        assert!(fixture.reading("gone").is_none());
    }

    // ── a second provider ───────────────────────────────────────────────────

    /// Codex has a live slot of its own; installing a Codex account fills it
    /// and touches nothing of Claude's, pointers included.
    #[test]
    fn switching_to_a_codex_account_fills_codex_own_slot_and_leaves_claude_alone() {
        let fixture = Fixture::new("codex-switch");
        fixture.creds.write(&CredsFile::new(oauth("r-a", LATER))).expect("claude live");
        let claude = stashed("a", oauth("r-a", LATER));
        let gpt = codex_stashed("g", codex_oauth("r-g", LATER, "g@x"));
        fixture.stash.save("a", &claude.account).expect("save");
        fixture.stash.save("g", &gpt.account).expect("save");
        fixture.stash.set_active(Provider::Claude, "a").expect("active");
        let mut accounts = fixture.stash.list().expect("list");

        switch_to(&fixture.ctx(), &mut accounts, &gpt).expect("switches");

        assert_eq!(fixture.codex_live().as_deref(), Some("r-g"));
        let file = fixture.codex.read().expect("read").expect("a file");
        assert_eq!(file.tokens.as_ref().map(|t| t.account_id.as_str()), Some("acct-g@x"));
        assert_eq!(fixture.tokens("a").1.as_deref(), Some("r-a"));
        assert_eq!(fixture.stash.active(Provider::Claude).as_deref(), Some("a"));
        assert_eq!(fixture.stash.active(Provider::Codex).as_deref(), Some("g"));
    }

    /// Codex CLI refreshes the login it holds; the account being left has
    /// its stash entry brought up to what the file held on the way out.
    #[test]
    fn switching_codex_captures_the_outgoing_login_into_its_stash_entry() {
        let fixture = Fixture::new("codex-capture");
        let h = codex_stashed("h", codex_oauth("r-h", LATER, "h@x"));
        let g = codex_stashed("g", codex_oauth("r-g", LATER, "g@x"));
        fixture.stash.save("h", &h.account).expect("save");
        fixture.stash.save("g", &g.account).expect("save");
        let live = codex::AuthFile::default().with(&codex_oauth("r-h-2", LATER + 1, "h@x"));
        fixture.codex.write(&live).expect("codex live");
        fixture.stash.set_active(Provider::Codex, "h").expect("active");
        let mut accounts = fixture.stash.list().expect("list");

        switch_to(&fixture.ctx(), &mut accounts, &g).expect("switches");

        assert_eq!(fixture.tokens("h").0, "r-h-2");
        assert_eq!(fixture.codex_live().as_deref(), Some("r-g"));
    }

    #[test]
    fn reconciling_levels_a_codex_account_against_its_live_file() {
        let fixture = Fixture::new("codex-reconcile");
        let g = codex_stashed("g", codex_oauth("r-g", 1, "g@x"));
        fixture.stash.save("g", &g.account).expect("save");
        let live = codex::AuthFile::default().with(&codex_oauth("r-g-2", LATER, "g@x"));
        fixture.codex.write(&live).expect("codex live");
        // Every switch writes the pointer; a rotated token is what it is for.
        fixture.stash.set_active(Provider::Codex, "g").expect("active");
        let mut accounts = fixture.stash.list().expect("list");

        let ctx = fixture.ctx();
        let live = identify_live(&ctx, &accounts).expect("identifies");
        assert_eq!(live.of(Provider::Codex), Some("g"));
        assert_eq!(live.of(Provider::Claude), None);
        reconcile(&ctx, &mut accounts, &live).expect("reconciles");

        assert_eq!(fixture.tokens("g").0, "r-g-2");
        assert_eq!(accounts[0].account.oauth.refresh_token, "r-g-2");
    }

    #[test]
    fn a_codex_account_is_recorded_under_the_name_its_token_gives() {
        let fixture = Fixture::new("codex-record");
        let recorded = record(
            &fixture.ctx(),
            Provider::Codex,
            codex_oauth("r-g", LATER, "you@x.com"),
            None,
            Installed::Yes,
        )
        .expect("records");

        assert_eq!(recorded.stashed.slug, "you_at_x.com");
        assert_eq!(recorded.stashed.account.provider, Provider::Codex);
        assert_eq!(recorded.stashed.account.email, "you@x.com");
        assert_eq!(recorded.stashed.account.uuid, "acct-you@x.com");
        assert_eq!(recorded.stashed.account.plan_label(), "codex pro");
        assert_eq!(fixture.stash.active(Provider::Codex).as_deref(), Some("you_at_x.com"));
        assert_eq!(fixture.stash.active(Provider::Claude), None);
    }

    /// The same email signed up with both providers gets the provider's name
    /// in front of its slug, and a name given outright never lands on the
    /// other provider's account — that would spend its refresh token.
    #[test]
    fn a_slug_the_other_provider_holds_is_prefixed_or_refused() {
        let fixture = Fixture::new("codex-collide");
        let claude = stashed("you_at_x.com", oauth("r-a", LATER));
        fixture.stash.save("you_at_x.com", &claude.account).expect("save");

        let derived = record(
            &fixture.ctx(),
            Provider::Codex,
            codex_oauth("r-g", LATER, "you@x.com"),
            None,
            Installed::No,
        )
        .expect("records");
        assert_eq!(derived.stashed.slug, "codex-you_at_x.com");

        let error = record(
            &fixture.ctx(),
            Provider::Codex,
            codex_oauth("r-g2", LATER, "other@x.com"),
            Some("you_at_x.com"),
            Installed::No,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("claude"), "{error}");
        assert_eq!(fixture.tokens("you_at_x.com").0, "r-a");
    }

    /// A pool can name accounts of both providers; a Codex request only
    /// ever falls over to a Codex account.
    #[test]
    fn the_desk_keeps_a_mixed_pool_to_the_providers_own_accounts() {
        let fixture = Fixture::new("desk-mixed");
        for (slug, provider) in [("a", Provider::Claude), ("b", Provider::Claude)] {
            let mut entry = stashed(slug, oauth(&format!("r-{slug}"), LATER));
            entry.account.provider = provider;
            fixture.stash.save(slug, &entry.account).expect("save");
        }
        for slug in ["g", "h"] {
            let entry =
                codex_stashed(slug, codex_oauth(&format!("r-{slug}"), LATER, &format!("{slug}@x")));
            fixture.stash.save(slug, &entry.account).expect("save");
        }
        fixture.stash.set_active(Provider::Claude, "a").expect("active");
        fixture.stash.set_active(Provider::Codex, "g").expect("active");
        let pool = ["b", "h", "a", "g"].map(String::from).to_vec();
        let ctx = fixture.ctx();
        let desk = Desk { ctx: &ctx, pool: &pool };

        let codex = desk.grant(Provider::Codex, &[]).expect("grants");
        assert_eq!((codex.slug.as_str(), codex.account_id.as_deref()), ("g", Some("acct-g@x")));
        assert_eq!(desk.grant(Provider::Codex, &["g".into()]).expect("falls over").slug, "h");
        assert_eq!(desk.grant(Provider::Claude, &["a".into()]).expect("falls over").slug, "b");
    }

    #[test]
    fn a_pin_is_a_claude_code_session_and_says_so_for_a_codex_account() {
        let fixture = Fixture::new("codex-pin");
        let g = codex_stashed("g", codex_oauth("r-g", LATER, "g@x"));
        fixture.stash.save("g", &g.account).expect("save");
        let error = pin(&fixture.ctx(), Some("g"), &[]).unwrap_err().to_string();
        assert!(error.contains("Claude Code"), "{error}");
    }

    #[test]
    fn a_cached_listing_marks_the_account_in_use_of_each_provider() {
        let fixture = Fixture::new("codex-cached");
        let a = stashed("a", oauth("r-a", LATER));
        let g = codex_stashed("g", codex_oauth("r-g", LATER, "g@x"));
        fixture.stash.save("a", &a.account).expect("save");
        fixture.stash.save("g", &g.account).expect("save");
        fixture.stash.set_active(Provider::Claude, "a").expect("active");
        fixture.stash.set_active(Provider::Codex, "g").expect("active");

        let entries = cached_entries(&fixture.ctx()).expect("lists");
        assert!(entries.iter().all(|e| e.entry.active));
        assert_eq!(
            entries.iter().find(|e| e.entry.slug == "g").map(|e| e.entry.provider),
            Some(Provider::Codex)
        );
        assert_eq!(
            entries.iter().find(|e| e.entry.slug == "g").map(|e| e.entry.plan.as_str()),
            Some("codex ?")
        );
    }

    // ── the gateway's desk ──────────────────────────────────────────────────

    use crate::codex::Creds as _;
    use crate::serve::Accounts as _;

    /// Far enough off that nothing here reaches for a refresh.
    const LATER: i64 = i64::MAX / 2;

    fn desk_fixture(name: &str, active: Option<&str>, pool: &[&str]) -> Fixture {
        let mut fixture = Fixture::new(name);
        for slug in ["a", "b", "c"] {
            let entry = stashed(slug, oauth(&format!("r-{slug}"), LATER));
            fixture.stash.save(&entry.slug, &entry.account).expect("stash");
        }
        if let Some(slug) = active {
            fixture.stash.set_active(Provider::Claude, slug).expect("active");
        }
        fixture.pool = pool.iter().map(|s| s.to_string()).collect();
        fixture
    }

    #[test]
    fn a_grant_is_the_account_in_use() {
        let fixture = desk_fixture("desk-active", Some("b"), &[]);
        let ctx = fixture.ctx();
        let grant =
            Desk { ctx: &ctx, pool: &fixture.pool }.grant(Provider::Claude, &[]).expect("grants");
        assert_eq!((grant.slug.as_str(), grant.token.as_str()), ("b", "access-r-b"));
        assert_eq!(grant.email, "b@example.com");
    }

    #[test]
    fn a_limited_account_in_use_gives_way_to_the_pool_in_order() {
        let fixture = desk_fixture("desk-pool", Some("b"), &["c", "a"]);
        let ctx = fixture.ctx();
        let desk = Desk { ctx: &ctx, pool: &fixture.pool };
        assert_eq!(desk.grant(Provider::Claude, &["b".into()]).expect("grants").slug, "c");
        assert_eq!(
            desk.grant(Provider::Claude, &["b".into(), "c".into()]).expect("grants").slug,
            "a"
        );
    }

    #[test]
    fn with_every_account_limited_the_refusal_says_so() {
        let fixture = desk_fixture("desk-spent", Some("b"), &["a"]);
        let ctx = fixture.ctx();
        let why = Desk { ctx: &ctx, pool: &fixture.pool }
            .grant(Provider::Claude, &["b".into(), "a".into()])
            .expect_err("nothing left");
        assert!(why.contains("limited"), "{why}");
    }

    #[test]
    fn with_nothing_in_use_the_refusal_points_at_the_switch() {
        let fixture = desk_fixture("desk-none", None, &[]);
        let ctx = fixture.ctx();
        let why = Desk { ctx: &ctx, pool: &fixture.pool }
            .grant(Provider::Claude, &[])
            .expect_err("nothing in use");
        assert!(why.contains("ccs use"), "{why}");
    }

    #[test]
    fn with_nothing_in_use_the_pool_still_answers() {
        let fixture = desk_fixture("desk-pool-only", None, &["c"]);
        let ctx = fixture.ctx();
        assert_eq!(
            Desk { ctx: &ctx, pool: &fixture.pool }
                .grant(Provider::Claude, &[])
                .expect("grants")
                .slug,
            "c"
        );
    }

    /// A session refreshing the live credentials leaves the stash behind. A
    /// rejected token is the moment that shows: the copy a session holds is
    /// the one to hand out.
    #[test]
    fn a_rejected_token_is_replaced_by_the_newer_copy_a_session_holds() {
        let fixture = desk_fixture("desk-stale", Some("b"), &[]);
        fixture.creds.write(&CredsFile::new(oauth("r-b-2", LATER + 1))).expect("live");
        let ctx = fixture.ctx();
        let desk = Desk { ctx: &ctx, pool: &fixture.pool };
        let old = desk.grant(Provider::Claude, &[]).expect("grants");

        let renewed = desk.stale(&old).expect("answers").expect("a newer copy");
        assert_eq!(renewed.token, "access-r-b-2");
        // ...and the stash was brought up to date while it was at it.
        assert_eq!(fixture.tokens("b").0, "r-b-2");
    }

    /// A stash copy that has expired may already have been superseded by the
    /// session's own refresh. The newer copy is what gets handed out, and no
    /// refresh is attempted on the spent one.
    #[test]
    fn a_spent_stash_copy_is_brought_level_before_any_refresh_is_attempted() {
        let fixture = desk_fixture("desk-level", Some("b"), &[]);
        let spent = stashed("b", oauth("r-b", 0));
        fixture.stash.save(&spent.slug, &spent.account).expect("stash");
        fixture.creds.write(&CredsFile::new(oauth("r-b-2", LATER))).expect("live");
        let ctx = fixture.ctx();

        let grant =
            Desk { ctx: &ctx, pool: &fixture.pool }.grant(Provider::Claude, &[]).expect("grants");

        assert_eq!(grant.token, "access-r-b-2");
        assert_eq!(fixture.tokens("b").0, "r-b-2");
    }

    #[test]
    fn a_rejected_token_with_no_newer_copy_is_reported_as_such() {
        let fixture = desk_fixture("desk-fresh", Some("b"), &[]);
        let ctx = fixture.ctx();
        let desk = Desk { ctx: &ctx, pool: &fixture.pool };
        let grant = desk.grant(Provider::Claude, &[]).expect("grants");
        assert!(desk.stale(&grant).expect("answers").is_none());
    }
}
