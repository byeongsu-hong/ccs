# ccs

Switch Claude Code accounts without logging out, and see what each one has left
before you commit to it.

![the ccs picker](docs/picker.svg)

If you run more than one Claude subscription you know the shape of this problem.
You are deep in something, the weekly limit lands, and the only road to your
other account is `/login` — which signs you out everywhere, drops whatever
sessions you had going, and tells you nothing about whether the account you are
moving to has any room left either. An hour later you do the whole thing again in
reverse.

`ccs` keeps every account logged in at once, shows you where each one stands, and
moves between them in place. Sessions that are already running follow along on
their next request. Nothing restarts, and nothing gets signed out.

## What it is actually good at

**Seeing before switching.** The table above is the whole point. The five-hour
session window, the all-models weekly window, and any weekly window scoped to a
single model — Fable, today — for every account at once: how much is gone, and
how long until it comes back. You pick the account with room instead of
discovering there wasn't any two prompts later.

**Never logging in again.** Each account is stashed with its own credentials.
Switching installs one of them over the live file; it never signs the other one
out. Going back is another switch, not another login.

**Live sessions.** A switch reaches sessions that are already running, mid-task,
without a restart. That is the difference between "I'll switch accounts" being a
two-second decision and being a five-minute interruption.

**Pinning.** One session on one account, every other session left where it is.
This is the feature that changes how you work — see below.

## Install

You need a Rust toolchain. Then:

```sh
git clone https://github.com/orthory/ccs
cd ccs
make install
```

That builds a release binary and puts `ccs` on your `PATH` under `$CARGO_HOME/bin`.
Somewhere else:

```sh
sudo make install PREFIX=/usr/local
```

`make help` lists the rest — `make check` runs formatting, clippy and the tests,
`make uninstall` takes it back off.

## Getting your accounts in

Start with the one you are already signed in as, then log in to the others:

```sh
ccs add --current    # stash whoever is logged in right now
ccs add              # log in to another account
ccs add              # ...and another
```

`ccs add` runs `claude auth login` against a throwaway config directory, keeps the
credentials it mints, and destroys the directory. The account you are currently
using is never touched — no logout, no re-login, and sessions running against it
carry on straight through the login you are doing in the next window.

Steer the login page with `--email <address>`, `--console` (Console billing rather
than a subscription), or `--sso`. Stash under a name of your choosing with
`--name <slug>`; otherwise the slug comes from the email.

After that you are done logging in. `ccs use` moves between stashed accounts
directly.

## Everyday use

Run `ccs` with no arguments and you get the picker:

```
     ACCOUNT              PLAN    SESSION           WEEKLY            FABLE
>  1 you@example.com      max20x  █░░░  10% 3h54m   ███░  55% 6h24m   ████ 100% 6h24m   <- active
   2 you+alt@example.com  max5x   ░░░░   0%         █░░░  18% 2d11h   █░░░  22% 2d11h
   3 team@example.org     pro     ███░  61% 1h44m   ████  88% 5d19h   ███░  70% 5d19h

  session          █░░░  10%   resets in 3h 54m
  weekly           ███░  55%   resets in 6h 24m
  Fable            ████ 100%   resets in 6h 24m   spent

  up/down select   enter switch   esc unselect   r refresh   q quit      updated just now
```

Arrows or `j`/`k` move, `home`/`end` jump to the ends, and the block under the
table details whichever account you are sitting on. `enter` asks before it does
anything, and says what is spent if anything is:

```
  you@example.com has no Fable left. Switch anyway? [y/n]
```

Answering yes switches, and the picker stays where it is — the active marker moves
to the row you picked and the footer says what happened:

```
     ACCOUNT              PLAN    SESSION           WEEKLY            FABLE
   1 you@example.com      max20x  █░░░  10% 3h54m   ███░  55% 6h24m   ████ 100% 6h24m
>  2 you+alt@example.com  max5x   ░░░░   0%         █░░░  18% 2d11h   █░░░  22% 2d11h   <- active
   3 team@example.org     pro     ███░  61% 1h44m   ████  88% 5d19h   ███░  70% 5d19h

  switched to you+alt@example.com
```

Nothing is re-fetched to draw that: a switch spends nobody's limits, so the
readings on screen are as true after it as they were before. Switch again from
the same table if the first one was wrong — the terminal you leave the picker in
still gets the record of where you ended up.

`esc` backs out one step at a time. From a question it takes you back to the list;
from the list it puts the selection away entirely, which leaves `enter` with
nothing to act on:

```
     ACCOUNT              PLAN    SESSION           WEEKLY            FABLE
   1 you@example.com      max20x  █░░░  10% 3h54m   ███░  55% 6h24m   ████ 100% 6h24m   <- active
   2 you+alt@example.com  max5x   ░░░░   0%         █░░░  18% 2d11h   █░░░  22% 2d11h
   3 team@example.org     pro     ███░  61% 1h44m   ████  88% 5d19h   ███░  70% 5d19h

  up/down select   r refresh   q quit      updated just now
```

Move again and the selection comes back. One more `esc` from there leaves the
picker; `q` and `ctrl-c` leave from anywhere.

If you already know where you are going, skip the picker:

```sh
ccs use work            # slug, email, unambiguous prefix, or the index from `ccs ls`
ccs use 2
ccs ls                  # the same table, printed and gone
ccs status              # just the account in use, in detail
```

`ccs use` switches straight away and is the one that leaves you at your prompt;
the picker is where you switch and then keep looking. It only stops to ask when
the account you are switching to has a limit already at 100%, and `--force` skips
even that.

## Pinning a session to one account

A switch is global — every session follows it. Sometimes that is exactly wrong:
you want this terminal on the work account and everything else left alone. That
is `ccs pin`:

```sh
ccs pin                     # pick from the table, then launch
ccs pin work                # skip the picker
ccs pin work -- --continue  # anything after `--` is handed to Claude Code
```

It picks an account the same way `ccs` does, then starts Claude Code on it —
which is the one thing the picker cannot stay up for, because the session takes
the terminal. That
session is the only thing that moves. Every other session stays on the account in
use, and a later `ccs use` leaves the pinned one exactly where it is. Nothing
displays the account on its own. `ccs status` inside the session names it, and
`CLAUDE_CONFIG_DIR` points at the pen — which is named for the account — so a
status line can keep the answer in front of you without asking the network.

Run several at once, one terminal each, and you are working three accounts in
parallel with three separate limit budgets.

### What a pinned session shares, and what it doesn't

A pinned session is a normal session in every way but one. It gets a *pen* — a
configuration directory of symbolic links back to your real one, whose only file
of its own is the credentials — and `CLAUDE_CONFIG_DIR` points at it.

**Shared, live, with every other session:** settings, skills, plugins, agents,
slash commands, MCP servers, project trust, conversation history, todos — and the
global `.claude.json` alongside them. These are links, not copies, so a pen never
drifts from what it mirrors, and a skill you add inside a pinned session is a skill
every session has. There is no syncing step because there is nothing to sync.

**The pen's own, shared with nothing:** the credentials. That is the entire point
of the pen, and it is the only real file in it. A `ccs use` elsewhere rewrites the
live credentials and leaves the pen's standing.

**Also not mirrored:** the account stash itself and the credential write lock. The
stash stays reachable from inside a pen anyway — a pen records the configuration it
was cut from, so `ccs` inside a pinned session reads your real accounts, and
`ccs use` in there re-pins that one session rather than moving everybody. `ccs rm`
takes an account's pen away with it.

If Claude Code ever replaces one of those links with a real file of its own, the
pen keeps that file from then on rather than clobbering it back to a link.

## How it works

**Switching a live session.** Claude Code checks the mtime of its credentials file
each time it resolves credentials, and drops its in-memory auth when the file has
moved underneath it. `ccs` writes the replacement to a sibling temp file and
renames it into place, so a reader sees either the old file or the new one and
never a half-written one — and the rename freshens the mtime that running sessions
are watching. That is the whole trick. There is no daemon and no IPC.

**On macOS, where the credentials are not a file.** Claude Code keeps them in the
login keychain there, reads that in preference to the file, and writes the file
only when the keychain turns it away — so a switch written to the file would be a
switch nothing reads. `ccs` writes the item instead, under the name Claude Code
gives it: `Claude Code-credentials`, keyed to your login name. A session with no
credentials file to watch compares the token in the keychain instead, so a switch
reaches running sessions the same way it does anywhere else.

The item is namespaced by configuration directory — `CLAUDE_CONFIG_DIR` decides
which — which is what gives every pen credentials of its own, and what keeps a
pinned session pinned. `ccs rm` takes an account's item away with its pen, and an
interrupted `ccs add` leaves none behind.

**Not fighting over the file.** Claude Code takes a lock beside the credentials
file when it refreshes tokens. `ccs` takes the same lock, so a switch can't
interleave with a refresh and lose one of the two writes.

**Keeping stashed tokens alive.** This is the part that is easy to get wrong.
Claude Code refreshes access tokens in place, so a stashed copy goes stale the
moment its account is used, and a refresh can *rotate* the refresh token — which
makes the copy you were holding worthless. So `ccs` folds the live tokens back into
the stash on the way out of an account, refreshes any stashed token that has
expired before polling it, and writes down whatever comes back before doing
anything else with it. When the account it refreshed is the live one, the
credentials file gets the new token too, so running sessions are never left holding
one that has been superseded.

**Reading usage.** The limits come from the same OAuth endpoint Claude Code uses
for `/status`, one request per stashed account. The columns are built from whatever
the API reports rather than from a fixed list, so a newly scoped model turns up as
its own column without a change here.

**Leaving the reading behind.** Every poll is written down on the way past, one
file per account under `ccs/usage/`, stamped with when it was taken. Nothing in
`ccs` reads them back — they are there for anything that has to show where an
account stands far more often than a poll can be afforded, a status line above a
prompt being the case they exist for. The limits are stored exactly as the
endpoint reported them, so a reader draws whatever windows it finds rather than
knowing a list of them. A failed poll leaves the last reading standing instead of
blanking it, and the stamp is what says whether it is still worth believing;
`ccs status` is the cheapest way to freshen one, costing the account in use a
single round trip. A reading looks like this, and nothing but the endpoint decides
how many limits are in it:

```json
{
  "polled_at": "2026-08-28T02:31:04Z",
  "limits": [
    { "kind": "weekly_all", "percent": 55.0, "severity": "normal",
      "resets_at": "2026-09-01T14:00:00Z", "scope": null },
    { "kind": "weekly_scoped", "percent": 100.0, "severity": "critical",
      "resets_at": "2026-09-01T14:00:00Z",
      "scope": { "model": { "display_name": "Fable" } } }
  ]
}
```

**Polling.** The picker polls with the screen already up — on open, on `r`, and
every ten minutes on its own — so the list is never taken away to fetch. An
unattended poll waits for a lull rather than freezing the list under you, the
footer says how old the reading is, and a poll that fails says so there and leaves
the last good reading standing. The interval is long because each poll costs a
request per account against an endpoint that rate-limits. The countdowns don't wait
on it: they are recomputed from the reset instants every time the screen is
painted, so only the percentages are as old as the footer says.

## Commands

| command | what it does |
| --- | --- |
| `ccs` | the picker |
| `ccs ls` | every stashed account and what it has left |
| `ccs use <account>` | switch; running sessions follow |
| `ccs pin [<account>]` | start a session confined to one account |
| `ccs add` | log in to another account and stash it |
| `ccs add --current` | stash whichever account is logged in right now |
| `ccs rm <account>` | forget a stashed account |
| `ccs status` | limits for the account in use, with reset times |

`<account>` is a slug, an email, an unambiguous prefix of either, or the index from
`ccs ls`. `ccs pin` without one opens the picker; `ccs use` without one is an
error rather than a guess.

`-f`/`--force` switches even into an account with nothing left. `--json` on `ls`
and `status` gives you the same data for scripts. A status line wants the cache
under `ccs/usage/` instead — it repaints far more often than a poll can be
afforded.

## Limits

Columns are built from whatever the usage endpoint reports rather than from a
list in the code, so this describes what it returns today and is not a schema.
Today that is three:

- **session** — the rolling five-hour window
- **weekly** — the all-models weekly window
- **Fable** — the weekly window scoped to that model

A model-scoped limit is drawn under the model's own name, so if the endpoint
starts scoping another one it gets a column without a change here.

Green is fine, yellow is worth knowing about, red is spent. An account with any
limit at 100% is dimmed in the table, and `ccs` asks twice before walking into it.
A `—` means that account reported no limit of that kind at all, where another
account did.

## Where things live

```
~/.claude/.credentials.json     the live account, as Claude Code reads it
                                — on macOS, the login keychain instead
~/.claude/ccs/accounts/*.json   one stashed account each, mode 0600
~/.claude/ccs/pens/<account>/   one pinned session's configuration each
~/.claude/ccs/usage/*.json      what each account last had left, and when
~/.claude/ccs/state.json        which slug is currently installed
~/.claude/ccs/.login-<pid>/     a login in progress, destroyed when it ends
```

**Those account files hold OAuth refresh tokens.** They are written `0600` inside a
`0700` directory, and each one is worth exactly as much as a password. Don't sync
them anywhere you wouldn't sync a password. That is as true on macOS, where the
stash is these same files: the keychain holds the account in use and a pinned
session's, and the stash behind them is on disk either way.

A keychain item `ccs` creates for a pen is opened to the applications you run, the
way the plain file already is to the processes you run — otherwise a pinned session
would start by asking you to unlock something. The item Claude Code made for the
account in use is updated in place and keeps the access it came with.

A login directory left behind by an interrupted run is swept on the next `ccs add`.
Each is named after the process that owns it, so a run still in flight is never
swept out from under itself.

## Environment

`CLAUDE_CONFIG_DIR` is honoured the same way Claude Code honours it, which is what
lets `ccs` work correctly from inside a pinned session, and on macOS what names the
keychain item a session reads. `CLAUDE_SECURESTORAGE_CONFIG_DIR` is honoured there
too, for the same reason. `CCS_CLAUDE_BINARY` points at a `claude` that isn't on
`PATH`. `NO_COLOR` does what you expect.

`ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN` and `CLAUDE_CODE_OAUTH_TOKEN` take
precedence over the stored credentials for any session that inherits them — so a
session with one of those set ignores whatever account you switched to. `ccs` says
so rather than letting you wonder.

## Licence

MIT. See [LICENSE](LICENSE).
