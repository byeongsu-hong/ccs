//! Tell running Claude Code sessions about a switch, through the inbox each
//! one listens on.
//!
//! Every session publishes `sessions/<pid>.json` (with its socket path) and
//! `sessions/<pid>.<hash>.key` (with the token the inbox expects) under the
//! configuration directory it runs against. Pinned sessions do so inside their
//! pen, so a switch from outside never reaches them — which is right, since
//! the switch never reached their credentials either.
//!
//! The wire format is Claude Code's own peer messaging: one auth line, one
//! user-message line, done. It is undocumented, so a mismatch after an upgrade
//! costs only the notice, never the switch.

use std::fs;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(1);

/// Push `text` to every reachable session registered under `config`. Returns
/// how many took it; a session that is gone or refuses is skipped in silence.
pub fn broadcast(config: &Path, text: &str) -> usize {
    let dir = config.join("sessions");
    let Ok(entries) = fs::read_dir(&dir) else { return 0 };
    let body = format!("<claude-peer-message from-name=\"ccs\">\n{text}\n</claude-peer-message>");
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let pid = name.strip_suffix(".json")?;
            if !pid.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let reg: Value = serde_json::from_slice(&fs::read(e.path()).ok()?).ok()?;
            let sock = reg.get("messagingSocketPath")?.as_str()?.to_owned();
            Some((pid.to_owned(), sock))
        })
        .filter(|(pid, sock)| send(&dir, pid, sock, &body).is_some())
        .count()
}

fn token(dir: &Path, pid: &str) -> Option<String> {
    let prefix = format!("{pid}.");
    fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let name = e.file_name().into_string().ok()?;
        if !name.starts_with(&prefix) || !name.ends_with(".key") {
            return None;
        }
        let key: Value = serde_json::from_slice(&fs::read(e.path()).ok()?).ok()?;
        Some(key.get("peerToken")?.as_str()?.to_owned())
    })
}

fn send(dir: &Path, pid: &str, sock: &str, body: &str) -> Option<()> {
    let token = token(dir, pid)?;
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
    s.write_all(lines.as_bytes()).ok()
}
