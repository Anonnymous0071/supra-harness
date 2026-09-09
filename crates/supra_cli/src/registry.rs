#![deny(missing_docs)]
#![allow(dead_code, reason = "registry wiring grows stepwise through T30")]

use std::path::PathBuf;

use supra_config::Config;

pub(crate) struct Wired {
    pub(crate) commands: supra_command::Registry,
    pub(crate) hooks: supra_hook::Registry,
    pub(crate) session_dir: PathBuf,
    pub(crate) consent: Option<supra_telemetry::Consent>,
    pub(crate) theme: supra_theme::Theme,
}

pub(crate) fn state_dir() -> Option<PathBuf> {
    match std::env::var_os("XDG_STATE_HOME") {
        Some(value) if !value.is_empty() && std::path::Path::new(&value).is_absolute() => {
            Some(PathBuf::from(value))
        }
        _ => std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(|home| PathBuf::from(home).join(".local").join("state")),
    }
}

pub(crate) fn session_dir() -> Option<PathBuf> {
    state_dir().map(|base| base.join("supra").join("sessions"))
}

pub(crate) fn wire(_config: &Config) -> Wired {
    let commands = supra_command::Registry::with_builtins();
    let hooks = supra_hook::Registry::new();
    let session_dir = session_dir().unwrap_or_else(|| PathBuf::from(".supra").join("sessions"));
    let consent = state_dir().and_then(|dir| supra_telemetry::Consent::read(&dir).unwrap_or(None));
    let theme = supra_theme::Theme::default_dark();
    Wired { commands, hooks, session_dir, consent, theme }
}

pub(crate) fn register_hook(
    registry: &mut supra_hook::Registry,
    point: &str,
    command: &str,
) -> Result<(), supra_hook::HookError> {
    registry.register_named(point, command)
}
