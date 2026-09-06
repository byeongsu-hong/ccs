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
use crate::creds::{self, Backend, CredStore};
use crate::lock;
use crate::login;
use crate::model::{Account, CredsFile, Limit, Oauth, Stashed, plan_label};
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
/// `installed` is the live credentials, and belongs here only when they are
/// this account's.
fn copies(ctx: &Ctx, entry: &Stashed, installed: Option<&CredsFile>) -> Result<Vec<Oauth>> {
    // Only where there is a pen to read: a pen that was never built holds no
    // copy, and asking after one costs a keychain lookup on the machines that
    // keep credentials there.
    let pen_dir = pen::at(ctx.stash.root(), &entry.slug);
    let pen = match pen_dir.is_dir() {
        true => ctx.backend.confined(&pen_dir).read()?,
        false => None,
    };
    Ok([
        Some(entry.account.oauth.clone()),
        installed.map(|file| file.oauth.clone()),
        pen.map(|file| file.oauth),
    ]
    .into_iter()
    .flatten()
    .collect())
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
fn propagate(ctx: &Ctx, entry: &mut Stashed, oauth: &Oauth, live: Option<&str>) -> Result<()> {
    entry.account.oauth = oauth.clone();
    ctx.stash.save(&entry.slug, &entry.account)?;

    if live == Some(entry.slug.as_str()) {
        install(ctx.creds, oauth)?;
    }
    let pen = pen::at(ctx.stash.root(), &entry.slug);
    if pen.is_dir() {
        install(ctx.backend.confined(&pen).as_ref(), oauth)?;
    }
    Ok(())
}

/// Bring every copy of every account onto the newest credentials on disk for
/// it, before any of them is presented to the server.
///
/// This is the standing repair for the copies this tool does not write: a
/// session refreshing its own credentials leaves every other copy of that
/// account behind, and nothing says so until one of them is used.
fn reconcile(ctx: &Ctx, accounts: &mut [Stashed], live: Option<&str>) -> Result<()> {
    let installed = ctx.creds.read()?;
    for entry in accounts.iter_mut() {
        let mine = (live == Some(entry.slug.as_str())).then_some(installed.as_ref()).flatten();
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
fn persist(
    ctx: &Ctx,
    accounts: &mut [Stashed],
    probes: &[Probe],
    live: Option<&str>,
) -> Result<()> {
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
    let identified = identify(ctx, &accounts, Some(&file));

    let profile = ctx.api.profile(&oauth.access_token)?;
    let limits = ctx.api.usage(&oauth.access_token)?.limits;
    // An account nothing in the stash answers for has nowhere to be recorded;
    // the reading is keyed by slug, and there is no slug to key it under.
    if let Some(slug) = &identified {
        ctx.usage.record(slug, &limits)?;
    }
    let organization = profile.organization.as_ref();
    let plan = plan_label(
        organization.and_then(|o| o.rate_limit_tier.as_deref()),
        organization.and_then(|o| o.organization_type.as_deref()),
    );

    let entry = Entry {
        slug: identified.unwrap_or_default(),
        email: profile.account.email,
        plan,
        active: true,
        limits: Ok(limits),
    };
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
            login::run(ctx.backend, ctx.stash.root(), &claude_binary(), options)?
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
    reconcile(ctx, &mut accounts, live.as_deref())?;
    let probe = probe(ctx.api, stash::resolve(&accounts, &slug)?);
    persist(ctx, &mut accounts, std::slice::from_ref(&probe), live.as_deref())?;
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
                if let Some(next) = watch::rotate(table.entries(), &pool, high) {
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
    let key = serve::key(ctx.stash.root())?;
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("listening on 127.0.0.1:{port}"))?;

    println!("{} serving the API on http://127.0.0.1:{port} as the account in use", stamp());
    if !pool.is_empty() {
        println!("{} falling over to {} when it is limited", stamp(), pool.join(", "));
    }
    println!("paste into pi's models.json:\n{}", serve::pi_config(port));

    let (asks, inbox) = std::sync::mpsc::channel();
    serve::listen(listener, key, asks);
    let desk = Desk { ctx, pool: &pool };
    for ask in inbox {
        ask.answer(&desk);
    }
    Ok(())
}

/// Print the gateway key, for pi's `"apiKey": "!ccs serve --key"`.
pub fn serve_key(ctx: &Ctx) -> Result<()> {
    println!("{}", serve::key(ctx.stash.root())?);
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
    fn live(&self, accounts: &[Stashed]) -> Result<Option<String>> {
        let recorded = self.ctx.stash.active().filter(|s| accounts.iter().any(|a| a.slug == *s));
        match recorded {
            Some(slug) => Ok(Some(slug)),
            None => identify_live(self.ctx, accounts),
        }
    }

    /// The account a request goes out as: the one in use, unless it is among
    /// `avoid`, and then the first pooled account that is not.
    fn choose(&self, accounts: &[Stashed], live: Option<&str>, avoid: &[String]) -> Result<String> {
        let usable = |slug: &str| {
            accounts.iter().any(|a| a.slug == slug) && !avoid.iter().any(|a| a == slug)
        };
        if let Some(slug) = live.filter(|s| usable(s)) {
            return Ok(slug.to_string());
        }
        if let Some(slug) = self.pool.iter().find(|s| usable(s)) {
            return Ok(slug.clone());
        }
        match (live, avoid.is_empty()) {
            (None, true) => bail!("no account is in use; `ccs use` one"),
            _ => bail!("every account is limited: {}", avoid.join(", ")),
        }
    }

    /// `slug`'s credentials, refreshed if they are spent.
    ///
    /// A session may have refreshed them already, leaving the stash behind on
    /// a spent refresh token; the copies are brought level before one is
    /// presented to the server.
    fn hand_out(&self, slug: &str, accounts: &mut [Stashed], live: Option<&str>) -> Result<Grant> {
        let position = accounts.iter().position(|a| a.slug == slug).context("no such account")?;
        if accounts[position].account.oauth.needs_refresh() {
            reconcile(self.ctx, accounts, live)?;
        }
        let entry = &mut accounts[position];
        if let Some(oauth) = freshen(self.ctx.api, &entry.account.oauth)? {
            propagate(self.ctx, entry, &oauth, live)?;
        }
        Ok(grant_of(entry))
    }
}

fn grant_of(entry: &Stashed) -> Grant {
    Grant {
        slug: entry.slug.clone(),
        email: entry.account.email.clone(),
        token: entry.account.oauth.access_token.clone(),
    }
}

impl serve::Accounts for Desk<'_> {
    fn grant(&self, avoid: &[String]) -> Result<Grant, String> {
        let granted = || -> Result<Grant> {
            let mut accounts = self.ctx.stash.list()?;
            let live = self.live(&accounts)?;
            let slug = self.choose(&accounts, live.as_deref(), avoid)?;
            self.hand_out(&slug, &mut accounts, live.as_deref())
        };
        granted().map_err(|e| describe(&e))
    }

    fn stale(&self, grant: &Grant) -> Result<Option<Grant>, String> {
        let renewed = || -> Result<Option<Grant>> {
            let mut accounts = self.ctx.stash.list()?;
            let live = self.live(&accounts)?;
            reconcile(self.ctx, &mut accounts, live.as_deref())?;
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
    reconcile(ctx, &mut accounts, live.as_deref())?;

    // Named outright, the account is taken at its word and the session starts
    // without a round trip; the picker is where usage is shopped for.
    let target = match needle {
        Some(needle) => stash::resolve(&accounts, needle)?.clone(),
        None => {
            let Some(chosen) = choose(ctx, &mut accounts, Verb::Launch)? else { return Ok(()) };
            chosen
        }
    };

    let pen = pen_for(ctx, &target, live.as_deref())?;
    warn_overridden();
    println!("{} is pinned to this session only", target.account.email);
    pen::launch(&pen, &claude_binary(), args)
}

/// The account's pen, with its freshest credentials in place.
///
/// The pen is built before the credentials are settled so that folding them out
/// finds it, which is what leaves the pen and every other copy of the account on
/// the one token a refresh here has not spent.
fn pen_for(ctx: &Ctx, target: &Stashed, live: Option<&str>) -> Result<PathBuf> {
    let pen = pen::prepare(ctx.home, ctx.stash.root(), &target.slug)?;

    let mut entry = target.clone();
    let oauth =
        freshen(ctx.api, &entry.account.oauth)?.unwrap_or_else(|| entry.account.oauth.clone());
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
        reconcile(self.ctx, self.accounts, live.as_deref())?;

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
fn survey(ctx: &Ctx, accounts: &mut [Stashed], style: Style) -> Result<(Table, Option<String>)> {
    let live = identify_live(ctx, accounts)?;
    reconcile(ctx, accounts, live.as_deref())?;
    let probes = probe_all(ctx.api, accounts);
    persist(ctx, accounts, &probes, live.as_deref())?;
    remember(ctx, &probes)?;

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
fn switch_to(ctx: &Ctx, accounts: &mut [Stashed], target: &Stashed) -> Result<usize> {
    let _guard = lock::acquire(ctx.creds.dir())?;
    let live = ctx.creds.read()?;
    if let Some(live) = &live {
        capture_outgoing(ctx, accounts, live, &target.slug)?;
    }
    ctx.creds.write(&merged(live, &target.account.oauth))?;
    ctx.stash.set_active(&target.slug)?;
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
    live: &CredsFile,
    incoming: &str,
) -> Result<()> {
    let Some(slug) = identify(ctx, accounts, Some(live)) else { return Ok(()) };
    if slug == incoming {
        return Ok(());
    }
    let Some(entry) = accounts.iter_mut().find(|a| a.slug == slug) else { return Ok(()) };
    entry.account.oauth = live.oauth.clone();
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

    /// A configuration directory with a stash in it, private to one test so
    /// concurrently running tests never share a path.
    struct Fixture {
        dir: PathBuf,
        creds: Recorder,
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
                home: &self.home,
            }
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
        persist(&fixture.ctx(), &mut accounts, &probes, Some("work")).expect("persist");

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
        reconcile(&fixture.ctx(), &mut accounts, Some("work")).expect("reconcile");

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
        reconcile(&fixture.ctx(), &mut accounts, Some("other")).expect("reconcile");

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
        reconcile(&fixture.ctx(), &mut accounts, Some("work")).expect("reconcile");

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
        reconcile(&fixture.ctx(), &mut accounts, Some("work")).expect("reconcile");

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
        assert_eq!(fixture.stash.active().as_deref(), Some("b"));
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
        fixture.stash.set_active("a").expect("active");
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

    #[test]
    fn forgetting_an_account_takes_its_usage_reading_with_it() {
        let fixture = Fixture::new("forgotten");
        let entry = stashed("gone", oauth("r", 0));
        fixture.stash.save(&entry.slug, &entry.account).expect("stash");
        fixture.usage.record("gone", &[limit!("session", 5.0)]).expect("records");

        remove(&fixture.ctx(), "gone").expect("removes");
        assert!(fixture.reading("gone").is_none());
    }

    // ── the gateway's desk ──────────────────────────────────────────────────

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
            fixture.stash.set_active(slug).expect("active");
        }
        fixture.pool = pool.iter().map(|s| s.to_string()).collect();
        fixture
    }

    #[test]
    fn a_grant_is_the_account_in_use() {
        let fixture = desk_fixture("desk-active", Some("b"), &[]);
        let ctx = fixture.ctx();
        let grant = Desk { ctx: &ctx, pool: &fixture.pool }.grant(&[]).expect("grants");
        assert_eq!((grant.slug.as_str(), grant.token.as_str()), ("b", "access-r-b"));
        assert_eq!(grant.email, "b@example.com");
    }

    #[test]
    fn a_limited_account_in_use_gives_way_to_the_pool_in_order() {
        let fixture = desk_fixture("desk-pool", Some("b"), &["c", "a"]);
        let ctx = fixture.ctx();
        let desk = Desk { ctx: &ctx, pool: &fixture.pool };
        assert_eq!(desk.grant(&["b".into()]).expect("grants").slug, "c");
        assert_eq!(desk.grant(&["b".into(), "c".into()]).expect("grants").slug, "a");
    }

    #[test]
    fn with_every_account_limited_the_refusal_says_so() {
        let fixture = desk_fixture("desk-spent", Some("b"), &["a"]);
        let ctx = fixture.ctx();
        let why = Desk { ctx: &ctx, pool: &fixture.pool }
            .grant(&["b".into(), "a".into()])
            .expect_err("nothing left");
        assert!(why.contains("limited"), "{why}");
    }

    #[test]
    fn with_nothing_in_use_the_refusal_points_at_the_switch() {
        let fixture = desk_fixture("desk-none", None, &[]);
        let ctx = fixture.ctx();
        let why = Desk { ctx: &ctx, pool: &fixture.pool }.grant(&[]).expect_err("nothing in use");
        assert!(why.contains("ccs use"), "{why}");
    }

    #[test]
    fn with_nothing_in_use_the_pool_still_answers() {
        let fixture = desk_fixture("desk-pool-only", None, &["c"]);
        let ctx = fixture.ctx();
        assert_eq!(Desk { ctx: &ctx, pool: &fixture.pool }.grant(&[]).expect("grants").slug, "c");
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
        let old = desk.grant(&[]).expect("grants");

        let renewed = desk.stale(&old).expect("answers").expect("a newer copy");
        assert_eq!(renewed.token, "access-r-b-2");
        // ...and the stash was brought up to date while it was at it.
        assert_eq!(fixture.tokens("b").0, "r-b-2");
    }

    #[test]
    fn a_rejected_token_with_no_newer_copy_is_reported_as_such() {
        let fixture = desk_fixture("desk-fresh", Some("b"), &[]);
        let ctx = fixture.ctx();
        let desk = Desk { ctx: &ctx, pool: &fixture.pool };
        let grant = desk.grant(&[]).expect("grants");
        assert!(desk.stale(&grant).expect("answers").is_none());
    }
}
