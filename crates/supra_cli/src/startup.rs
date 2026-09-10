#![deny(missing_docs)]
#![allow(dead_code, reason = "startup wiring grows stepwise through T30")]

use std::path::PathBuf;

use supra_config::{Config, ConfigLayer, Loader, PermissionLayer, ThinkingLayer};
use supra_log::{Format, LogHandle, LogOptions};

use crate::args::{Cli, LogFormatArg};

pub(crate) struct Startup {
    pub(crate) config: Config,
    pub(crate) log: Option<LogHandle>,
    pub(crate) sandbox_off: bool,
}

pub(crate) fn discover_resolve(cli: &Cli) -> anyhow::Result<Config> {
    let mut loader = Loader::from_environment();
    if cli.ignore_project_config {
        loader = loader.with_project_start(PathBuf::from("/nonexistent-supra-no-project"));
    }
    let mut cli_layer = ConfigLayer::default();
    if let Some(mode) = cli.mode {
        let setting = supra_config::ModeSetting::try_from(mode.as_str().to_owned())
            .map_err(|detail| anyhow::anyhow!("{detail}"))?;
        cli_layer.permission.get_or_insert_with(PermissionLayer::default).mode = Some(setting);
    }
    if let Some(budget) = cli.think {
        cli_layer.thinking.get_or_insert_with(ThinkingLayer::default).budget = Some(budget);
    }
    loader = loader.with_cli(cli_layer);
    Ok(loader.load()?)
}

pub(crate) fn assemble(config: Config, log: Option<LogHandle>, sandbox_off: bool) -> Startup {
    Startup { config, log, sandbox_off }
}

pub(crate) fn init_logging(cli: &Cli) -> anyhow::Result<Option<LogHandle>> {
    let mut options = LogOptions::default();
    if let Some(path) = &cli.log {
        options = options.with_path(path.clone());
    }
    if let Some(format) = cli.log_format {
        options = options.with_format(match format {
            LogFormatArg::Json => Format::Json,
            LogFormatArg::Compact => Format::Compact,
        });
    }
    match supra_log::init(options) {
        Ok(handle) => Ok(Some(handle)),
        Err(supra_log::LogError::AlreadyInstalled) => Ok(None),
        Err(other) => Err(anyhow::anyhow!("{other}")),
    }
}

pub(crate) fn open_secrets(cli: &Cli) -> supra_secrets::SecretManager {
    match &cli.secrets_file {
        Some(path) => supra_secrets::SecretManager::open_with_file_store(path.clone()),
        None => supra_secrets::SecretManager::open(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::Cli;

    fn cli_with(args: &[&str]) -> Cli {
        let mut full = vec!["supra"];
        full.extend_from_slice(args);
        <Cli as clap::Parser>::try_parse_from(full).expect("args parse")
    }

    #[test]
    fn resolution_is_total_over_an_empty_environment() {
        let cli = cli_with(&[]);
        let config = discover_resolve(&cli).expect("empty env resolves to builtins");
        assert_eq!(config.cohort_limit(), supra_types::DEFAULT_PEER_LIMIT);
        assert_eq!(config.thinking_budget(), 0);
    }

    #[test]
    fn a_cli_mode_rides_the_cli_layer() {
        let cli = cli_with(&["--mode", "yolo"]);
        let config = discover_resolve(&cli).expect("mode parses");
        assert_eq!(config.permission_mode(), supra_types::Mode::Yolo);
        assert_eq!(config.source_of(supra_config::Setting::PermissionMode), supra_config::ConfigSource::Cli);
    }

    #[test]
    fn a_cli_think_budget_rides_the_cli_layer() {
        let cli = cli_with(&["--think", "1024"]);
        let config = discover_resolve(&cli).expect("budget parses");
        assert_eq!(config.thinking_budget(), 1024);
    }

    #[test]
    fn ignoring_the_project_config_needs_no_filesystem() {
        let cli = cli_with(&["--ignore-project-config", "--yes"]);
        discover_resolve(&cli).expect("escape hatch resolves");
    }

    #[test]
    fn secrets_open_with_an_explicit_file() {
        let dir = std::env::temp_dir().join("supra-cli-startup-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let file = dir.join("secrets.enc");
        let cli = cli_with(&["--secrets-file", file.to_str().expect("utf8")]);
        let manager = open_secrets(&cli);
        assert_eq!(manager.file_store_path(), file.as_path());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
