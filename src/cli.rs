//! Argument parsing.

use anyhow::{Result, bail};

pub const HELP: &str = "\
ccs - Claude Code account switcher

USAGE
    ccs                      pick an account interactively
    ccs ls                   every stashed account and what it has left
    ccs use <account>        switch to an account; running sessions follow
    ccs add                  log in to another account and stash it, without
                             disturbing the account in use
    ccs add --current        stash the account that is logged in right now
    ccs rm <account>         forget a stashed account
    ccs status               limits for the account currently in use

    <account> is a slug, an email, an unambiguous prefix of either, or the
    index shown by `ccs ls`.

OPTIONS
    -f, --force              switch even to an account with no headroom left
        --json               machine-readable output (ls, status)
        --name <slug>        stash under this name instead of the email (add)
        --email <address>    pre-fill the login page (add)
        --console            log in with Console billing, not a subscription (add)
        --sso                force the SSO login flow (add)
    -h, --help               this text
    -V, --version            version
";

#[derive(Debug, Clone)]
pub enum Cmd {
    Pick,
    List { json: bool },
    Use { target: String, force: bool },
    Add { name: Option<String>, current: bool, email: Option<String>, console: bool, sso: bool },
    Remove { target: String },
    Status { json: bool },
    Help,
    Version,
}

pub fn parse<I: Iterator<Item = String>>(args: I) -> Result<Cmd> {
    let args: Vec<String> = args.collect();
    let Some(head) = args.first() else { return Ok(Cmd::Pick) };

    match head.as_str() {
        "-h" | "--help" | "help" => Ok(Cmd::Help),
        "-V" | "--version" | "version" => Ok(Cmd::Version),
        "ls" | "list" => Ok(Cmd::List { json: has(&args[1..], "--json") }),
        "status" | "st" => Ok(Cmd::Status { json: has(&args[1..], "--json") }),
        "use" | "switch" => {
            let rest = &args[1..];
            let Some(target) = positional(rest) else {
                bail!("`ccs use` needs an account; `ccs ls` lists them");
            };
            Ok(Cmd::Use { target, force: has(rest, "--force") || has(rest, "-f") })
        }
        "add" | "capture" => {
            let rest = &args[1..];
            Ok(Cmd::Add {
                name: value(rest, "--name"),
                current: has(rest, "--current"),
                email: value(rest, "--email"),
                console: has(rest, "--console"),
                sso: has(rest, "--sso"),
            })
        }
        "rm" | "remove" | "forget" => {
            let Some(target) = positional(&args[1..]) else {
                bail!("`ccs rm` needs an account; `ccs ls` lists them");
            };
            Ok(Cmd::Remove { target })
        }
        other => bail!("unknown command {other:?}; `ccs --help` lists them"),
    }
}

fn has(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

/// The value following `flag`, when present.
fn value(args: &[String], flag: &str) -> Option<String> {
    let at = args.iter().position(|a| a == flag)?;
    args.get(at + 1).cloned()
}

/// Flags that consume the argument after them.
const VALUE_FLAGS: [&str; 2] = ["--name", "--email"];

/// The first argument that is neither a flag nor a flag's value.
fn positional(args: &[String]) -> Option<String> {
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if VALUE_FLAGS.contains(&arg.as_str()) {
            skip_next = true;
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return Some(arg.clone());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(words: &[&str]) -> Cmd {
        parse(words.iter().map(|w| w.to_string())).expect("parses")
    }

    #[test]
    fn no_arguments_opens_the_picker() {
        assert!(matches!(parsed(&[]), Cmd::Pick));
    }

    #[test]
    fn list_takes_an_optional_json_flag() {
        assert!(matches!(parsed(&["ls"]), Cmd::List { json: false }));
        assert!(matches!(parsed(&["list", "--json"]), Cmd::List { json: true }));
    }

    #[test]
    fn use_takes_a_target_and_an_optional_force() {
        let Cmd::Use { target, force } = parsed(&["use", "jesse"]) else { panic!("not a use") };
        assert_eq!((target.as_str(), force), ("jesse", false));

        let Cmd::Use { force, .. } = parsed(&["use", "jesse", "-f"]) else { panic!("not a use") };
        assert!(force);
    }

    #[test]
    fn a_flag_before_the_target_does_not_become_the_target() {
        let Cmd::Use { target, force } = parsed(&["use", "--force", "jesse"]) else {
            panic!("not a use")
        };
        assert_eq!((target.as_str(), force), ("jesse", true));
    }

    #[test]
    fn add_logs_in_by_default_and_captures_the_live_account_only_on_request() {
        assert!(matches!(parsed(&["add"]), Cmd::Add { current: false, .. }));
        assert!(matches!(parsed(&["add", "--current"]), Cmd::Add { current: true, .. }));
    }

    #[test]
    fn add_reads_the_name_flag_without_treating_it_as_a_target() {
        let Cmd::Add { name, .. } = parsed(&["add", "--name", "work"]) else {
            panic!("not an add")
        };
        assert_eq!(name.as_deref(), Some("work"));
        assert!(matches!(parsed(&["add"]), Cmd::Add { name: None, .. }));
    }

    #[test]
    fn add_forwards_the_flags_that_steer_the_login_page() {
        let Cmd::Add { email, console, sso, .. } =
            parsed(&["add", "--email", "me@x.com", "--console", "--sso"])
        else {
            panic!("not an add")
        };
        assert_eq!(email.as_deref(), Some("me@x.com"));
        assert!(console && sso);
    }

    #[test]
    fn a_value_carrying_flag_does_not_donate_its_value_as_a_target() {
        let Cmd::Remove { target } = parsed(&["rm", "--name", "notthetarget", "work"]) else {
            panic!("not a remove")
        };
        assert_eq!(target, "work");
    }

    #[test]
    fn remove_needs_a_target() {
        let Cmd::Remove { target } = parsed(&["rm", "work"]) else { panic!("not a remove") };
        assert_eq!(target, "work");
        assert!(parse(["rm".to_string()].into_iter()).is_err());
    }

    #[test]
    fn use_without_a_target_is_an_error_not_a_silent_no_op() {
        assert!(parse(["use".to_string()].into_iter()).is_err());
    }

    #[test]
    fn an_unknown_command_is_refused() {
        assert!(parse(["frobnicate".to_string()].into_iter()).is_err());
    }
}
