//! The gateway: an Anthropic Messages endpoint on loopback that sends every
//! request out as the account in use.
//!
//! A client that speaks the Anthropic API — pi, through its `models.json` —
//! points its base URL here and presents the gateway key. pi takes any key
//! spelled `sk-ant-oat…` for an OAuth token and does the Claude Code shaping
//! itself: the bearer header, the OAuth betas, the identity line at the head
//! of the system prompt. So nothing here reads a body. The client's key is
//! swapped for the account's access token and the rest is relayed as it came,
//! both ways, with the response streamed as it arrives.
//!
//! This module is the wire: parsing what a client sends, deciding what to
//! forward, and writing what comes back. Which account answers is `cmd`'s
//! business, asked over a channel from the thread that holds the connection.

use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::fsx::write_atomic;

/// The key file is a credential in its own right: owner-only.
const KEY_MODE: u32 = 0o600;
const KEY_FILE: &str = "gateway.key";

/// What pi keys on to treat a key as an OAuth token, plus a mark of its own.
const KEY_PREFIX: &str = "sk-ant-oat-ccs-";

/// Bodies are held whole so a limited request can be tried again on another
/// account. Anything past this is not a conversation.
const MAX_BODY: usize = 64 * 1024 * 1024;

/// Headers that describe the connection this end, not the request: they are
/// remade for the connection upstream rather than forwarded. The client's
/// credentials go with them, since the account's replace them, and its accepted
/// encodings, since the reply is passed on as bytes and has to arrive as such.
const NOT_FORWARDED: [&str; 10] = [
    "host",
    "authorization",
    "x-api-key",
    "content-length",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "accept-encoding",
    "proxy-connection",
    "upgrade",
];

/// One request as the client sent it, body and all.
#[derive(Debug)]
pub struct Request {
    pub method: String,
    /// Path and query, as spelled on the request line.
    pub target: String,
    /// Names lower-cased; order kept.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers.iter().find(|(n, _)| *n == name).map(|(_, v)| v.as_str())
    }

    /// Whether the client presented `key`, either as the bearer token pi sends
    /// for an OAuth key or as the API key a plainer client would.
    pub fn presents(&self, key: &str) -> bool {
        let bearer = self
            .header("authorization")
            .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
            .map(str::trim);
        bearer == Some(key) || self.header("x-api-key").map(str::trim) == Some(key)
    }

    /// The headers to send upstream in this request's name.
    pub fn forwarded(&self) -> Vec<(String, String)> {
        self.headers.iter().filter(|(n, _)| !NOT_FORWARDED.contains(&n.as_str())).cloned().collect()
    }
}

/// Read one request off a connection. `None` when the client has hung up
/// without sending one, which is how every keep-alive connection ends.
pub fn read_request(reader: &mut impl BufRead) -> Result<Option<Request>> {
    let mut line = String::new();
    if reader.read_line(&mut line).context("reading the request line")? == 0 {
        return Ok(None);
    }
    let mut words = line.split_whitespace();
    let (Some(method), Some(target)) = (words.next(), words.next()) else {
        bail!("malformed request line {:?}", line.trim_end());
    };
    let (method, target) = (method.to_string(), target.to_string());

    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).context("reading a header")? == 0 {
            bail!("the request ended inside its headers");
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            bail!("malformed header {line:?}");
        };
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }

    let request = Request { method, target, headers, body: Vec::new() };
    let body = match (request.header("transfer-encoding"), request.header("content-length")) {
        (Some(encoding), _) if encoding.eq_ignore_ascii_case("chunked") => read_chunked(reader)?,
        (_, Some(length)) => {
            let length: usize = length.parse().context("reading Content-Length")?;
            if length > MAX_BODY {
                bail!("request body of {length} bytes is past what is relayed");
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).context("reading the request body")?;
            body
        }
        _ => Vec::new(),
    };
    Ok(Some(Request { body, ..request }))
}

fn read_chunked(reader: &mut impl BufRead) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).context("reading a chunk size")?;
        let size = line.trim().split(';').next().unwrap_or_default();
        let size =
            usize::from_str_radix(size, 16).with_context(|| format!("chunk size {size:?}"))?;
        if size == 0 {
            // Trailers, up to the blank line that ends them.
            loop {
                let mut trailer = String::new();
                let read = reader.read_line(&mut trailer).context("reading a trailer")?;
                if read == 0 || trailer.trim_end_matches(['\r', '\n']).is_empty() {
                    break;
                }
            }
            return Ok(body);
        }
        if body.len() + size > MAX_BODY {
            bail!("request body is past what is relayed");
        }
        let start = body.len();
        body.resize(start + size, 0);
        reader.read_exact(&mut body[start..]).context("reading a chunk")?;
        let mut end = [0; 2];
        reader.read_exact(&mut end).context("reading a chunk's end")?;
    }
}

/// Write a response, streaming `body` out in chunks as it yields them, so a
/// server-sent event reaches the client the moment it arrives here.
pub fn write_response(
    out: &mut impl Write,
    status: u16,
    headers: &[(String, String)],
    body: &mut impl Read,
) -> io::Result<()> {
    write!(out, "HTTP/1.1 {status} {}\r\n", reason(status))?;
    for (name, value) in headers {
        write!(out, "{name}: {value}\r\n")?;
    }
    out.write_all(b"transfer-encoding: chunked\r\n\r\n")?;
    let mut buffer = [0; 16 * 1024];
    loop {
        let read = match body.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        write!(out, "{read:x}\r\n")?;
        out.write_all(&buffer[..read])?;
        out.write_all(b"\r\n")?;
        out.flush()?;
    }
    out.write_all(b"0\r\n\r\n")?;
    out.flush()
}

/// Write a whole response at once: a refusal, or an error of this gateway's own.
pub fn write_error(out: &mut impl Write, status: u16, kind: &str, message: &str) -> io::Result<()> {
    let headers = vec![("content-type".to_string(), "application/json".to_string())];
    write_response(out, status, &headers, &mut error_body(kind, message).as_bytes())
}

/// An error in the shape the API itself uses, so a client's own handling of
/// one applies unchanged.
pub fn error_body(kind: &str, message: &str) -> String {
    json!({ "type": "error", "error": { "type": kind, "message": message } }).to_string()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        529 => "Overloaded",
        _ => "",
    }
}

// ── relaying ────────────────────────────────────────────────────────────────

/// An account to send a request as.
#[derive(Debug, Clone, PartialEq)]
pub struct Grant {
    pub slug: String,
    pub email: String,
    /// A live access token.
    pub token: String,
}

/// Where grants come from. The connection thread asks; whoever holds the
/// stash answers.
pub trait Accounts {
    /// An account other than those in `avoid`, which have been found limited
    /// for the request in hand. The error is for the client to read.
    fn grant(&self, avoid: &[String]) -> Result<Grant, String>;

    /// The server refused `grant`'s token. A newer one for the same account,
    /// when one can be had; `None` when it was as fresh as they come.
    fn stale(&self, grant: &Grant) -> Result<Option<Grant>, String>;
}

/// Response headers that describe the hop upstream rather than the answer.
/// Length and framing are remade for the client's connection; the encoding
/// is dropped because the body is handed on decoded.
const NOT_RELAYED: [&str; 5] =
    ["content-length", "transfer-encoding", "content-encoding", "connection", "keep-alive"];

/// How long to wait for a connection, and for the head of a response. A body
/// gets no deadline: a streamed completion runs for as long as it runs.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// The API a request is relayed to.
pub struct Upstream {
    agent: ureq::Agent,
    base: String,
}

/// What came back from upstream, the body still arriving.
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Box<dyn Read + Send>,
}

impl Upstream {
    pub fn new(base: &str) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(None)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_recv_response(Some(RESPONSE_TIMEOUT))
            .http_status_as_error(false)
            // The client's own user agent is forwarded; failing that, none.
            .user_agent("")
            // Bytes are relayed as they come, so the answer has to be plain.
            .accept_encoding("")
            .accept("")
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
            base: base.trim_end_matches('/').into(),
        }
    }

    /// Send `request` as `token`.
    pub fn send(&self, request: &Request, token: &str) -> Result<Reply> {
        let mut builder = ureq::http::Request::builder()
            .method(request.method.as_str())
            .uri(format!("{}{}", self.base, request.target))
            .header("authorization", format!("Bearer {token}"));
        for (name, value) in request.forwarded() {
            builder = builder.header(name, value);
        }
        let outgoing = builder.body(request.body.clone()).context("building the request")?;
        let response = self.agent.run(outgoing).context("reaching the API")?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter(|(name, _)| !NOT_RELAYED.contains(&name.as_str()))
            .filter_map(|(name, value)| Some((name.to_string(), value.to_str().ok()?.to_string())))
            .collect();
        let body = response.into_body().into_with_config().limit(u64::MAX).reader();
        Ok(Reply { status, headers, body: Box::new(body) })
    }
}

/// Answer one request: check the key, send it as the account in use, and
/// relay whatever comes back — unless what comes back is a limit and the pool
/// has another account to try, or a rejected token that can be renewed.
///
/// Every retry happens before a byte reaches the client, which is what makes
/// it invisible.
pub fn answer(
    request: &Request,
    key: &str,
    upstream: &Upstream,
    accounts: &dyn Accounts,
    out: &mut impl Write,
) -> io::Result<Outcome> {
    if !request.presents(key) {
        write_error(out, 401, "authentication_error", "no such gateway key")?;
        return Ok(Outcome::refused(401));
    }
    if !request.target.starts_with("/v1/") {
        write_error(out, 404, "not_found_error", "only the API under /v1/ is relayed")?;
        return Ok(Outcome::refused(404));
    }

    let mut tried: Vec<String> = Vec::new();
    let mut renewed = false;
    let mut grant = match accounts.grant(&tried) {
        Ok(grant) => grant,
        Err(why) => {
            write_error(out, 503, "api_error", &why)?;
            return Ok(Outcome::refused(503));
        }
    };
    loop {
        let mut reply = match upstream.send(request, &grant.token) {
            Ok(reply) => reply,
            Err(e) => {
                let why = format!("the API could not be reached: {e:#}");
                write_error(out, 502, "api_error", &why)?;
                return Ok(Outcome { status: 502, slug: Some(grant.slug), tried });
            }
        };
        match reply.status {
            429 => {
                tried.push(grant.slug.clone());
                if let Ok(next) = accounts.grant(&tried) {
                    grant = next;
                    continue;
                }
            }
            401 if !renewed => {
                renewed = true;
                if let Ok(Some(next)) = accounts.stale(&grant) {
                    grant = next;
                    continue;
                }
            }
            _ => {}
        }
        write_response(out, reply.status, &reply.headers, &mut reply.body)?;
        return Ok(Outcome { status: reply.status, slug: Some(grant.slug), tried });
    }
}

/// How a request was answered, for the log line.
#[derive(Debug, PartialEq)]
pub struct Outcome {
    pub status: u16,
    /// The account it went out as, when it went out at all.
    pub slug: Option<String>,
    /// Accounts found limited along the way.
    pub tried: Vec<String>,
}

impl Outcome {
    fn refused(status: u16) -> Self {
        Self { status, slug: None, tried: Vec::new() }
    }
}

// ── connections ─────────────────────────────────────────────────────────────

const API_BASE: &str = "https://api.anthropic.com";

/// A question for whoever holds the stash, with somewhere to put the answer.
pub enum Ask {
    Grant { avoid: Vec<String>, reply: Sender<Result<Grant, String>> },
    Stale { grant: Grant, reply: Sender<Result<Option<Grant>, String>> },
}

impl Ask {
    pub fn answer(self, accounts: &dyn Accounts) {
        // A connection that gave up waiting is not an error worth anything.
        match self {
            Self::Grant { avoid, reply } => drop(reply.send(accounts.grant(&avoid))),
            Self::Stale { grant, reply } => drop(reply.send(accounts.stale(&grant))),
        }
    }
}

/// A connection thread's way of asking.
#[derive(Clone)]
struct Line(Sender<Ask>);

const GONE: &str = "the stash is no longer answering";

impl Accounts for Line {
    fn grant(&self, avoid: &[String]) -> Result<Grant, String> {
        let (reply, answer) = mpsc::channel();
        self.0.send(Ask::Grant { avoid: avoid.to_vec(), reply }).map_err(|_| GONE.to_string())?;
        answer.recv().map_err(|_| GONE.to_string())?
    }

    fn stale(&self, grant: &Grant) -> Result<Option<Grant>, String> {
        let (reply, answer) = mpsc::channel();
        self.0.send(Ask::Stale { grant: grant.clone(), reply }).map_err(|_| GONE.to_string())?;
        answer.recv().map_err(|_| GONE.to_string())?
    }
}

/// Accept connections for as long as the process runs, each on a thread of
/// its own, asking `asks` which account to send as. Returns at once.
pub fn listen(listener: TcpListener, key: String, asks: Sender<Ask>) {
    thread::spawn(move || {
        let upstream = Arc::new(Upstream::new(API_BASE));
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let (key, line, upstream) = (key.clone(), Line(asks.clone()), Arc::clone(&upstream));
            thread::spawn(move || connection(stream, &key, &upstream, &line));
        }
    });
}

/// Answer requests on one connection until the client hangs up.
fn connection(stream: TcpStream, key: &str, upstream: &Upstream, accounts: &dyn Accounts) {
    let Ok(read_end) = stream.try_clone() else { return };
    let mut reader = io::BufReader::new(read_end);
    let mut writer = io::BufWriter::new(stream);
    loop {
        let request = match read_request(&mut reader) {
            Ok(Some(request)) => request,
            Ok(None) => return,
            Err(e) => {
                let _ = write_error(&mut writer, 400, "invalid_request_error", &format!("{e:#}"));
                return;
            }
        };
        let started = Instant::now();
        match answer(&request, key, upstream, accounts, &mut writer) {
            Ok(outcome) => println!("{}", logged(&request, &outcome, started.elapsed())),
            // The client went away mid-answer; there is nobody to tell.
            Err(_) => return,
        }
        if request.header("connection").is_some_and(|c| c.eq_ignore_ascii_case("close")) {
            return;
        }
    }
}

/// One line per request: what was asked, how it was answered, and as whom.
fn logged(request: &Request, outcome: &Outcome, took: std::time::Duration) -> String {
    let stamp = jiff::Timestamp::now().strftime("%H:%M:%S");
    let path = request.target.split('?').next().unwrap_or_default();
    let mut line = format!("{stamp} {} {path} {}", request.method, outcome.status);
    if let Some(slug) = &outcome.slug {
        line.push_str(&format!(" as {slug}"));
    }
    if !outcome.tried.is_empty() {
        line.push_str(&format!(" ({} limited)", outcome.tried.join(", ")));
    }
    line.push_str(&format!(" {:.1}s", took.as_secs_f64()));
    line
}

/// The gateway key: minted the first time it is asked for, read back after.
pub fn key(root: &Path) -> Result<String> {
    let path = root.join(KEY_FILE);
    match fs::read_to_string(&path) {
        Ok(held) if held.trim().starts_with(KEY_PREFIX) => return Ok(held.trim().to_string()),
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    }
    let minted = generate_key();
    write_atomic(&path, format!("{minted}\n").as_bytes(), KEY_MODE)?;
    Ok(minted)
}

/// A key pi will take for an OAuth token, with enough behind the prefix that
/// nobody guesses it.
pub fn generate_key() -> String {
    let mut random = [0u8; 16];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut random))
        .expect("/dev/urandom is readable");
    let hex: String = random.iter().map(|b| format!("{b:02x}")).collect();
    format!("{KEY_PREFIX}{hex}")
}

/// The `models.json` fragment that points pi here.
pub fn pi_config(port: u16) -> String {
    serde_json::to_string_pretty(&json!({
        "providers": {
            "anthropic": {
                "baseUrl": format!("http://127.0.0.1:{port}"),
                "apiKey": "!ccs serve --key"
            }
        }
    }))
    .expect("a literal serialises")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn request(raw: &str) -> Request {
        read_request(&mut Cursor::new(raw.as_bytes())).expect("reads").expect("a request")
    }

    #[test]
    fn a_request_is_read_with_its_target_headers_and_body() {
        let parsed = request(
            "POST /v1/messages?beta=true HTTP/1.1\r\n\
             Host: 127.0.0.1:4141\r\n\
             Content-Type: application/json\r\n\
             Content-Length: 7\r\n\
             \r\n\
             {\"a\":1}",
        );
        assert_eq!(parsed.method, "POST");
        assert_eq!(parsed.target, "/v1/messages?beta=true");
        assert_eq!(parsed.header("content-type"), Some("application/json"));
        assert_eq!(parsed.body, b"{\"a\":1}");
    }

    #[test]
    fn a_chunked_body_is_read_whole() {
        let parsed = request(
            "POST /v1/messages HTTP/1.1\r\n\
             Transfer-Encoding: chunked\r\n\
             \r\n\
             3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n",
        );
        assert_eq!(parsed.body, b"abcde");
    }

    #[test]
    fn a_closed_connection_is_no_request_rather_than_a_broken_one() {
        let none = read_request(&mut Cursor::new(b"")).expect("reads");
        assert!(none.is_none());
    }

    #[test]
    fn header_lookup_ignores_case() {
        let parsed = request("GET /v1/models HTTP/1.1\r\nX-Api-Key: k\r\n\r\n");
        assert_eq!(parsed.header("x-api-key"), Some("k"));
        assert_eq!(parsed.header("X-API-KEY"), Some("k"));
    }

    #[test]
    fn the_key_is_accepted_as_a_bearer_or_an_api_key_and_nothing_else() {
        let bearer = request("GET / HTTP/1.1\r\nAuthorization: Bearer sk-ant-oat-ccs-abc\r\n\r\n");
        let api_key = request("GET / HTTP/1.1\r\nx-api-key: sk-ant-oat-ccs-abc\r\n\r\n");
        let wrong = request("GET / HTTP/1.1\r\nAuthorization: Bearer sk-ant-oat-ccs-abd\r\n\r\n");
        let missing = request("GET / HTTP/1.1\r\n\r\n");

        assert!(bearer.presents("sk-ant-oat-ccs-abc"));
        assert!(api_key.presents("sk-ant-oat-ccs-abc"));
        assert!(!wrong.presents("sk-ant-oat-ccs-abc"));
        assert!(!missing.presents("sk-ant-oat-ccs-abc"));
    }

    #[test]
    fn forwarding_keeps_the_clients_headers_but_not_its_credentials_or_framing() {
        let parsed = request(
            "POST /v1/messages HTTP/1.1\r\n\
             Host: 127.0.0.1:4141\r\n\
             Authorization: Bearer sk-ant-oat-ccs-abc\r\n\
             x-api-key: whatever\r\n\
             Content-Length: 2\r\n\
             Connection: keep-alive\r\n\
             Accept-Encoding: gzip, br\r\n\
             anthropic-beta: oauth-2025-04-20\r\n\
             User-Agent: claude-cli/2.0.0\r\n\
             \r\n{}",
        );
        let kept = parsed.forwarded();
        let names: Vec<&str> = kept.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["anthropic-beta", "user-agent"]);
    }

    #[test]
    fn a_generated_key_reads_as_an_oauth_token_to_pi_and_is_never_the_same_twice() {
        let one = generate_key();
        let two = generate_key();
        let suffix = one.strip_prefix("sk-ant-oat-ccs-").expect("the prefix pi keys on");
        assert_eq!(suffix.len(), 32);
        assert!(suffix.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(one, two);
    }

    #[test]
    fn the_key_is_minted_once_and_kept_private() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("ccs-serve-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("root");

        let first = key(&root).expect("mints");
        let second = key(&root).expect("reads back");
        assert_eq!(first, second);
        let mode = std::fs::metadata(root.join("gateway.key")).expect("file").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_response_is_written_chunked_so_a_stream_goes_out_as_it_comes_in() {
        let mut out = Vec::new();
        let headers = vec![("content-type".to_string(), "text/event-stream".to_string())];
        write_response(&mut out, 200, &headers, &mut Cursor::new(b"event: ping\n\n"))
            .expect("writes");

        let text = String::from_utf8(out).expect("ascii");
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"), "{text}");
        assert!(text.contains("content-type: text/event-stream\r\n"), "{text}");
        assert!(text.contains("transfer-encoding: chunked\r\n"), "{text}");
        assert!(text.ends_with("\r\n\r\nd\r\nevent: ping\n\n\r\n0\r\n\r\n"), "{text}");
    }

    #[test]
    fn a_refusal_is_shaped_like_the_apis_own_errors() {
        let body = error_body("authentication_error", "no such gateway key");
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(parsed["type"], "error");
        assert_eq!(parsed["error"]["type"], "authentication_error");
        assert_eq!(parsed["error"]["message"], "no such gateway key");
    }

    // ── relaying ────────────────────────────────────────────────────────────

    use std::cell::RefCell;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    const KEY: &str = "sk-ant-oat-ccs-0123";

    /// What the fake upstream saw of each request: the bearer it was sent as,
    /// and the headers that reached it.
    #[derive(Debug, Clone)]
    struct Seen {
        bearer: Option<String>,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    /// An upstream on loopback answering a scripted sequence of responses,
    /// one connection each. `(status, body)` per answer.
    fn upstream(script: Vec<(u16, &'static str)>) -> (Upstream, Arc<Mutex<Vec<Seen>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        std::thread::spawn(move || {
            for (status, body) in script {
                let (stream, _) = listener.accept().expect("accept");
                let mut reader = io::BufReader::new(&stream);
                let request = read_request(&mut reader).expect("reads").expect("a request");
                record.lock().expect("lock").push(Seen {
                    bearer: request
                        .header("authorization")
                        .and_then(|v| v.strip_prefix("Bearer "))
                        .map(String::from),
                    headers: request.headers.clone(),
                    body: request.body.clone(),
                });
                let mut writer = &stream;
                write!(
                    writer,
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .expect("writes");
            }
        });
        (Upstream::new(&base), seen)
    }

    /// Accounts handed out in order, remembering what was asked.
    #[derive(Default)]
    struct Pool {
        grants: Vec<Grant>,
        asked_to_avoid: RefCell<Vec<Vec<String>>>,
        marked_stale: RefCell<Vec<String>>,
        /// What a stale report is answered with, when anything.
        renewed: Option<Grant>,
    }

    fn grant(slug: &str) -> Grant {
        Grant {
            slug: slug.into(),
            email: format!("{slug}@example.com"),
            token: format!("tok-{slug}"),
        }
    }

    impl Accounts for Pool {
        fn grant(&self, avoid: &[String]) -> Result<Grant, String> {
            self.asked_to_avoid.borrow_mut().push(avoid.to_vec());
            self.grants
                .iter()
                .find(|g| !avoid.contains(&g.slug))
                .cloned()
                .ok_or_else(|| "every account is spent".to_string())
        }

        fn stale(&self, grant: &Grant) -> Result<Option<Grant>, String> {
            self.marked_stale.borrow_mut().push(grant.slug.clone());
            Ok(self.renewed.clone())
        }
    }

    fn post(path: &str, key: &str) -> Request {
        request(&format!(
            "POST {path} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {key}\r\n\
             anthropic-beta: oauth-2025-04-20\r\nContent-Length: 9\r\n\r\n{{\"m\":\"x\"}}"
        ))
    }

    /// Status and unchunked body of a response as written to a client.
    fn parse_response(raw: &[u8]) -> (u16, String) {
        let text = String::from_utf8_lossy(raw);
        let (head, body) = text.split_once("\r\n\r\n").expect("a head");
        let status = head.split_whitespace().nth(1).expect("status").parse().expect("number");
        let mut out = String::new();
        let mut rest = body;
        loop {
            let (size, after) = rest.split_once("\r\n").expect("a chunk size");
            let size = usize::from_str_radix(size, 16).expect("hex");
            if size == 0 {
                break;
            }
            out.push_str(&after[..size]);
            rest = &after[size + 2..];
        }
        (status, out)
    }

    fn answered(request: &Request, upstream: &Upstream, pool: &Pool) -> (u16, String) {
        let mut out = Vec::new();
        answer(request, KEY, upstream, pool, &mut out).expect("answers");
        parse_response(&out)
    }

    #[test]
    fn a_request_goes_out_as_the_account_and_comes_back_as_it_was_answered() {
        let (up, seen) = upstream(vec![(200, r#"{"id":"msg"}"#)]);
        let pool = Pool { grants: vec![grant("work")], ..Default::default() };

        let (status, body) = answered(&post("/v1/messages", KEY), &up, &pool);

        assert_eq!((status, body.as_str()), (200, r#"{"id":"msg"}"#));
        let seen = seen.lock().expect("lock");
        assert_eq!(seen[0].bearer.as_deref(), Some("tok-work"));
        assert_eq!(seen[0].body, b"{\"m\":\"x\"}");
        assert!(seen[0].headers.contains(&("anthropic-beta".into(), "oauth-2025-04-20".into())));
        assert!(!seen[0].headers.iter().any(|(n, _)| n == "accept-encoding"));
    }

    #[test]
    fn a_wrong_key_is_refused_before_anything_goes_upstream() {
        let (up, seen) = upstream(vec![]);
        let pool = Pool { grants: vec![grant("work")], ..Default::default() };

        let (status, body) = answered(&post("/v1/messages", "sk-ant-oat-ccs-nope"), &up, &pool);

        assert_eq!(status, 401);
        assert!(body.contains("authentication_error"), "{body}");
        assert!(seen.lock().expect("lock").is_empty());
        assert!(pool.asked_to_avoid.borrow().is_empty());
    }

    #[test]
    fn only_the_api_is_relayed() {
        let (up, _) = upstream(vec![]);
        let pool = Pool { grants: vec![grant("work")], ..Default::default() };
        let (status, _) = answered(&post("/admin", KEY), &up, &pool);
        assert_eq!(status, 404);
    }

    #[test]
    fn a_limited_account_is_passed_over_for_the_next_in_the_pool() {
        let (up, seen) = upstream(vec![(429, r#"{"rate":"limited"}"#), (200, r#"{"id":"ok"}"#)]);
        let pool = Pool { grants: vec![grant("work"), grant("alt")], ..Default::default() };

        let (status, body) = answered(&post("/v1/messages", KEY), &up, &pool);

        assert_eq!((status, body.as_str()), (200, r#"{"id":"ok"}"#));
        let seen = seen.lock().expect("lock");
        assert_eq!(seen[0].bearer.as_deref(), Some("tok-work"));
        assert_eq!(seen[1].bearer.as_deref(), Some("tok-alt"));
        assert_eq!(*pool.asked_to_avoid.borrow(), vec![vec![], vec!["work".to_string()]]);
    }

    #[test]
    fn a_limit_with_nobody_left_to_try_is_relayed_as_it_is() {
        let (up, _) = upstream(vec![(429, r#"{"rate":"limited"}"#)]);
        let pool = Pool { grants: vec![grant("work")], ..Default::default() };

        let (status, body) = answered(&post("/v1/messages", KEY), &up, &pool);

        assert_eq!((status, body.as_str()), (429, r#"{"rate":"limited"}"#));
    }

    #[test]
    fn a_rejected_token_is_renewed_once_and_the_request_tried_again() {
        let (up, seen) = upstream(vec![(401, r#"{"auth":"no"}"#), (200, r#"{"id":"ok"}"#)]);
        let mut renewed = grant("work");
        renewed.token = "tok-work-2".into();
        let pool =
            Pool { grants: vec![grant("work")], renewed: Some(renewed), ..Default::default() };

        let (status, _) = answered(&post("/v1/messages", KEY), &up, &pool);

        assert_eq!(status, 200);
        let seen = seen.lock().expect("lock");
        assert_eq!(seen[1].bearer.as_deref(), Some("tok-work-2"));
        assert_eq!(*pool.marked_stale.borrow(), vec!["work".to_string()]);
    }

    #[test]
    fn a_rejected_token_that_cannot_be_renewed_is_relayed_as_a_rejection() {
        let (up, _) = upstream(vec![(401, r#"{"auth":"no"}"#)]);
        let pool = Pool { grants: vec![grant("work")], ..Default::default() };

        let (status, body) = answered(&post("/v1/messages", KEY), &up, &pool);

        assert_eq!((status, body.as_str()), (401, r#"{"auth":"no"}"#));
    }

    #[test]
    fn no_account_to_send_as_is_said_in_the_apis_terms() {
        let (up, _) = upstream(vec![]);
        let pool = Pool::default();

        let (status, body) = answered(&post("/v1/messages", KEY), &up, &pool);

        assert_eq!(status, 503);
        assert!(body.contains("every account is spent"), "{body}");
    }

    #[test]
    fn an_unreachable_upstream_is_a_bad_gateway_not_a_hang() {
        let up = Upstream::new("http://127.0.0.1:1");
        let pool = Pool { grants: vec![grant("work")], ..Default::default() };

        let (status, body) = answered(&post("/v1/messages", KEY), &up, &pool);

        assert_eq!(status, 502);
        assert!(body.contains("api_error"), "{body}");
    }

    #[test]
    fn the_snippet_points_pi_at_this_port_and_at_the_key_command() {
        let snippet = pi_config(4141);
        let parsed: serde_json::Value = serde_json::from_str(&snippet).expect("json");
        assert_eq!(parsed["providers"]["anthropic"]["baseUrl"], "http://127.0.0.1:4141");
        assert_eq!(parsed["providers"]["anthropic"]["apiKey"], "!ccs serve --key");
    }
}
