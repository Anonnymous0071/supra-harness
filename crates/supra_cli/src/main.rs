//! `supra` — the single binary for T30.
//!
//! One binary, subcommands `run` (default), `eval`, `update`, `config show`.

#![deny(missing_docs)]
#![forbid(unsafe_code)]
#![allow(clippy::print_stdout, reason = "binary output")]
#![allow(clippy::unnecessary_wraps, reason = "handlers return Result for ? in later wiring")]
#![allow(clippy::needless_pass_by_value, reason = "handler enums small; reference adds noise")]

use clap::{Parser, Subcommand};

/// Top-level CLI.
#[derive(Debug, Parser)]
#[command(name = "supra", version, about = "supra-harness T30")]
struct Cli {
    /// Do not load the project config (`.supra/config.toml`).
    #[arg(long)]
    ignore_project_config: bool,

    /// Confirm `--ignore-project-config`.
    #[arg(long)]
    yes: bool,

    /// Path to an explicit secrets file.
    #[arg(long, value_name = "PATH")]
    secrets_file: Option<std::path::PathBuf>,

    /// Permission mode override.
    #[arg(long, value_name = "MODE")]
    mode: Option<String>,

    /// Thinking budget override (tokens, 0 to disable).
    #[arg(long, value_name = "N")]
    think: Option<u32>,

    /// Subcommand; defaults to `run` when absent.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the harness (default).
    Run,
    /// Economy gate.
    Eval {
        /// Live probe against providers (needs credentials).
        #[arg(long)]
        live: bool,
    },
    /// Updater.
    Update {
        #[command(subcommand)]
        action: UpdateAction,
    },
    /// Show resolved configuration.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Debug, Subcommand)]
enum UpdateAction {
    /// Check for a newer release.
    Check,
    /// Apply the latest release.
    Apply,
}

#[derive(Debug, Subcommand)]
enum ConfigAction {
    /// Print the resolved config and where each value came from.
    Show,
}

fn main() -> anyhow::Result<()> {
    let mut cli = Cli::parse();

    if cli.ignore_project_config && !cli.yes {
        anyhow::bail!("--ignore-project-config requires --yes to confirm");
    }

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
