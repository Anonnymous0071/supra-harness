#![deny(missing_docs)]
#![allow(unreachable_pub, reason = "Cli/Command used only in main.rs, but pub for wiring")]

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "supra", version, about = "supra-harness T30")]
pub struct Cli {
    #[arg(long)]
    pub ignore_project_config: bool,

    #[arg(long)]
    pub yes: bool,

    #[arg(long, value_name = "PATH")]
    pub secrets_file: Option<PathBuf>,

    #[arg(long, value_name = "MODE")]
    pub mode: Option<String>,

    #[arg(long, value_name = "N")]
    pub think: Option<u32>,

    #[arg(long, value_name = "ON|OFF", default_value = "on")]
    pub sandbox: String,

    #[arg(long, value_name = "PATH")]
    pub log: Option<PathBuf>,

    #[arg(long, value_name = "FORMAT")]
    pub log_format: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Run,
    Eval {
        #[arg(long)]
        live: bool,
    },
    Update {
        #[command(subcommand)]
        action: UpdateAction,
    },
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum UpdateAction {
    Check,
    Apply,
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    Show,
}

impl Cli {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.ignore_project_config && !self.yes {
            anyhow::bail!("--ignore-project-config requires --yes to confirm");
        }
        if self.sandbox.eq_ignore_ascii_case("off") && !self.yes {
            anyhow::bail!("--sandbox off requires --yes to confirm");
        }
        if !self.sandbox.eq_ignore_ascii_case("on") && !self.sandbox.eq_ignore_ascii_case("off") {
            anyhow::bail!("--sandbox must be on or off");
        }
        if let Some(mode) = &self.mode {
            let ok = ["plan", "ask", "auto", "yolo"].contains(&mode.as_str());
            if !ok {
                anyhow::bail!("--mode must be one of plan, ask, auto, yolo");
            }
        }
        if let Some(fmt) = &self.log_format {
            if fmt != "json" && fmt != "compact" {
                anyhow::bail!("--log-format must be json or compact");
            }
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
        assert_eq!(cli.sandbox, "on");
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
        cli.sandbox = "off".to_owned();
        assert!(cli.validate().is_err(), "without --yes");
        cli.yes = true;
        cli.validate().expect("with --yes");
    }

    #[test]
    fn the_sandbox_flag_only_names_on_or_off() {
        let mut cli = base();
        cli.sandbox = "sometimes".to_owned();
        assert!(cli.validate().is_err());
        for spelling in ["on", "ON", "off", "OFF"] {
            let mut cli = base();
            cli.sandbox = spelling.to_owned();
            cli.yes = true;
            cli.validate().unwrap_or_else(|error| panic!("{spelling} is valid: {error}"));
        }
    }

    #[test]
    fn the_mode_flag_only_names_a_real_mode() {
        for mode in ["plan", "ask", "auto", "yolo"] {
            let mut cli = base();
            cli.mode = Some(mode.to_owned());
            cli.validate().unwrap_or_else(|error| panic!("{mode} is valid: {error}"));
        }
        let mut cli = base();
        cli.mode = Some("reckless".to_owned());
        assert!(cli.validate().is_err());
    }

    #[test]
    fn the_log_format_only_names_json_or_compact() {
        for format in ["json", "compact"] {
            let mut cli = base();
            cli.log_format = Some(format.to_owned());
            cli.validate().unwrap_or_else(|error| panic!("{format} is valid: {error}"));
        }
        let mut cli = base();
        cli.log_format = Some("xml".to_owned());
        assert!(cli.validate().is_err());
    }
}
