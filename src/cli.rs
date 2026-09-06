//! Argument parsing.

use anyhow::{Result, bail};

pub const HELP: &str = "\
ccs - Claude Code account switcher

USAGE
    ccs                      pick an account interactively
    ccs ls                   every stashed account and what it has left
    ccs use <account>        switch to an account; running sessions follow
    ccs pin [<account>]      start a session confined to one account, leaving
                             every other session on the account in use
    ccs add                  log in to another account and stash it, without
                             disturbing the account in use
    ccs add --current        stash the account that is logged in right now
    ccs rm <account>         forget a stashed account
    ccs status               limits for the account currently in use
    ccs notify [<kind>...]   from inside a Claude Code session: have notices
                             delivered into that session's chat. Kinds:
                             switch, session-high, session-reset, weekly-reset;
                             all of them when none is named. `ccs notify off`
                             stops them. A session running with permission
                             prompts bypassed adds --bypass, or it will hold
                             every notice for review.
    ccs watch                poll every account and raise session-high,
                             session-reset and weekly-reset notices; runs
                             until killed. With --rotate, also switch away
                             from a pooled account whose session has run
                             high, to the pooled account whose weekly window
                             resets soonest and still has room
    ccs serve                serve the Anthropic API on 127.0.0.1 as the account
                             in use, for pi and anything else that speaks it;
                             runs until killed. Prints the models.json snippet
                             to paste into pi. With --rotate, a request the
                             account in use is too limited to answer is sent
                             again as the next pooled account
    ccs serve --key          print the key a client presents to the gateway

    <account> is a slug, an email, an unambiguous prefix of either, or the
    index shown by `ccs ls`. Without one, `ccs pin` asks.

    Anything after `--` is passed on to Claude Code: `ccs pin -- --continue`.

OPTIONS
    -f, --force              switch even to an account with no headroom left
        --json               machine-readable output (ls, status)
        --cached             what the last poll wrote down, without polling (ls);
                             `ccs watch` is what keeps that current
        --name <slug>        stash under this name instead of the email (add)
        --email <address>    pre-fill the login page (add)
        --console            log in with Console billing, not a subscription (add)
        --sso                force the SSO login flow (add)
        --every <seconds>    poll interval (watch; default 300)
        --high <percent>     session percentage that counts as high (watch; default 90)
        --rotate <a>,<b>,... accounts to rotate between (watch) or fall over
                             to (serve); repeatable
        --port <n>           port to serve on (serve; default 4141)
    -h, --help               this text
    -V, --version            version
";

#[derive(Debug, Clone)]
pub enum Cmd {
    Pick,
    List { json: bool, cached: bool },
    Use { target: String, force: bool },
    Add { name: Option<String>, current: bool, email: Option<String>, console: bool, sso: bool },
    Pin { target: Option<String>, args: Vec<String> },
    Remove { target: String },
    Status { json: bool },
    Notify { off: bool, kinds: Vec<String>, bypass: bool },
    Watch { every: u64, high: f64, rotate: Vec<String> },
    Serve { port: u16, rotate: Vec<String> },
    ServeKey,
    Help,
    Version,
}

pub fn parse<I: Iterator<Item = String>>(args: I) -> Result<Cmd> {
    let args: Vec<String> = args.collect();
    let Some(head) = args.first() else { return Ok(Cmd::Pick) };

    match head.as_str() {
        "-h" | "--help" | "help" => Ok(Cmd::Help),
        "-V" | "--version" | "version" => Ok(Cmd::Version),
        "ls" | "list" => {
            Ok(Cmd::List { json: has(&args[1..], "--json"), cached: has(&args[1..], "--cached") })
        }
        "status" | "st" => Ok(Cmd::Status { json: has(&args[1..], "--json") }),
        "notify" => {
            let rest = &args[1..];
            Ok(Cmd::Notify {
                off: has(rest, "off"),
                kinds: rest
                    .iter()
                    .filter(|a| *a != "off" && !a.starts_with("--"))
                    .cloned()
                    .collect(),
                bypass: has(rest, "--bypass"),
            })
        }
        "watch" => {
            let rest = &args[1..];
            let number = |flag: &str, fallback: f64| -> Result<f64> {
                match value(rest, flag) {
                    None => Ok(fallback),
                    Some(v) => {
                        v.parse().map_err(|_| anyhow::anyhow!("{flag} wants a number, not {v:?}"))
                    }
                }
            };
            Ok(Cmd::Watch {
                every: number("--every", 300.0)? as u64,
                high: number("--high", 90.0)?,
                rotate: pool(rest),
            })
        }
        "serve" | "gateway" => {
            let rest = &args[1..];
            if has(rest, "--key") {
                return Ok(Cmd::ServeKey);
            }
            let port = match value(rest, "--port") {
                None => 4141,
                Some(v) => match v.parse() {
                    Ok(0) | Err(_) => bail!("--port wants a port, not {v:?}"),
                    Ok(port) => port,
                },
            };
            Ok(Cmd::Serve { port, rotate: pool(rest) })
        }
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
        "pin" | "confine" => {
            let (mine, forwarded) = forwarded(&args[1..]);
            Ok(Cmd::Pin { target: positional(mine), args: forwarded })
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

/// Every account named after a `--rotate`, comma-separated or repeated.
fn pool(args: &[String]) -> Vec<String> {
    values(args, "--rotate")
        .flat_map(|v| v.split(',').map(str::trim).map(String::from).collect::<Vec<_>>())
        .filter(|v| !v.is_empty())
        .collect()
}

/// Split at `--`: what follows belongs to the command being launched rather
/// than to this one, flags and all.
fn forwarded(args: &[String]) -> (&[String], Vec<String>) {
    let Some(at) = args.iter().position(|a| a == "--") else { return (args, Vec::new()) };
    (&args[..at], args[at + 1..].to_vec())
}

fn has(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

/// The value following `flag`, when present.
fn value(args: &[String], flag: &str) -> Option<String> {
    let at = args.iter().position(|a| a == flag)?;
    args.get(at + 1).cloned()
}

/// Every value following an occurrence of `flag`.
fn values<'a>(args: &'a [String], flag: &'a str) -> impl Iterator<Item = &'a String> + 'a {
    args.windows(2).filter(move |w| w[0] == flag).map(|w| &w[1])
}

/// Flags that consume the argument after them.
const VALUE_FLAGS: [&str; 6] = ["--name", "--email", "--every", "--high", "--rotate", "--port"];

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
        assert!(matches!(parsed(&["ls"]), Cmd::List { json: false, cached: false }));
        assert!(matches!(parsed(&["list", "--json"]), Cmd::List { json: true, .. }));
    }

    #[test]
    fn list_can_be_asked_for_the_last_readings_rather_than_a_poll() {
        assert!(matches!(
            parsed(&["ls", "--cached", "--json"]),
            Cmd::List { json: true, cached: true }
        ));
    }

    #[test]
    fn use_takes_a_target_and_an_optional_force() {
        let Cmd::Use { target, force } = parsed(&["use", "work"]) else { panic!("not a use") };
        assert_eq!((target.as_str(), force), ("work", false));

        let Cmd::Use { force, .. } = parsed(&["use", "work", "-f"]) else { panic!("not a use") };
        assert!(force);
    }

    #[test]
    fn a_flag_before_the_target_does_not_become_the_target() {
        let Cmd::Use { target, force } = parsed(&["use", "--force", "work"]) else {
            panic!("not a use")
        };
        assert_eq!((target.as_str(), force), ("work", true));
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
    fn pin_without_a_target_asks_rather_than_failing() {
        let Cmd::Pin { target, args } = parsed(&["pin"]) else { panic!("not a pin") };
        assert_eq!(target, None);
        assert!(args.is_empty());
    }

    #[test]
    fn pin_takes_a_target_when_one_is_given() {
        let Cmd::Pin { target, .. } = parsed(&["pin", "work"]) else { panic!("not a pin") };
        assert_eq!(target.as_deref(), Some("work"));
    }

    #[test]
    fn pin_hands_everything_after_a_double_dash_to_claude_code() {
        let Cmd::Pin { target, args } = parsed(&["pin", "work", "--", "--continue", "-p", "hi"])
        else {
            panic!("not a pin")
        };
        assert_eq!(target.as_deref(), Some("work"));
        assert_eq!(args, ["--continue", "-p", "hi"]);
    }

    #[test]
    fn a_forwarded_flag_is_never_mistaken_for_the_target() {
        let Cmd::Pin { target, args } = parsed(&["pin", "--", "resume"]) else {
            panic!("not a pin")
        };
        assert_eq!(target, None);
        assert_eq!(args, ["resume"]);
    }

    #[test]
    fn watch_collects_a_rotation_pool_from_commas_and_repeats() {
        let Cmd::Watch { rotate, high, .. } =
            parsed(&["watch", "--rotate", "a, b", "--high", "95", "--rotate", "c"])
        else {
            panic!("not a watch")
        };
        assert_eq!(rotate, ["a", "b", "c"]);
        assert_eq!(high, 95.0);
        assert!(matches!(parsed(&["watch"]), Cmd::Watch { rotate, .. } if rotate.is_empty()));
    }

    #[test]
    fn serve_defaults_its_port_and_takes_a_pool_like_watch() {
        let Cmd::Serve { port, rotate } = parsed(&["serve"]) else { panic!("not a serve") };
        assert_eq!((port, rotate.is_empty()), (4141, true));

        let Cmd::Serve { port, rotate } = parsed(&["serve", "--port", "8080", "--rotate", "a,b"])
        else {
            panic!("not a serve")
        };
        assert_eq!(port, 8080);
        assert_eq!(rotate, ["a", "b"]);
    }

    #[test]
    fn serve_key_only_prints_the_key() {
        assert!(matches!(parsed(&["serve", "--key"]), Cmd::ServeKey));
    }

    #[test]
    fn serve_refuses_port_zero_since_the_snippet_could_not_name_it() {
        assert!(
            parse(["serve".to_string(), "--port".to_string(), "0".to_string()].into_iter())
                .is_err()
        );
    }

    #[test]
    fn serve_refuses_a_port_that_is_not_one() {
        assert!(
            parse(["serve".to_string(), "--port".to_string(), "lots".to_string()].into_iter())
                .is_err()
        );
    }

    #[test]
    fn an_unknown_command_is_refused() {
        assert!(parse(["frobnicate".to_string()].into_iter()).is_err());
    }
}
