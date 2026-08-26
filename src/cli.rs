//! Argument parsing.

use anyhow::{Result, bail};

pub const HELP: &str = "\
ccs - Claude Code account switcher

USAGE
    ccs                      pick an account interactively
    ccs ls                   every stashed account and what it has left
    ccs use <account>        switch to an account; running sessions follow
    ccs add [--name <slug>]  stash the account that is logged in right now
    ccs rm <account>         forget a stashed account
    ccs status               limits for the account currently in use

    <account> is a slug, an email, an unambiguous prefix of either, or the
    index shown by `ccs ls`.

OPTIONS
    -f, --force              switch even to an account with no headroom left
        --json               machine-readable output (ls, status)
    -h, --help               this text
    -V, --version            version
";

#[derive(Debug, Clone)]
pub enum Cmd {
    Pick,
    List { json: bool },
    Use { target: String, force: bool },
    Add { name: Option<String> },
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
        "add" | "capture" => Ok(Cmd::Add { name: value(&args[1..], "--name") }),
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

/// The first argument that is not a flag or a flag's value.
fn positional(args: &[String]) -> Option<String> {
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--name" {
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
    fn add_reads_the_name_flag_without_treating_it_as_a_target() {
        let Cmd::Add { name } = parsed(&["add", "--name", "work"]) else { panic!("not an add") };
        assert_eq!(name.as_deref(), Some("work"));
        assert!(matches!(parsed(&["add"]), Cmd::Add { name: None }));
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
