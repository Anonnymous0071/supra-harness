//! `supra` — the single binary for T30.
//!
//! One binary, subcommands `run` (default), `eval`, `update`, `config show`.

#![deny(missing_docs)]
#![forbid(unsafe_code)]
#![allow(clippy::print_stdout, reason = "binary output")]
#![allow(clippy::unnecessary_wraps, reason = "handlers return Result for ? in later wiring")]
#![allow(clippy::needless_pass_by_value, reason = "handler enums small; reference adds noise")]

mod args;

use args::{Cli, Command, ConfigAction, UpdateAction};
use clap::Parser;

fn main() -> anyhow::Result<()> {
    let mut cli = Cli::parse();
    cli.validate()?;

    let command = cli.command.take().unwrap_or(Command::Run);
    match command {
        Command::Run => run(cli),
        Command::Eval { live } => eval_cmd(live),
        Command::Update { action } => update_cmd(action),
        Command::Config { action } => config_cmd(action),
    }
}

fn run(_cli: Cli) -> anyhow::Result<()> {
    println!("supra run: wiring lands in T30.2+");
    Ok(())
}

fn eval_cmd(live: bool) -> anyhow::Result<()> {
    if live {
        anyhow::bail!("live probe not yet wired; use `supra eval` for the offline shape-check");
    }
    if supra_eval::offline_shape_check() {
        println!("offline shape-check: ok");
        Ok(())
    } else {
        anyhow::bail!("offline shape-check: FAILED");
    }
}

fn update_cmd(action: UpdateAction) -> anyhow::Result<()> {
    match action {
        UpdateAction::Check => {
            println!("supra update check: not yet wired");
            Ok(())
        }
        UpdateAction::Apply => {
            anyhow::bail!("supra update apply: not yet wired");
        }
    }
}

fn config_cmd(action: ConfigAction) -> anyhow::Result<()> {
    match action {
        ConfigAction::Show => {
            println!("supra config show: not yet wired");
            Ok(())
        }
    }
}
