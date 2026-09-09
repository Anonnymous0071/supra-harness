#![deny(missing_docs)]
#![allow(dead_code, reason = "startup wiring grows stepwise through T30")]

use std::path::PathBuf;

use supra_config::{Config, ConfigLayer, Loader, PermissionLayer, ThinkingLayer};
use supra_log::{Format, LogHandle, LogOptions};

use crate::args::Cli;

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
    if let Some(mode) = &cli.mode {
        let setting = supra_config::ModeSetting::try_from(mode.clone())
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
    if let Some(format) = &cli.log_format {
        let parsed = match format.as_str() {
            "json" => Format::Json,
            "compact" => Format::Compact,
            other => anyhow::bail!("--log-format must be json or compact, got {other:?}"),
        };
        options = options.with_format(parsed);
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
