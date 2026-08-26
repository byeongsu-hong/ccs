mod api;
mod cli;
mod cmd;
mod creds;
mod fsx;
mod lock;
mod model;
mod picker;
mod render;
mod stash;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};

use crate::api::Api;
use crate::cli::Cmd;
use crate::creds::FileStore;
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
    let creds = FileStore::new(&config_dir);
    let stash = Stash::open(&config_dir)?;
    let api = Api::new();
    let ctx = cmd::Ctx { creds: &creds, stash: &stash, api: &api, config_dir: &config_dir };

    match command {
        Cmd::Pick => cmd::pick(&ctx),
        Cmd::List { json } => cmd::list(&ctx, json),
        Cmd::Use { target, force } => cmd::use_account(&ctx, &target, force),
        Cmd::Add { name } => cmd::add(&ctx, name.as_deref()),
        Cmd::Remove { target } => cmd::remove(&ctx, &target),
        Cmd::Status { json } => cmd::status(&ctx, json),
        Cmd::Help | Cmd::Version => unreachable!("answered before the wiring above"),
    }
}

/// Claude Code's configuration directory, honouring the same override Claude
/// Code itself honours.
fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".claude"))
}
