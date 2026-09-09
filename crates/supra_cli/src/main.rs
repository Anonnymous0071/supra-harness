//! `supra` — the single binary for T30.
//!
//! One binary, subcommands `run` (default), `eval`, `update`, `config show`.

#![deny(missing_docs)]
#![forbid(unsafe_code)]
#![allow(clippy::print_stdout, reason = "binary output")]
#![allow(clippy::unnecessary_wraps, reason = "handlers return Result for ? in later wiring")]
#![allow(clippy::needless_pass_by_value, reason = "handler enums small; reference adds noise")]
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

mod args;
mod registry;
mod runtime;
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
    let estimated = runtime::estimate_tier();
    let planned = runtime::plan_turn(&assembled.config, estimated);
    println!(
        "mode {} cohort {} sessions {} | {}",
        assembled.config.permission_mode().label(),
        assembled.config.cohort_limit(),
        wired.session_dir.display(),
        runtime::describe(&planned)
    );
    Ok(())
}

fn eval_cmd(live: bool) -> anyhow::Result<()> {
    if !supra_eval::offline_shape_check() {
        anyhow::bail!("offline shape-check: FAILED");
    }
    println!("offline shape-check: ok");
    if !live {
        return Ok(());
    }
    live_probe()
}

fn live_probe() -> anyhow::Result<()> {
    live_probe_report(live_probe_key())?;
    Ok(())
}

fn live_probe_report(key: Option<String>) -> anyhow::Result<&'static str> {
    match key {
        None => {
            println!("live probe skipped: no provider credential in the environment");
            Ok("skipped")
        }
        Some(_) => {
            anyhow::bail!("live probe needs a provider credential and network; shape-check passed")
        }
    }
}

fn live_probe_key() -> Option<String> {
    for var in ["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "GEMINI_API_KEY", "GOOGLE_API_KEY"] {
        if let Ok(value) = std::env::var(var) {
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

fn update_check_message() -> anyhow::Result<String> {
    let current = supra_update::parse_version(env!("CARGO_PKG_VERSION"))?;
    Ok(format!("supra {current}: update check needs network; verify artefacts with supra_update::verify"))
}

fn update_cmd(action: UpdateAction) -> anyhow::Result<()> {
    match action {
        UpdateAction::Check => {
            println!("{}", update_check_message()?);
            Ok(())
        }
        UpdateAction::Apply => {
            anyhow::bail!(
                "supra update apply refuses without a verified signature: fetch, verify, then apply"
            );
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

#[cfg(test)]
mod tests {
    use super::live_probe_report;
    use super::*;

    #[test]
    fn the_live_probe_skips_explicitly_without_a_credential() {
        let report = live_probe_report(None).expect("absence is Ok");
        assert_eq!(report, "skipped");
    }

    #[test]
    fn presence_refuses_until_the_networked_probe_lands() {
        let error = live_probe_report(Some("key".to_owned())).expect_err("presence refuses");
        assert!(error.to_string().contains("needs a provider credential and network"), "{error}");
    }

    #[test]
    fn update_check_names_the_verifier() {
        let message = update_check_message().expect("check never fails without network");
        assert!(
            message.contains("supra_update::verify"),
            "check must name the verifier, not just any help: {message}"
        );
        update_cmd(UpdateAction::Check).expect("check never fails without network");
    }

    #[test]
    fn update_apply_refuses_without_a_signature() {
        let error = update_cmd(UpdateAction::Apply).expect_err("apply must refuse");
        assert!(error.to_string().contains("verified signature"), "{error}");
    }
}
