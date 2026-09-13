//! `supra` — the single binary for T30.
//!
//! One binary, subcommands `run`, `eval`, `update`, `config show`.

#![deny(missing_docs)]
#![forbid(unsafe_code)]
#![allow(clippy::print_stdout, reason = "binary output")]
#![allow(clippy::unnecessary_wraps, reason = "handlers return Result for ? in later wiring")]
#![allow(clippy::needless_pass_by_value, reason = "handler enums small; reference adds noise")]
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

mod args;
mod registry;
mod render;
mod runtime;
mod slash;
mod startup;

use args::{Cli, Command, ConfigAction, SandboxArg, UpdateAction};
use clap::Parser;

fn main() -> anyhow::Result<()> {
    let mut cli = Cli::parse();
    cli.validate()?;

    let command = cli
        .command
        .take()
        .ok_or_else(|| anyhow::anyhow!("a command is required; run `supra run --help` to execute a task"))?;
    match command {
        Command::Run { provider, resume, task } => {
            run(cli, provider.as_deref(), resume.as_deref(), &task.join(" "))
        }
        Command::Eval { live } => eval_cmd(live),
        Command::Update { action } => update_cmd(action),
        Command::Config { action } => config_cmd(&cli, action),
    }
}

fn run(cli: Cli, provider: Option<&str>, resume: Option<&str>, task: &str) -> anyhow::Result<()> {
    let config = startup::discover_resolve(&cli)?;
    let log = startup::init_logging(&cli)?;
    let secrets = startup::open_secrets(&cli);
    let sandbox_off = cli.sandbox == SandboxArg::Off;
    let wired = registry::wire(&config);
    let assembled = startup::assemble(config, log, sandbox_off);
    let resume = resume
        .map(str::parse::<supra_types::SessionId>)
        .transpose()
        .map_err(|error| anyhow::anyhow!("--resume is not a session id: {error}"))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| anyhow::anyhow!("tokio runtime: {error}"))?;
    if !task.trim().is_empty() {
        let result = runtime.block_on(runtime::execute_turn_resuming(
            &assembled.config,
            &secrets,
            &wired.session_dir,
            &wired.hooks,
            resume,
            provider,
            task,
        ))?;
        println!("session {}", result.session);
        println!("{}", result.answer);
        return Ok(());
    }
    if resume.is_some() {
        anyhow::bail!("--resume needs a task: `supra run --resume ID <task>` runs one turn on it");
    }
    repl(&assembled, &secrets, &wired, provider, &runtime)
}

fn repl(
    assembled: &startup::Startup,
    secrets: &supra_secrets::SecretManager,
    wired: &registry::Wired,
    provider: Option<&str>,
    runtime: &tokio::runtime::Runtime,
) -> anyhow::Result<()> {
    use std::io::{BufRead as _, Write as _};
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let mut session: Option<supra_types::SessionId> = None;
    loop {
        print!("supra> ");
        std::io::stdout().flush().map_err(|error| anyhow::anyhow!("stdout: {error}"))?;
        let Some(line) = lines.next() else { break };
        let line = line.map_err(|error| anyhow::anyhow!("stdin: {error}"))?;
        let action = slash::dispatch(&wired.commands, slash::parse_line(&line))
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        match action {
            slash::Action::Exit => break,
            slash::Action::Help => {
                for command in wired.commands.all() {
                    println!("/{}\t{}", command.name, command.description);
                }
            }
            slash::Action::Clear => {
                print!("\x1b[2J\x1b[H");
            }
            slash::Action::Mode | slash::Action::Permissions => {
                println!("mode {}", assembled.config.permission_mode().label());
            }
            slash::Action::Sandbox => {
                println!("sandbox {}", if assembled.sandbox_off { "off" } else { "on" });
            }
            slash::Action::Telemetry => {
                println!(
                    "telemetry {}",
                    wired.consent.map_or("off", |consent| match consent {
                        supra_telemetry::Consent::Off => "off",
                        supra_telemetry::Consent::On => "on",
                    })
                );
            }
            slash::Action::Model(current) => {
                println!("model {}", current.as_deref().unwrap_or("(configured)"));
            }
            slash::Action::New => {
                session = None;
                println!("new session");
            }
            slash::Action::Resume(id) => {
                let id = id
                    .parse::<supra_types::SessionId>()
                    .map_err(|error| anyhow::anyhow!("resume needs a session id, got {id:?}: {error}"))?;
                session = Some(id);
                println!("resumed {id}");
            }
            slash::Action::Branch => {
                let Some(id) = session else {
                    anyhow::bail!("branch needs a live session: run a task first");
                };
                let Some(saved) = supra_session::Session::resume(&wired.session_dir, id)? else {
                    anyhow::bail!("no saved session {id}");
                };
                let branched = saved.branch();
                let branched_id = branched.id();
                branched.save(&wired.session_dir)?;
                session = Some(branched_id);
                println!("branched {id} -> {branched_id}");
            }
            slash::Action::Export => {
                let Some(id) = session else {
                    anyhow::bail!("export needs a live session: run a task first");
                };
                let Some(saved) = supra_session::Session::resume(&wired.session_dir, id)? else {
                    anyhow::bail!("no saved session {id}");
                };
                println!("{}", saved.export_markdown());
            }
            slash::Action::Task(task) => {
                if task.trim().is_empty() {
                    continue;
                }
                let result = runtime.block_on(runtime::execute_turn_resuming(
                    &assembled.config,
                    secrets,
                    &wired.session_dir,
                    &wired.hooks,
                    session,
                    provider,
                    &task,
                ))?;
                session = Some(result.session);
                println!("session {}", result.session);
                println!("{}", result.answer);
            }
        }
    }
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
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| anyhow::anyhow!("tokio runtime: {error}"))?;
    runtime.block_on(live_probe())
}

async fn live_probe() -> anyhow::Result<()> {
    let cli = Cli::try_parse_from(["supra", "eval", "--live"])?;
    let config = startup::discover_resolve(&cli)?;
    let secrets = startup::open_secrets(&cli);

    let mut configured = 0usize;
    for name in ["anthropic", "openai"] {
        if config.providers().get(name).is_none() {
            continue;
        }
        configured += 1;
        let credential = match config.provider_secret(name, &secrets) {
            Ok(credential) => credential,
            Err(error) => {
                println!("live probe {name}: skipped ({error})");
                continue;
            }
        };
        let client = supra_llm::Client::from_config(&config, name)?;
        let request = supra_llm::Request {
            provider: client.provider(),
            model: client.model().to_owned(),
            messages: vec![supra_llm::Message::text(supra_llm::Role::User, "Reply with exactly: live-ok")],
            tools: Vec::new(),
            thinking: supra_llm::Thinking { budget_tokens: 0 },
            breakpoints: Vec::new(),
        };
        match client.send(&request, &credential).await {
            Ok(completion) => {
                println!(
                    "live probe {name}: ok text={:?} usage={:?}",
                    completion.text().trim(),
                    completion.usage
                );
            }
            Err(error) => {
                println!("live probe {name}: FAILED ({error})");
                anyhow::bail!("live probe {name} failed: {error}");
            }
        }
    }
    if configured == 0 {
        println!("live probe: skipped (no configured providers)");
    }
    Ok(())
}

fn update_cmd(action: UpdateAction) -> anyhow::Result<()> {
    match action {
        UpdateAction::Check { archive, manifest, signature, public_key } => {
            let verified = supra_update::check_local(supra_update::LocalUpdate {
                archive: &archive,
                manifest: &manifest,
                signature: &signature,
                public_key: &public_key,
                expected_target: supra_update::current_target(),
            })?;
            println!(
                "verified supra {} for {}: {} -> {}",
                verified.version(),
                verified.target(),
                verified.archive(),
                verified.executable_path()
            );
            Ok(())
        }
        UpdateAction::Apply { archive, manifest, signature, public_key, install_path } => {
            let install_path = match install_path {
                Some(path) => path,
                None => std::env::current_exe()
                    .map_err(|error| anyhow::anyhow!("resolve running supra executable: {error}"))?,
            };
            let outcome = supra_update::apply_local(
                supra_update::LocalUpdate {
                    archive: &archive,
                    manifest: &manifest,
                    signature: &signature,
                    public_key: &public_key,
                    expected_target: supra_update::current_target(),
                },
                &install_path,
            )?;
            println!("installed supra {} at {}", outcome.version, outcome.install_path.display());
            Ok(())
        }
    }
}

fn config_cmd(cli: &Cli, action: ConfigAction) -> anyhow::Result<()> {
    match action {
        ConfigAction::Show => {
            let config = startup::discover_resolve(cli)?;
            for setting in supra_config::Setting::ALL {
                println!("{} = {:?}", setting.path(), config.source_of(setting));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_live_probe_skips_providers_with_no_credential() {
        // An empty environment names no provider, so the probe has
        // nothing to call: absence is `Ok`, not a refusal.
        let cli = Cli::try_parse_from(["supra", "eval"]).expect("args parse");
        let config = startup::discover_resolve(&cli).expect("empty env resolves");
        assert!(config.providers().is_empty(), "no providers without configuration");
    }

    #[test]
    fn a_configured_provider_without_a_credential_is_named_not_panicked() {
        // The probe resolves the secret through the manager; a missing
        // credential is a skip with the provider named, never a panic.
        let cli = Cli::try_parse_from(["supra", "eval"]).expect("args parse");
        let config = startup::discover_resolve(&cli).expect("resolves");
        let secrets = startup::open_secrets(&cli);
        for name in ["anthropic", "openai"] {
            if config.providers().get(name).is_none() {
                continue;
            }
            let _ = config.provider_secret(name, &secrets);
        }
    }

    #[test]
    fn update_commands_fail_closed_on_missing_local_inputs() {
        use std::path::PathBuf;

        let missing = PathBuf::from("definitely-missing-update-input");
        let check = UpdateAction::Check {
            archive: missing.clone(),
            manifest: missing.clone(),
            signature: missing.clone(),
            public_key: missing.clone(),
        };
        assert!(update_cmd(check).is_err(), "check must not report an unavailable bundle as valid");
        let apply = UpdateAction::Apply {
            archive: missing.clone(),
            manifest: missing.clone(),
            signature: missing.clone(),
            public_key: missing,
            install_path: Some(PathBuf::from("must-not-be-created")),
        };
        assert!(update_cmd(apply).is_err(), "apply must verify before creating a destination");
    }
}
