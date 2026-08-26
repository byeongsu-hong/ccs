# ccs

Switch Claude Code accounts — including sessions that are already running — and
see what each account has left before you commit to it.

```
   ACCOUNT              PLAN    SESSION    WEEKLY     FABLE      OPUS
 1 jesse@soob.co        max20x  █░░░   6%  ███░  54%  ████ 100%  █░░░  12%  <- active
 2 jesse+alt@soob.co    max5x   ░░░░   0%  █░░░  18%  █░░░  22%  █░░░   4%
 3 work@acme.com        pro     ███░  61%  ████  88%  —          ███░  70%
```

## Why it works on live sessions

Claude Code polls the mtime of its credentials file and drops its in-memory auth
when it changes. `ccs` replaces that file atomically, so every running session
picks up the new account within a few seconds — no restart, no re-login.

The switch is global: all sessions follow. To pin a single session to one
account instead, launch it with its own `CLAUDE_CONFIG_DIR`.

## Setup

```sh
cargo install --path .
```

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
| `ccs` | interactive picker: arrows to move, `enter` to switch, `r` to re-poll, `q` to quit |
| `ccs ls` | every stashed account with its session, weekly, and per-model limits |
| `ccs use <account>` | switch to an account; running sessions follow |
| `ccs add` | log in to another account and stash it, without disturbing the one in use |
| `ccs add --current` | stash whichever account is logged in right now |
| `ccs rm <account>` | forget a stashed account |
| `ccs status` | limits for the account currently in use, with reset times |

`<account>` is a slug, an email, an unambiguous prefix of either, or the index
from `ccs ls`. `ccs ls --json` and `ccs status --json` emit the same data for
scripts and status lines.

Switching to an account with a limit already at 100% asks first; `--force`
skips the question.

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
