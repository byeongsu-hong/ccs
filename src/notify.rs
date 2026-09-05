//! Tell the sessions that asked about a switch, through the inbox each one
//! listens on.
//!
//! A session subscribes with `ccs notify` from inside itself: Claude Code
//! exports its inbox path as `CLAUDE_CODE_MESSAGING_SOCKET`, and that is what
//! gets remembered. Every session also publishes `sessions/<pid>.<hash>.key`
//! under its configuration directory, holding the token its inbox expects.
//!
//! The wire format is Claude Code's own peer messaging: one auth line, one
//! user-message line, done. It is undocumented, so a mismatch after an upgrade
//! costs only the notice, never the switch. A session that bypasses permission
//! prompts holds a notice from a process outside its own tree for review, so
//! `ccs use` is best run from the session that wants to hear about it.

use std::fs;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::fsx::write_atomic;

const SOCKET_ENV: &str = "CLAUDE_CODE_MESSAGING_SOCKET";
const FILE: &str = "notify.json";
const TIMEOUT: Duration = Duration::from_secs(1);

/// Add or drop the calling session from the list a switch reports to.
pub fn subscribe(root: &Path, on: bool) -> Result<()> {
    let sock = std::env::var(SOCKET_ENV).ok().filter(|s| !s.is_empty()).with_context(|| {
        format!("{SOCKET_ENV} is not set; run this from inside a Claude Code session")
    })?;
    if on && !Path::new(&sock).exists() {
        bail!("{sock} does not exist; is this session's inbox up?");
    }
    let mut subs = subscribers(root);
    subs.retain(|s| s != &sock);
    if on {
        subs.push(sock.clone());
    }
    save(root, &subs)?;
    println!("{} {sock}", if on { "will notify" } else { "will no longer notify" });
    Ok(())
}

/// Push `text` to every subscriber still reachable. Returns how many took it;
/// a subscriber whose inbox is gone is forgotten.
pub fn broadcast(config: &Path, root: &Path, text: &str) -> usize {
    let subs = subscribers(root);
    if subs.is_empty() {
        return 0;
    }
    let sessions = config.join("sessions");
    let body = format!("<claude-peer-message from-name=\"ccs\">\n{text}\n</claude-peer-message>");
    let (alive, dead): (Vec<_>, Vec<_>) =
        subs.into_iter().partition(|sock| send(&sessions, sock, &body).is_some());
    if !dead.is_empty() {
        let _ = save(root, &alive);
    }
    alive.len()
}

fn subscribers(root: &Path) -> Vec<String> {
    fs::read(root.join(FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save(root: &Path, subs: &[String]) -> Result<()> {
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

fn send(sessions: &Path, sock: &str, body: &str) -> Option<()> {
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
