mod api;
mod cli;
mod cmd;
mod creds;
mod fsx;
mod lock;
mod login;
mod model;
mod notify;
mod pen;
mod picker;
mod render;
mod sha256;
mod stash;
mod usage;
mod watch;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};

use crate::api::Api;
use crate::cli::Cmd;
use crate::creds::Backend;
use crate::stash::Stash;

fn main() -> ExitCode {
    let Err(error) = run() else { return ExitCode::SUCCESS };
    let detail = error.chain().map(|c| c.to_string()).collect::<Vec<_>>().join(": ");
    eprintln!("ccs: {detail}");
    ExitCode::FAILURE
}

fn run() -> Result<()> {
    let command = cli::parse(std::env::args().skip(1))?;
    match command {
        Cmd::Help => {
            print!("{}", cli::HELP);
            return Ok(());
        }
        Cmd::Version => {
            println!("ccs {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        _ => {}
    }

    let config_dir = config_dir()?;
    // A pen records the configuration it was cut from; anywhere else, this is it.
    let here = pen::Home { config: config_dir.clone(), global: global_config()? };
    let home = pen::home_of(&config_dir).unwrap_or(here);

    let backend = Backend::detect();
    let creds = backend.live(&config_dir);
    let stash = Stash::open(&home.config)?;
    let usage = usage::Cache::open(stash.root())?;
    let api = Api::new();
    let ctx = cmd::Ctx {
        creds: creds.as_ref(),
        backend,
        stash: &stash,
        usage: &usage,
        api: &api,
        home: &home,
    };

    match command {
        Cmd::Pick => cmd::pick(&ctx),
        Cmd::List { json } => cmd::list(&ctx, json),
        Cmd::Use { target, force } => cmd::use_account(&ctx, &target, force),
        Cmd::Add { name, current, email, console, sso } => {
            let options = login::Options { email, console, sso };
            cmd::add(&ctx, name.as_deref(), current, &options)
        }
        Cmd::Pin { target, args } => cmd::pin(&ctx, target.as_deref(), &args),
        Cmd::Remove { target } => cmd::remove(&ctx, &target),
        Cmd::Status { json } => cmd::status(&ctx, json),
        Cmd::Notify { off, kinds } => cmd::notify(&ctx, off, &kinds),
        Cmd::Watch { every, high } => cmd::watch(&ctx, std::time::Duration::from_secs(every), high),
        Cmd::Help | Cmd::Version => unreachable!("answered before the wiring above"),
    }
}

/// The configuration directory this process acts on: where the credentials a
/// switch replaces live. Honours the same override Claude Code itself honours,
/// so a session confined to a pen switches inside that pen rather than out of
/// it.
fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    Ok(home()?.join(".claude"))
}

/// Where Claude Code resolves its global configuration file, which is beside
/// the home directory rather than inside the configuration directory — until
/// `CLAUDE_CONFIG_DIR` is set, which moves it in.
fn global_config() -> Result<PathBuf> {
    let dir = match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => home()?,
    };
    Ok(dir.join(pen::GLOBAL))
}

fn home() -> Result<PathBuf> {
    Ok(PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?))
}
