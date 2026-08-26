# ccs

Switch Claude Code accounts — including sessions that are already running — and
see what each account has left before you commit to it.

```
   ACCOUNT              PLAN    SESSION           WEEKLY            FABLE             OPUS
 1 jesse@soob.co        max20x  █░░░  10% 3h54m   ███░  55% 6h24m   ████ 100% 6h24m   █░░░  12% 6h24m   <- active
 2 jesse+alt@soob.co    max5x   ░░░░   0%         █░░░  18% 2d11h   █░░░  22% 2d11h   ░░░░   4% 2d11h
 3 work@acme.com        pro     ███░  61% 1h44m   ████  88% 5d19h   —                 ███░  70% 5d19h
```

Each cell carries how much of that limit is gone and how long until it comes
back.

## Why it works on live sessions

Claude Code compares the mtime of its credentials file every time it resolves
credentials, and drops its in-memory auth when it has moved. `ccs` replaces that
file atomically, so a running session picks up the new account on its next
request — no restart, no re-login.

The switch is global: all sessions follow. To confine one session to one
account instead, start it with `ccs pin`.

## Pinning one session

`ccs pin` picks an account the same way `ccs` does — the usage table, arrows,
`enter` — and then starts Claude Code on it:

```sh
ccs pin                     # choose from the table, then launch
ccs pin work                # skip the picker
ccs pin work -- --continue  # anything after `--` goes to Claude Code
```

That session is the only one that moves. Every other session stays on the
account in use, and a later `ccs use` leaves the pinned one where it is. It
says which account it is on wherever Claude Code shows a session name.

A pinned session is a normal session in every other way: same settings, skills,
plugins, MCP servers, project trust and history. `ccs pin` gives the account a
*pen* — a configuration directory of symbolic links back to the real one, whose
only file of its own is the credentials — and points `CLAUDE_CONFIG_DIR` at it.
Nothing is copied, so the pen never falls behind what it mirrors.

Inside a pinned session `ccs` still reads the real stash, and `ccs use` there
re-pins that session alone. `ccs rm` takes an account's pen away with it.

## Setup

```sh
make install
```

That builds and puts `ccs` on your `PATH` under `$CARGO_HOME/bin`. Point it
somewhere else with `PREFIX`:

```sh
sudo make install PREFIX=/usr/local
```

`make help` lists the rest: `make check` runs formatting, clippy and the
tests; `make uninstall` takes it back off.

Capture the account you are already on, then log in to the rest:

```sh
ccs add --current    # stash whoever is logged in right now
ccs add              # log in to another account
ccs add              # ...and another
```

`ccs add` runs `claude auth login` against a throwaway config directory, keeps
the credentials it mints, and destroys the directory. The account you are using
is never signed out and never touched — no logout, no re-login, and running
sessions carry on through it.

Steer the login page with `--email <address>`, `--console`, or `--sso`; stash
under a chosen name with `--name <slug>`.

After that you never log in again — `ccs use` moves between stashed accounts
directly.

## Commands

| command | what it does |
| --- | --- |
| `ccs` | interactive picker: arrows to move, `enter` then `y` to switch, `r` to re-poll, `q` to quit |
| `ccs ls` | every stashed account with its session, weekly, and per-model limits |
| `ccs use <account>` | switch to an account; running sessions follow |
| `ccs pin [<account>]` | start a session confined to one account, leaving every other session alone |
| `ccs add` | log in to another account and stash it, without disturbing the one in use |
| `ccs add --current` | stash whichever account is logged in right now |
| `ccs rm <account>` | forget a stashed account |
| `ccs status` | limits for the account currently in use, with reset times |

`<account>` is a slug, an email, an unambiguous prefix of either, or the index
from `ccs ls`. Without one, `ccs pin` opens the picker. `ccs ls --json` and
`ccs status --json` emit the same data for scripts and status lines.

The picker always asks before switching, and says what is spent if anything is.
On the command line `ccs use` switches straight away, asking only when the
target has a limit already at 100%; `--force` skips that question.

Polling happens with the picker on screen — the first load, every `r`, and once
a minute on its own — so the list is never taken away to fetch. An unattended
poll waits for a lull rather than freezing the list under you, the footer says
how old the reading is, and a poll that fails says so there and leaves the last
good reading standing.

## Limits

The columns come from whatever the API reports rather than a fixed list, so a
newly scoped model appears on its own:

- **session** — the rolling five-hour window
- **weekly** — the all-models weekly window
- one column per model with its own weekly limit (Fable, Opus, …)

## Where things live

```
~/.claude/.credentials.json     the live account, as Claude Code reads it
~/.claude/ccs/accounts/*.json   one stashed account each, mode 0600
~/.claude/ccs/pens/<account>/   one pinned session's configuration each
~/.claude/ccs/state.json        which slug is currently installed
~/.claude/ccs/.login-<pid>/     a login in progress, destroyed when it ends
```

Stash files hold OAuth refresh tokens. They are written `0600` inside a `0700`
directory, and they are worth exactly as much as a login — do not sync them
anywhere you would not sync a password.

`CLAUDE_CONFIG_DIR` is honoured, the same way Claude Code honours it, and
`CCS_CLAUDE_BINARY` points at a `claude` that is not on `PATH`.

A login directory left behind by an interrupted run is swept on the next `ccs
add` — each is named after the process that owns it, so a run still in flight
is never swept out from under itself.

## Keeping tokens alive

Claude Code refreshes access tokens in place, so a stashed copy goes stale as
soon as its account is used. `ccs` folds the live tokens back into the stash on
the way out of an account, and refreshes any stashed token that has expired
before polling it. A refresh can rotate the refresh token, so the result is
written down before anything else happens — and when the refreshed account is
the live one, the credentials file is updated too, so running sessions are not
left holding a token that no longer works.
