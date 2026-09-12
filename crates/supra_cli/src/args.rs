#![deny(missing_docs)]
#![allow(unreachable_pub, reason = "Cli/Command used only in main.rs, but pub for wiring")]

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "supra", version, about = "Peer-validated coding-agent harness")]
pub struct Cli {
    #[arg(long, help = "Skip project-local configuration; requires --yes")]
    pub ignore_project_config: bool,

    #[arg(long, help = "Confirm security-sensitive command-line overrides")]
    pub yes: bool,

    #[arg(long, value_name = "PATH", help = "Use PATH as the encrypted secrets store")]
    pub secrets_file: Option<PathBuf>,

    #[arg(long, value_enum, help = "Override the permission mode")]
    pub mode: Option<ModeArg>,

    #[arg(long, value_name = "N", help = "Set the reasoning token budget")]
    pub think: Option<u32>,

    #[arg(long, value_enum, default_value_t = SandboxArg::On, help = "Enable or disable sandboxing; off requires --yes")]
    pub sandbox: SandboxArg,

    #[arg(long, value_name = "PATH", help = "Write structured logs to PATH")]
    pub log: Option<PathBuf>,

    #[arg(long, value_enum, help = "Select the structured log format")]
    pub log_format: Option<LogFormatArg>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ModeArg {
    Plan,
    Ask,
    Auto,
    Yolo,
}

impl ModeArg {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Ask => "ask",
            Self::Auto => "auto",
            Self::Yolo => "yolo",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum SandboxArg {
    #[default]
    On,
    Off,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum LogFormatArg {
    Json,
    Compact,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(about = "Execute one peer-validated provider turn")]
    Run {
        #[arg(long, value_name = "NAME", help = "Use the configured provider NAME")]
        provider: Option<String>,
        #[arg(required = true, trailing_var_arg = true, help = "Task to send to the provider cohort")]
        task: Vec<String>,
    },
    #[command(about = "Run the offline evaluation suite")]
    Eval {
        #[arg(long, help = "Also run the provider-backed evaluation probe")]
        live: bool,
    },
    #[command(about = "Check or apply signed supra updates")]
    Update {
        #[command(subcommand)]
        action: UpdateAction,
    },
    #[command(about = "Inspect resolved configuration")]
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum UpdateAction {
    #[command(about = "Check whether a signed update is available")]
    Check,
    #[command(about = "Apply an update after signature verification")]
    Apply,
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    #[command(about = "Show each setting and its winning source")]
    Show,
}

impl Cli {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.ignore_project_config && !self.yes {
            anyhow::bail!("--ignore-project-config requires --yes to confirm");
        }
        if self.sandbox == SandboxArg::Off && !self.yes {
            anyhow::bail!("--sandbox off requires --yes to confirm");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Cli {
        <Cli as clap::Parser>::try_parse_from(["supra"]).expect("defaults parse")
    }

    #[test]
    fn the_defaults_require_no_confirmation() {
        let cli = base();
        assert!(!cli.ignore_project_config);
        assert!(!cli.yes);
        assert_eq!(cli.sandbox, SandboxArg::On);
        cli.validate().expect("defaults are valid");
    }

    #[test]
    fn ignoring_the_project_config_requires_confirmation() {
        let mut cli = base();
        cli.ignore_project_config = true;
        assert!(cli.validate().is_err(), "without --yes");
        cli.yes = true;
        cli.validate().expect("with --yes");
    }

    #[test]
    fn turning_the_sandbox_off_requires_confirmation() {
        let mut cli = base();
        cli.sandbox = SandboxArg::Off;
        assert!(cli.validate().is_err(), "without --yes");
        cli.yes = true;
        cli.validate().expect("with --yes");
    }

    #[test]
    fn clap_rejects_unknown_enum_values() {
        for args in [
            ["supra", "--sandbox", "sometimes"],
            ["supra", "--mode", "reckless"],
            ["supra", "--log-format", "xml"],
        ] {
            assert!(<Cli as clap::Parser>::try_parse_from(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn help_describes_security_flags_and_commands() {
        use clap::CommandFactory as _;

        let mut top = Cli::command();
        let top_help = top.render_long_help().to_string();
        assert!(top_help.contains("Confirm security-sensitive"), "{top_help}");
        assert!(top_help.contains("Enable or disable sandboxing"), "{top_help}");
        assert!(top_help.contains("Execute one peer-validated provider turn"), "{top_help}");

        let mut run = Cli::command().find_subcommand_mut("run").expect("run command").clone();
        let run_help = run.render_long_help().to_string();
        assert!(run_help.contains("configured provider"), "{run_help}");
        assert!(run_help.contains("Task to send"), "{run_help}");

        let mut update = Cli::command().find_subcommand_mut("update").expect("update command").clone();
        let update_help = update.render_long_help().to_string();
        assert!(update_help.contains("Apply an update after signature verification"), "{update_help}");
    }
}
