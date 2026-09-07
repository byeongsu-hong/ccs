use std::process::ExitCode;

use anyhow::Result;

use ccs::cli::{self, Cmd};
use ccs::env::Env;
use ccs::model::Provider;
use ccs::{cmd, login};

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

    let env = Env::open()?;
    let ctx = env.ctx();

    match command {
        Cmd::Pick => cmd::pick(&ctx),
        Cmd::List { json, cached } => cmd::list(&ctx, json, cached),
        Cmd::Use { target, force } => cmd::use_account(&ctx, &target, force),
        Cmd::Add { name, current, email, console, sso, codex } => {
            let options = login::Options { email, console, sso };
            let provider = if codex { Provider::Codex } else { Provider::Claude };
            cmd::add(&ctx, provider, name.as_deref(), current, &options)
        }
        Cmd::Pin { target, args } => cmd::pin(&ctx, target.as_deref(), &args),
        Cmd::Remove { target } => cmd::remove(&ctx, &target),
        Cmd::Status { json } => cmd::status(&ctx, json),
        Cmd::Notify { off, kinds, bypass } => cmd::notify(&ctx, off, &kinds, bypass),
        Cmd::Watch { every, high, rotate } => {
            cmd::watch(&ctx, std::time::Duration::from_secs(every), high, &rotate)
        }
        Cmd::Serve { port, rotate } => cmd::serve(&ctx, port, &rotate),
        Cmd::ServeKey { provider } => cmd::serve_key(&ctx, provider),
        Cmd::Help | Cmd::Version => unreachable!("answered before the wiring above"),
    }
}
