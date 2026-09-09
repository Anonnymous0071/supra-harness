//! `supra` — the single binary for T30.
//!
//! One binary, subcommands `run` (default), `eval`, `update`, `config show`.

#![deny(missing_docs)]
#![forbid(unsafe_code)]
#![allow(clippy::print_stdout, reason = "binary output")]
#![allow(clippy::unnecessary_wraps, reason = "handlers return Result for ? in later wiring")]
#![allow(clippy::needless_pass_by_value, reason = "handler enums small; reference adds noise")]

mod args;
mod registry;
mod startup;

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

fn run(cli: Cli) -> anyhow::Result<()> {
    let config = startup::discover_resolve(&cli)?;
    let log = startup::init_logging(&cli)?;
    let secrets = startup::open_secrets(&cli);
    let _ = secrets.primary_backend();
    let sandbox_off = cli.sandbox.eq_ignore_ascii_case("off");
    let wired = registry::wire(&config);
    let assembled = startup::assemble(config, log, sandbox_off);
    println!(
        "mode {} cohort {} sessions {}",
        assembled.config.permission_mode().label(),
        assembled.config.cohort_limit(),
        wired.session_dir.display()
    );
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
    let cli = Cli::parse();
    match action {
        ConfigAction::Show => {
            let config = startup::discover_resolve(&cli)?;
            for setting in supra_config::Setting::ALL {
                println!("{} = {:?}", setting.path(), config.source_of(setting));
            }
            Ok(())
        }
    }
}
