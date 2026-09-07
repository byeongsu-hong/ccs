//! Reading and replacing Claude Code's live credentials.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

use crate::fsx::write_atomic;
use crate::model::CredsFile;
use crate::sha256;

/// Credentials are secrets: owner read/write only, matching what Claude Code
/// itself writes.
const MODE: u32 = 0o600;

/// Environment Claude Code reads in preference to the credentials file.
/// Anything set here decides the account whatever the file holds — which makes
/// it something a login has to clear, and something a pinned session has to be
/// warned about.
pub const OVERRIDING: [&str; 5] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
    "CLAUDE_CODE_HOST_CREDS_FILE",
];

/// Where Claude Code's live credentials live. Shareable across threads,
/// which every store is: a path, a name, nothing that moves.
pub trait CredStore: Send + Sync {
    /// The live credentials, or `None` when nobody is logged in.
    fn read(&self) -> Result<Option<CredsFile>>;
    /// Replace the live credentials atomically.
    fn write(&self, creds: &CredsFile) -> Result<()>;
    /// The configuration directory these credentials answer for. The write
    /// lock is taken here, which is where Claude Code takes it too.
    fn dir(&self) -> &Path;
    /// Where this store keeps them, for saying so in errors.
    fn describe(&self) -> String;
}

// ── choosing a backend ──────────────────────────────────────────────────────

/// Which of the two places Claude Code keeps credentials this build talks to.
///
/// Claude Code stores them in the login keychain on macOS and in a plain file
/// everywhere else, so a switch that wrote the file on a Mac would be read by
/// nobody. `Keychain` covers both: the keychain is authoritative, and the file
/// is the same fallback Claude Code itself falls back to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Backend {
    File,
    Keychain,
}

impl Backend {
    /// The backend the Claude Code on this machine is using.
    pub fn detect() -> Self {
        match cfg!(target_os = "macos") {
            true => Self::Keychain,
            false => Self::File,
        }
    }

    /// The store for the credentials in use: those of the configuration
    /// directory this process itself acts on.
    pub fn live(self, config_dir: &Path) -> Box<dyn CredStore> {
        // A session reading these has the environment this process was given,
        // so the namespace is whatever that environment spells. Empty is not
        // set, which is how Claude Code reads it too.
        let scoped = std::env::var_os("CLAUDE_CONFIG_DIR").is_some_and(|dir| !dir.is_empty());
        self.store(config_dir, scoped)
    }

    /// The store for a configuration directory a session will be pointed at:
    /// a pen, or the throwaway directory a login is run in. Such a session
    /// always has `CLAUDE_CONFIG_DIR` set, which is what puts its credentials
    /// in a namespace of their own.
    pub fn confined(self, dir: &Path) -> Box<dyn CredStore> {
        self.store(dir, true)
    }

    fn store(self, dir: &Path, scoped: bool) -> Box<dyn CredStore> {
        match self {
            Self::File => Box::new(FileStore::new(dir)),
            Self::Keychain => Box::new(KeychainStore::new(dir, scoped)),
        }
    }

    /// Take away everything a confined directory holds beyond the directory
    /// itself. A pen or a login directory can be removed with the filesystem;
    /// the keychain item it was given cannot, and outlives it silently.
    pub fn forget(self, dir: &Path) -> Result<()> {
        match self {
            Self::File => Ok(()),
            Self::Keychain => KeychainStore::new(dir, true).forget(),
        }
    }
}

// ── the plain file ──────────────────────────────────────────────────────────

pub struct FileStore {
    dir: PathBuf,
    path: PathBuf,
}

impl FileStore {
    pub fn new(config_dir: &Path) -> Self {
        Self { dir: config_dir.to_path_buf(), path: config_dir.join(".credentials.json") }
    }

    fn exists(&self) -> bool {
        self.path.exists()
    }
}

impl CredStore for FileStore {
    fn read(&self) -> Result<Option<CredsFile>> {
        let raw = match fs::read(&self.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading {}", self.path.display())),
        };
        serde_json::from_slice(&raw)
            .with_context(|| format!("parsing {}", self.path.display()))
            .map(Some)
    }

    fn write(&self, creds: &CredsFile) -> Result<()> {
        let body = serde_json::to_vec_pretty(creds).context("serialising credentials")?;
        write_atomic(&self.path, &body, MODE)
    }

    fn dir(&self) -> &Path {
        &self.dir
    }

    fn describe(&self) -> String {
        self.path.display().to_string()
    }
}

// ── the login keychain ──────────────────────────────────────────────────────

/// `security(1)`'s exit codes for an item that is not there.
const NOT_FOUND: [i32; 2] = [44, 36];

/// Claude Code's own name for the item, before the namespace suffix.
const ITEM: &str = "Claude Code-credentials";

/// The account name Claude Code files the item under when the login name is
/// not one a keychain attribute can carry.
const ANONYMOUS: &str = "claude-code-user";

/// The live credentials as Claude Code keeps them on macOS: an item in the
/// login keychain, with the plain file as the fallback it drops to when the
/// keychain holds nothing, or refuses to be written.
///
/// Reads prefer the keychain and writes go there first, which is the order
/// Claude Code resolves them in — anything else would leave a switch written
/// somewhere nothing reads.
pub struct KeychainStore {
    dir: PathBuf,
    service: String,
    account: String,
    /// Written only when it is already there, or when the keychain refuses the
    /// write. A Mac that keeps its credentials in the keychain never grows a
    /// plaintext copy because of a switch.
    fallback: FileStore,
}

impl KeychainStore {
    pub fn new(config_dir: &Path, scoped: bool) -> Self {
        Self {
            dir: config_dir.to_path_buf(),
            service: service(config_dir, scoped),
            account: account(),
            fallback: FileStore::new(&storage_dir(config_dir)),
        }
    }

    fn security(&self, args: &[&str]) -> Result<std::process::Output> {
        Command::new("/usr/bin/security").args(args).output().context("running /usr/bin/security")
    }

    /// The item's contents, or `None` when there is no such item.
    fn item(&self) -> Result<Option<String>> {
        let out = self.security(&[
            "find-generic-password",
            "-a",
            &self.account,
            "-s",
            &self.service,
            "-w",
        ])?;
        if !out.status.success() {
            return match NOT_FOUND.contains(&out.status.code().unwrap_or(-1)) {
                true => Ok(None),
                false => {
                    Err(anyhow!("reading the keychain item {:?}: {}", self.service, stderr(&out)))
                }
            };
        }
        // An empty answer is an absent item rather than empty credentials,
        // which is the other way `security` says there is nothing there.
        let said = String::from_utf8_lossy(&out.stdout);
        match said.trim() {
            "" => Ok(None),
            said => Ok(Some(decoded(said)?)),
        }
    }

    /// Take the item away. Missing is the outcome asked for, not a failure.
    fn forget(&self) -> Result<()> {
        let out =
            self.security(&["delete-generic-password", "-a", &self.account, "-s", &self.service])?;
        if !out.status.success() && !NOT_FOUND.contains(&out.status.code().unwrap_or(-1)) {
            bail!("removing the keychain item {:?}: {}", self.service, stderr(&out));
        }
        Ok(())
    }
}

impl CredStore for KeychainStore {
    fn read(&self) -> Result<Option<CredsFile>> {
        let Some(body) = self.item()? else { return self.fallback.read() };
        serde_json::from_str(&body)
            .with_context(|| format!("parsing the keychain item {:?}", self.service))
            .map(Some)
    }

    fn write(&self, creds: &CredsFile) -> Result<()> {
        // Single-line and compact: the payload travels as one argument.
        let body = serde_json::to_vec(creds).context("serialising credentials")?;
        // Whether there is an item here already decides whether this is
        // creating one. An item that cannot be read is treated as one that is
        // there: the alternative is widening the access on something that
        // exists, on the strength of a failed read.
        let held = !matches!(self.item(), Ok(None));

        // Hexadecimal because `security` hands data back that way whenever it
        // is not plain ASCII, so writing it the same way keeps one spelling of
        // the payload rather than two.
        //
        // On the command line because there is nowhere else to put it:
        // `security` reads a password from a prompt otherwise, and truncates
        // that at 128 bytes — well under what credentials come to. It is
        // visible to `ps` for the length of the call, which is the same
        // exposure Claude Code takes writing its own.
        let payload = hex(&body);
        let mut args = vec![
            "add-generic-password",
            "-U",
            "-a",
            &self.account,
            "-s",
            &self.service,
            "-X",
            &payload,
        ];
        // An item this tool creates is one no application has been trusted
        // with yet, so a Claude Code reading it would be met with a keychain
        // prompt. Opening it to the applications this user runs is what the
        // plain file already grants them, and what keeps a pinned session from
        // stopping on a dialog. An item Claude Code made keeps the access it
        // came with: updating in place leaves that alone.
        if !held {
            args.push("-A");
        }

        let out = self.security(&args)?;
        if !out.status.success() {
            let refused = stderr(&out);
            // Said out loud: this leaves credentials in a plain file on a
            // machine whose owner has every reason to think they are in the
            // keychain. Claude Code says as much when it takes the same step.
            eprintln!("ccs: the keychain refused the write ({refused}); storing them in a file");
            return self
                .fallback
                .write(creds)
                .with_context(|| format!("the keychain refused the write ({refused})"));
        }
        // Claude Code watches the file's mtime for a switch to follow when it
        // has one to watch, and the keychain otherwise. Keeping a file that is
        // already there in step is what makes both answers the same one.
        if self.fallback.exists() {
            self.fallback.write(creds)?;
        }
        Ok(())
    }

    fn dir(&self) -> &Path {
        &self.dir
    }

    fn describe(&self) -> String {
        format!("the login keychain, under {:?}", self.service)
    }
}

/// The keychain item's name for a configuration directory.
///
/// Claude Code namespaces the item by the directory whenever `CLAUDE_CONFIG_DIR`
/// decides where that is, so every pen has an item of its own and the default
/// configuration keeps the unsuffixed name. Spelling it the same way is the
/// whole of the interoperation: an item under any other name is one nothing
/// reads.
fn service(config_dir: &Path, scoped: bool) -> String {
    // Claude Code lets this name the directory the item is keyed by on its
    // own, and an empty value asks for the default namespace outright.
    let override_dir = std::env::var_os("CLAUDE_SECURESTORAGE_CONFIG_DIR");
    let (namespaced, keyed_by) = match &override_dir {
        Some(dir) if dir.is_empty() => (false, PathBuf::new()),
        Some(dir) => (true, PathBuf::from(dir)),
        None => (scoped, config_dir.to_path_buf()),
    };
    if !namespaced {
        return ITEM.to_string();
    }
    // Claude Code hashes the path as it is spelled, normalised to NFC. The
    // paths this is handed are the ones handed to Claude Code in the same
    // breath, so they are spelled alike — and identical for every path made of
    // ASCII, which a pen's is. A configuration path carrying composed
    // characters in another normal form would need normalising first.
    let digest = sha256::hex(keyed_by.as_os_str().as_encoded_bytes());
    format!("{ITEM}-{}", &digest[..8])
}

/// Where the plain file belonging to a configuration directory sits.
///
/// Normally beside the configuration itself. `CLAUDE_SECURESTORAGE_CONFIG_DIR`
/// moves the credentials out on their own — the item's name with them, which
/// `service` answers for — and empty asks for the default configuration rather
/// than for the directory in hand.
fn storage_dir(config_dir: &Path) -> PathBuf {
    match std::env::var_os("CLAUDE_SECURESTORAGE_CONFIG_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        Some(_) => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(".claude"),
            None => config_dir.to_path_buf(),
        },
        None => config_dir.to_path_buf(),
    }
}

/// The account attribute Claude Code files the item under: the login name,
/// or a fixed stand-in when it is not one the attribute can carry.
fn account() -> String {
    let name =
        std::env::var("USER").ok().filter(|name| !name.is_empty()).unwrap_or_else(login_name);
    match plain(&name) {
        true => name,
        false => ANONYMOUS.to_string(),
    }
}

/// The login name of whoever is running this, for the environment that does
/// not spell it out. Claude Code reads the password database here; asking
/// `id` is the same question without a dependency to ask it with.
fn login_name() -> String {
    let Ok(out) = Command::new("/usr/bin/id").arg("-un").output() else { return String::new() };
    match out.status.success() {
        true => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        false => String::new(),
    }
}

fn plain(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// What `security` says when it will not do something.
fn stderr(out: &std::process::Output) -> String {
    let said = String::from_utf8_lossy(&out.stderr);
    match said.trim() {
        "" => format!("exit status {}", out.status),
        said => said.replace('\n', "; "),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What `security` handed back, which is the payload itself when it is plain
/// ASCII and its hexadecimal spelling when it is not.
fn decoded(said: &str) -> Result<String> {
    let hexadecimal = !said.is_empty()
        && said.len().is_multiple_of(2)
        && said.bytes().all(|b| b.is_ascii_hexdigit())
        && !said.starts_with('{');
    if !hexadecimal {
        return Ok(said.to_string());
    }
    let bytes: Vec<u8> = (0..said.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&said[i..i + 2], 16).expect("two hexadecimal digits"))
        .collect();
    String::from_utf8(bytes).context("the keychain item is not text")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default configuration keeps the name Claude Code gives it, and
    /// anything confined to a directory of its own is keyed by that directory.
    #[test]
    fn names_the_item_the_way_claude_code_does() {
        let default = service(Path::new("/home/you/.claude"), false);
        let pen = service(Path::new("/home/you/.claude/ccs/pens/work"), true);

        assert_eq!(default, "Claude Code-credentials");
        assert_eq!(pen, "Claude Code-credentials-f71cf559");
    }

    /// Two pens are two items: a switch inside one is not a switch in the
    /// other, which is the whole of what pinning promises.
    #[test]
    fn gives_each_confined_directory_its_own_item() {
        let work = service(Path::new("/home/you/.claude/ccs/pens/work"), true);
        let alt = service(Path::new("/home/you/.claude/ccs/pens/alt"), true);

        assert_ne!(work, alt);
    }

    /// A login name a keychain attribute cannot carry is not one to key an
    /// item by, and Claude Code falls back to a fixed name rather than to a
    /// mangled one.
    #[test]
    fn keeps_only_a_login_name_the_attribute_can_carry() {
        assert!(plain("eddy"));
        assert!(plain("first.last_1-2"));
        assert!(!plain(""));
        assert!(!plain("ад"));
        assert!(!plain("with space"));
    }

    #[test]
    fn reads_both_spellings_security_hands_back() {
        let plain = r#"{"claudeAiOauth":{"accessToken":"a"}}"#;

        assert_eq!(decoded(plain).expect("plain"), plain);
        assert_eq!(decoded("7b2261223a22c3bc227d").expect("hexadecimal"), "{\"a\":\"ü\"}");
    }

    /// A payload of hexadecimal digits that is JSON all the same is JSON: the
    /// leading brace is what says so.
    #[test]
    fn does_not_mistake_json_for_hexadecimal() {
        assert_eq!(decoded("{}").expect("braces"), "{}");
    }

    /// The one thing unit tests cannot answer: whether `security` stores and
    /// hands back what this thinks it does. Kept out of the default run
    /// because it writes to whoever's login keychain is running it — under a
    /// name of its own, and taken away again on the way out.
    ///
    /// `cargo test -- --ignored` asks for it.
    #[test]
    #[ignore = "writes to the login keychain"]
    #[cfg(target_os = "macos")]
    fn a_keychain_item_round_trips_through_security() {
        use crate::model::Oauth;

        let dir = std::env::temp_dir().join(format!("ccs-keychain-{}", std::process::id()));
        let store = KeychainStore::new(&dir, true);
        let _ = store.forget();

        assert!(store.read().expect("read an item that is not there").is_none());

        let mut creds = CredsFile::new(Oauth {
            access_token: "sk-ant-oat01-test".to_string(),
            refresh_token: "sk-ant-ort01-test".to_string(),
            expires_at: 42,
            scopes: vec!["user:inference".to_string()],
            subscription_type: Some("max".to_string()),
            extra: serde_json::Map::new(),
        });
        // The credentials Claude Code keeps carry more than this tool knows
        // about; a switch that dropped them would take the MCP logins with it.
        creds.extra.insert("mcpOAuth".to_string(), serde_json::json!({ "server|1": "token" }));

        store.write(&creds).expect("write");
        let back = store.read().expect("read").expect("the item that was just written");

        assert_eq!(back.oauth.refresh_token, "sk-ant-ort01-test");
        assert_eq!(back.extra["mcpOAuth"]["server|1"], "token");
        // Writing again is an update rather than a second item.
        store.write(&creds).expect("write again");
        assert!(store.read().expect("read again").is_some());

        store.forget().expect("forget");
        assert!(store.read().expect("read after forgetting").is_none());
        // Nothing was left on disk: the keychain answered, so the file was
        // never reached for.
        assert!(!dir.join(".credentials.json").exists());
    }

    #[test]
    fn round_trips_a_payload_through_hexadecimal() {
        let body = r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-üñî"}}"#;

        assert_eq!(decoded(&hex(body.as_bytes())).expect("round trip"), body);
    }
}
