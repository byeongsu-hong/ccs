# Codex accounts: a second provider in the stash

## Goal

Stash, read and switch OpenAI Codex (ChatGPT subscription) accounts the way
Claude accounts are handled today, side by side in the same table, picker,
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
  secondary_window}, additional_rate_limits: [{limit_name, rate_limit}]}`,
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
Codex account is prefixed, `codex pro`, so the table reads without a column.
Two rows can be `active`, one per provider; marking one active unmarks only
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
  becomes one scoped limit named after `limit_name`, taking its weekly
  window (or its only one). `reset_at` becomes RFC 3339.
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

- `src/cli.rs` — `ccs add --codex`, `ccs add --current --codex`, `ccs serve
  --key [codex]`.
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
  auto-detection uses the Claude inbox variable first, then CODEX_THREAD_ID;
  nested clients can select explicitly. --cached on status selects the account
  from its live file and reads the existing cache, including the poll timestamp.

Codex queue support was checked through the installed CLI 0.153.4 help. Clients
without that command can use cached status but cannot subscribe to queued notices.
Subscriptions are opt-in conversation messages; they do not install hooks, alter
permissions, or translate Claude's configuration into Codex configuration.
