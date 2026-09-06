//! Tell the sessions that asked about a switch, through the inbox each one
//! listens on.
//!
//! A session subscribes with `ccs notify` from inside itself, naming the
//! kinds of notice it wants (see `watch::KINDS`): Claude Code exports its
//! inbox path as `CLAUDE_CODE_MESSAGING_SOCKET`, and that is what gets
//! remembered. Every session also publishes `sessions/<pid>.<hash>.key`
//! under its configuration directory, holding the token its inbox expects.
//!
//! The wire format is Claude Code's own peer messaging: one auth line, one
//! user-message line, done. It is undocumented, so a mismatch after an upgrade
//! costs only the notice, never the switch.
//!
//! A session that bypasses permission prompts holds a notice for review unless
//! the sender is in its own process tree or attests the same permission mode.
//! A subscriber that runs that way says so with `--bypass`, and every notice
//! to it carries the attestation; then `ccs watch` can run anywhere.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::fsx::write_atomic;
use crate::watch::KINDS;

const SOCKET_ENV: &str = "CLAUDE_CODE_MESSAGING_SOCKET";
const FILE: &str = "notify.json";
const TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Default, Serialize, Deserialize)]
struct Subscription {
    kinds: Vec<String>,
    /// The permission-mode class the session runs in, when it said: "bypass"
    /// or "prompting". Attested on every notice so the inbox accepts it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
}

type Subscribers = BTreeMap<String, Subscription>;

/// Add the calling session to the list, for `kinds` (every kind when empty),
/// or drop it. `bypass` says the session runs without permission prompts.
pub fn subscribe(root: &Path, on: bool, kinds: &[String], bypass: bool) -> Result<()> {
    if let Some(bad) = kinds.iter().find(|k| !KINDS.contains(&k.as_str())) {
        bail!("unknown notice kind {bad:?}; the kinds are {}", KINDS.join(", "));
    }
    let sock = std::env::var(SOCKET_ENV).ok().filter(|s| !s.is_empty()).with_context(|| {
        format!("{SOCKET_ENV} is not set; run this from inside a Claude Code session")
    })?;
    if on && !Path::new(&sock).exists() {
        bail!("{sock} does not exist; is this session's inbox up?");
    }
    let mut subs = subscribers(root);
    subs.remove(&sock);
    if on {
        let kinds =
            if kinds.is_empty() { KINDS.map(String::from).to_vec() } else { kinds.to_vec() };
        let mode = bypass.then(|| "bypass".to_string());
        println!("will notify {sock} of {}", kinds.join(", "));
        subs.insert(sock, Subscription { kinds, mode });
    } else {
        println!("will no longer notify {sock}");
    }
    save(root, &subs)
}

/// Push a `kind` notice reading `text` to every subscriber to that kind still
/// reachable. Returns how many took it; a subscriber whose inbox is gone is
/// forgotten.
pub fn broadcast(config: &Path, root: &Path, kind: &str, text: &str) -> usize {
    let mut subs = subscribers(root);
    let wanted: Vec<(String, Option<String>)> = subs
        .iter()
        .filter(|(_, s)| s.kinds.iter().any(|k| k == kind))
        .map(|(sock, s)| (sock.clone(), s.mode.clone()))
        .collect();
    if wanted.is_empty() {
        return 0;
    }
    let sessions = config.join("sessions");
    let mut told = 0;
    for (sock, mode) in wanted {
        let attest = mode.as_deref().map(|m| format!(" from-mode=\"{m}\"")).unwrap_or_default();
        let body = format!(
            "<cross-session-message from-name=\"ccs\"{attest}>\nccs {kind}: {text}\n</cross-session-message>"
        );
        if send(&sessions, &sock, &body, mode.as_deref()).is_some() {
            told += 1;
        } else {
            subs.remove(&sock);
            let _ = save(root, &subs);
        }
    }
    told
}

fn subscribers(root: &Path) -> Subscribers {
    fs::read(root.join(FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save(root: &Path, subs: &Subscribers) -> Result<()> {
    write_atomic(&root.join(FILE), &serde_json::to_vec_pretty(subs)?, 0o600)
}

/// The token a session's inbox expects, from the key it published beside its
/// registration. The key name carries the pid and a hash of the socket path;
/// the pid alone is enough to pick it out.
fn token(sessions: &Path, sock: &str) -> Option<String> {
    let pid = Path::new(sock).file_stem()?.to_str()?;
    let prefix = format!("{pid}.");
    fs::read_dir(sessions).ok()?.flatten().find_map(|e| {
        let name = e.file_name().into_string().ok()?;
        if !name.starts_with(&prefix) || !name.ends_with(".key") {
            return None;
        }
        let key: Value = serde_json::from_slice(&fs::read(e.path()).ok()?).ok()?;
        Some(key.get("peerToken")?.as_str()?.to_owned())
    })
}

fn send(sessions: &Path, sock: &str, body: &str, mode: Option<&str>) -> Option<()> {
    let token = token(sessions, sock)?;
    let mut s = UnixStream::connect(sock).ok()?;
    s.set_write_timeout(Some(TIMEOUT)).ok()?;
    let msg_id = format!("ccs-{}-{}", std::process::id(), crate::model::now_ms());
    let lines = format!(
        "{}\n{}\n",
        json!({"type": "auth", "token": token}),
        json!({
            "type": "user",
            "message": {"role": "user", "content": body},
            "priority": "next",
            "from": "ccs",
            "msg_id": msg_id,
            "from_mode": mode,
        })
    );
    s.write_all(lines.as_bytes()).ok()?;
    // Stay alive until the inbox has looked at the message: it vets the sender
    // by walking its process ancestry in /proc, and a sender that has already
    // exited by then is treated as a stranger and held for review.
    let _ = s.shutdown(Shutdown::Write);
    let _ = s.set_read_timeout(Some(TIMEOUT));
    let _ = s.read(&mut [0; 64]);
    Some(())
}
