//! Logging in to an account without disturbing the one in use.
//!
//! Claude Code keeps its whole state under `CLAUDE_CONFIG_DIR`, so pointing a
//! child `claude auth login` at a throwaway directory mints credentials for
//! another account while the live ones sit untouched. Whatever lands there is
//! taken for the stash and the directory is destroyed.

use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::creds::{self, Backend};
use crate::model::{Oauth, Provider};

/// Prefix marking a throwaway login directory, so one left behind by an
/// interrupted run can be recognised later and swept.
const SCRATCH: &str = ".login-";

const SCRATCH_MODE: u32 = 0o700;

/// How the login page should be reached.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub email: Option<String>,
    pub console: bool,
    pub sso: bool,
}

impl Options {
    pub fn validate(&self, provider: Provider) -> Result<()> {
        let claude_options = self.email.is_some() || self.console || self.sso;
        let incompatible = provider == Provider::Codex && claude_options;
        if incompatible {
            bail!(
                "--email, --console and --sso steer Claude's login page; choose Claude Code or omit them for Codex"
            );
        }
        Ok(())
    }
}

/// A throwaway config directory, destroyed on drop.
struct Scratch {
    path: PathBuf,
    /// The backend the login wrote through, so the credentials it minted are
    /// destroyed along with the directory wherever they landed.
    backend: Backend,
}

impl Scratch {
    fn new(backend: Backend, root: &Path) -> Result<Self> {
        let path = root.join(format!("{SCRATCH}{}", std::process::id()));
        fs::create_dir_all(&path).with_context(|| format!("creating {}", path.display()))?;
        fs::set_permissions(&path, Permissions::from_mode(SCRATCH_MODE))
            .with_context(|| format!("securing {}", path.display()))?;
        Ok(Self { path, backend })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = self.backend.forget(&self.path);
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Run an interactive login in a throwaway directory and hand back what it
/// minted. The terminal belongs to the child for the duration.
pub fn run(backend: Backend, root: &Path, binary: &str, options: &Options) -> Result<Oauth> {
    sweep(backend, root);
    let scratch = Scratch::new(backend, root)?;

    let mut command = Command::new(binary);
    command.args(["auth", "login"]).env("CLAUDE_CONFIG_DIR", &scratch.path);
    // Anything that answers for the credentials file would let the child
    // satisfy itself without logging in, leaving nothing to capture.
    for key in creds::OVERRIDING {
        command.env_remove(key);
    }
    if let Some(email) = &options.email {
        command.args(["--email", email]);
    }
    if options.console {
        command.arg("--console");
    }
    if options.sso {
        command.arg("--sso");
    }

    let status = command.status().with_context(|| {
        format!("running `{binary} auth login`; set CCS_CLAUDE_BINARY if it is not on PATH")
    })?;
    if !status.success() {
        bail!("`{binary} auth login` did not complete; nothing was stashed");
    }

    let Some(file) = backend.confined(&scratch.path).read()? else {
        bail!("the login finished but left no credentials behind; nothing was stashed");
    };
    Ok(file.oauth)
}

/// Run an interactive Codex login in a throwaway home and hand back what it
/// minted. Codex keeps everything under `CODEX_HOME`, so the login in use is
/// never touched.
///
/// The login is by device code: a URL and a code to type in, from any
/// browser on any machine. Codex's default flow instead waits on a localhost
/// callback, which a login over SSH or on a headless box never reaches.
pub fn run_codex(backend: Backend, root: &Path, binary: &str) -> Result<Oauth> {
    // The sweep is of every abandoned login here, Claude ones included, and
    // those hold a keychain item on the machines that keep credentials
    // there; only the real backend can take that away.
    sweep(backend, root);
    let scratch = Scratch::new(Backend::File, root)?;

    let status = Command::new(binary)
        .args(["login", "--device-auth"])
        .env("CODEX_HOME", &scratch.path)
        .status()
        .with_context(|| {
            format!(
                "running `{binary} login --device-auth`; set CCS_CODEX_BINARY if it is not on PATH"
            )
        })?;
    if !status.success() {
        bail!("`{binary} login --device-auth` did not complete; nothing was stashed");
    }
    let store = crate::codex::Store::at(&scratch.path, Some(&scratch.path));
    let Some(file) = crate::codex::Creds::read(&store)? else {
        bail!("the login finished but left no credentials behind; nothing was stashed");
    };
    let Some(oauth) = file.oauth() else {
        bail!("the login was with an API key, not a ChatGPT account; nothing was stashed");
    };
    Ok(oauth)
}

/// Remove throwaway directories belonging to runs that are over. They can hold
/// credentials, so they do not get to linger.
fn sweep(backend: Backend, root: &Path) {
    let Ok(entries) = fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if abandoned(name) {
            let _ = backend.forget(&path);
            let _ = fs::remove_dir_all(&path);
        }
    }
}

/// A scratch directory whose owning process is gone has been abandoned. Naming
/// each one after its process is what makes that answerable, and keeps a
/// concurrent run's directory from being swept out from under it.
fn abandoned(name: &str) -> bool {
    let Some(pid) = name.strip_prefix(SCRATCH) else { return false };
    !running(pid)
}

/// Whether a process is still there.
///
/// `/proc` answers this where there is one; where there is not, signal zero is
/// the same question asked of the kernel directly. Anything unanswerable is
/// taken for alive, because sweeping a directory a live login is using would
/// take its credentials out from under it.
fn running(pid: &str) -> bool {
    if Path::new("/proc").is_dir() {
        return Path::new("/proc").join(pid).exists();
    }
    Command::new("/bin/kill")
        .args(["-0", pid])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A private root per test, so concurrently running tests never share a
    /// scratch path.
    fn temp_root(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("ccs-login-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("root");
        path
    }

    #[test]
    fn a_directory_owned_by_this_very_process_is_not_abandoned() {
        assert!(!abandoned(&format!("{SCRATCH}{}", std::process::id())));
    }

    #[test]
    fn a_directory_owned_by_a_departed_process_is_abandoned() {
        // Beyond any pid the kernel will hand out, so nothing can own it.
        assert!(abandoned(&format!("{SCRATCH}999999999")));
    }

    #[test]
    fn unrelated_neighbours_are_left_alone() {
        assert!(!abandoned("accounts"));
        assert!(!abandoned("pens"));
        assert!(!abandoned("usage"));
        assert!(!abandoned("state.json"));
    }

    #[test]
    fn a_login_that_exits_non_zero_stashes_nothing() {
        let root = temp_root("nonzero");
        let error =
            run(Backend::File, &root, "false", &Options::default()).unwrap_err().to_string();
        assert!(error.contains("did not complete"), "{error}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_login_that_mints_nothing_is_reported_rather_than_passing_silently() {
        let root = temp_root("nothing");
        let error = run(Backend::File, &root, "true", &Options::default()).unwrap_err().to_string();
        assert!(error.contains("no credentials"), "{error}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_binary_names_itself_and_the_override() {
        let root = temp_root("missing");
        let error = run(Backend::File, &root, "/nonexistent/claude", &Options::default())
            .unwrap_err()
            .to_string();
        assert!(error.contains("/nonexistent/claude"), "{error}");
        assert!(error.contains("CCS_CLAUDE_BINARY"), "{error}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_codex_login_asks_for_a_device_code() {
        let root = temp_root("device");
        let fake = root.join("codex");
        fs::write(&fake, "#!/bin/sh\necho \"$@\" > \"$CODEX_HOME/../args\"\nexit 1\n")
            .expect("fake");
        fs::set_permissions(&fake, Permissions::from_mode(0o755)).expect("chmod");

        let error =
            run_codex(Backend::File, &root, fake.to_str().expect("utf-8")).unwrap_err().to_string();
        assert!(error.contains("did not complete"), "{error}");
        let args = fs::read_to_string(root.join("args")).expect("args");
        assert_eq!(args.trim(), "login --device-auth");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_failed_login_leaves_no_scratch_directory_behind() {
        let root = temp_root("cleanup");
        let _ = run(Backend::File, &root, "false", &Options::default());
        let leftovers: Vec<_> = fs::read_dir(&root)
            .expect("read root")
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(SCRATCH))
            .collect();
        assert!(leftovers.is_empty(), "a failed login must not leave credentials lying around");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_scratch_directory_is_private_and_removed_on_drop() {
        let root = temp_root("scratch");

        let path = {
            let scratch = Scratch::new(Backend::File, &root).expect("scratch");
            let mode = fs::metadata(&scratch.path).expect("stat").permissions().mode();
            assert_eq!(mode & 0o777, SCRATCH_MODE);
            scratch.path.clone()
        };
        assert!(!path.exists(), "scratch directory should not outlive its guard");

        let _ = fs::remove_dir_all(&root);
    }
}
