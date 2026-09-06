# Codex accounts: a second provider in the stash

## Goal

Stash, read and switch OpenAI Codex (ChatGPT subscription) accounts the way
Claude accounts are handled today, in separate sections of the same picker,
watcher, menu bar app and gateway. Two live slots — Claude Code's credentials
and Codex's `auth.json` — each with its own account in use, switched
independently.

## What is known

- Codex CLI keeps its login in `$CODEX_HOME/auth.json` (default
  `~/.codex/auth.json`): `{auth_mode: "chatgpt", OPENAI_API_KEY, tokens:
  {id_token, access_token, refresh_token, account_id}, last_refresh}`. It
  honours `CODEX_HOME`, so a login can be run in a throwaway directory as
  `ccs add` already does with `CLAUDE_CONFIG_DIR`. Writing the file does not
  establish when every running Codex client adopts it: verify with `/status`
  and resume if the session still reports its previous account.
- Identity is in the `id_token` claims: `email`, and under
  `https://api.openai.com/auth`: `chatgpt_account_id`, `chatgpt_plan_type`.
  The access token's `exp` claim is its expiry.
- Refresh: `POST https://auth.openai.com/oauth/token` with
  `{grant_type: "refresh_token", refresh_token, client_id:
  "app_EMoamEEZ73f0CkXaXp7hrann"}` → `{access_token, refresh_token,
  expires_in[, id_token]}`.
- Usage: `GET https://chatgpt.com/backend-api/wham/usage` with the bearer and
  `chatgpt-account-id` → `{email, plan_type, rate_limit: {primary_window,
  secondary_window}, additional_rate_limits: [{limit_name, rate_limit}],
  model_usage: {model_id: {available, available_at, credits_would_enable}}}`,
  each window `{used_percent, limit_window_seconds, reset_at}`.
- pi/Aside's `openai-codex` provider sends `Authorization: Bearer <key>` and a
  `chatgpt-account-id` header it reads out of the key, which it takes for a
  JWT (`https://api.openai.com/auth`.`chatgpt_account_id`). Its transport is
  WebSocket first; when the socket never opens it falls back to SSE on
  `POST <baseUrl>/codex/responses`. Overriding the built-in provider's
  `baseUrl` keeps its models.

## Model

`Account` gains `provider: claude | codex` (absent means `claude`, so every
existing stash file still reads). Codex tokens ride in the same `Oauth`
shape: `access_token`, `refresh_token`, `expires_at`, and `idToken` and
`accountId` in the flattened extra map. Everything that keys on `expires_at`
or `refresh_token` — copies, newest, propagate, reconcile — works unchanged.

`state.json` keeps one pointer per provider: `active` (Claude, as before) and
`codex`.

`Entry` (table rows) and the JSON views carry `provider`. The plan label of a
Codex account remains prefixed, `codex pro`, for existing JSON/menu bar consumers.
The terminal table groups accounts by provider, then email, matching numeric
account resolution. Claude has its own dynamic limit columns. Codex shows
shared quota windows and additional named pool rows per account. Model
availability has its own columns: `gpt-6-astra` is labeled Astra, while other
IDs retain their names. Availability is independent of numeric quota windows.
Spark is hidden from human-facing terminal output and selection warnings; raw
JSON/cache data retain it. The POOL column appears only when a visible named
quota pool exists. Missing model availability is unknown, not available.
Two accounts can be `active`, one per provider; marking one active unmarks only
its provider's rows.

## Modules

- `src/codex.rs` — the Codex side, one file: `AuthFile` (auth.json in and
  out, unknown fields kept), `Store` (read/write at `CODEX_HOME` or
  `~/.codex`, atomic, 0600, under the same directory lock the Claude side
  takes, which Codex CLI does not know — its own writes can still interleave
  with a switch, and the levelling on the next read is what repairs that),
  JWT claim reading, `Client` (refresh, usage), and `limits(&Usage)` mapping
  windows to `Limit`s: a window of 18000s is `session`, 604800s is
  `weekly_all`, anything else `<n>s`; each `additional_rate_limits` entry
  retains every reported window, scoped by `limit_name`. Nullable or missing
  additional pools are accepted. `reset_at` becomes RFC 3339. Old cached scoped
  windows lack a duration and are labeled `CACHED WINDOW` until refreshed.
  `model_usage` is carried through normalization, polling, cache and JSON. Older
  cache files without it still read. A fresh response replaces the complete
  snapshot; failed probes preserve the previous snapshot and timestamp.
- `src/render.rs` — Astra availability displays available/unavailable, a future
  countdown when supplied, and a credits-unlock hint when reported. RFC 3339
  strings and Unix seconds are accepted for the optional timestamp. A past or
  unreadable timestamp never flips the availability flag. An unavailable model
  adds a selection warning; it does not trigger shared-quota rotation/notices.
  The picker has only account rows and a keys/confirmation footer; detailed
  limit prose remains in `ccs status`.
- `src/cmd.rs` — `Ctx` gains `codex: &dyn codex::Creds` and `codex_api:
  &codex::Client`. `live`, `identify`, `install`, `copies`, `propagate`,
  `switch_to`, `probe`, `record` branch on the account's provider. `ccs pin`
  launches the selected account's client. Codex copies are identified by their
  ChatGPT workspace ID and user email, including pins switched in place. Refreshes
  propagate to matching copies; a pin switch or capture leaves the global pointer alone.
  `ccs status` reports each provider that has live credentials; --codex/--claude
  selects one, and --cached reads its last poll without refreshing credentials.
- `src/login.rs` — `run` gains the provider: a Codex login runs `codex
  login` with `CODEX_HOME` pointed at the scratch directory.
- `src/watch.rs` — `rotate` runs per provider, over that provider's rows and
  the pool members among them.
- `src/serve.rs` — a second key, `codex.key`: a JWT with `alg: none` whose
  payload names `chatgpt_account_id: "ccs"` and carries 32 random hex, so
  pi decodes an account id and the gateway checks the whole string. Paths
  under `/backend-api/` are relayed to `https://chatgpt.com/backend-api/`
  as the active Codex account, with both `Authorization` and
  `chatgpt-account-id` replaced; `/v1/` stays Anthropic. An `Upgrade:
  websocket` request is answered `426`, which sends pi to SSE. `Grant`
  carries the account id; `Accounts::grant` takes the provider; the 429
  pool is filtered to it. `ccs serve --key codex` prints the Codex key; the
  startup snippet covers both providers:

  ```json
  { "providers": {
      "anthropic":    { "baseUrl": "http://127.0.0.1:4141", "apiKey": "!ccs serve --key" },
      "openai-codex": { "baseUrl": "http://127.0.0.1:4141/backend-api", "apiKey": "!ccs serve --key codex" } } }
  ```

- `src/cli.rs`, `src/main.rs`, `src/picker.rs` — interactive provider menus for
  add/capture, status (including both), notify and gateway key commands. Flags
  bypass menus. Noninteractive add requires a selector; JSON/piped status keeps
  both providers, and piped gateway key keeps its existing Claude default.
  Cancellation restores the terminal before any command action. Claude login
  flags are validated after provider selection and before credentials are read.
- `app/` — `Account.provider` decoded; rows show the provider in the plan
  text already. No other change.

## Out of scope

Pointing Codex CLI itself at the gateway; a WebSocket relay; API-key
(`auth_mode: apikey`) logins; OS credential-store integration; native Codex
status-line customization and per-turn token accounting.

## Session integration

- `src/pen.rs`: a Codex pin records its source home in `.ccs-codex-pen.json`,
  shares only configuration through symlinks, and keeps auth and runtime state
  private. Codex launches with `CODEX_HOME` and file credential storage.
  Removing an account removes matching pinned credentials, preserving history.
- `src/notify.rs`: Codex subscriptions live in `notify-codex.json`, separately
  from Claude's existing inbox registrations. A subscription records its thread,
  home, executable and kinds. Registration checks `queue --help` for thread and
  message support. Delivery invokes that executable with literal arguments,
  under the registered home, and reports failure without dropping subscriptions.
  Claude registrations retain their permission attestation and now remember
  their key directory. Switch notices are confined to the home that changed.
- `src/watch.rs`: every event carries its provider. High usage and reset
  comparisons use that provider's active account, and delivery routes accordingly.
- `src/cli.rs`: --codex/--claude selects status or notice provider. Notification
  auto-detection requires exactly one of the Claude inbox variable and
  CODEX_THREAD_ID; nested clients select explicitly or through the terminal menu.
  --cached on status selects the account from its live file and reads the existing
  cache, including the poll timestamp.

Codex queue support was checked through the installed CLI 0.153.4 help. Clients
without that command can use cached status but cannot subscribe to queued notices.
Subscriptions are opt-in conversation messages; they do not install hooks, alter
permissions, or translate Claude's configuration into Codex configuration.
