//! Layered configuration for supra-harness.
//!
//! **T7** of the stage sequence: discovery, per-field precedence, provenance, and
//! fail-fast validation.
//!
//! # The shape of the problem
//!
//! Configuration goes wrong in three recognisable ways, and each has a structural
//! answer here rather than a convention.
//!
//! **A whole file overwriting another.** Precedence is per *field*, so a project
//! file setting `[cohort] limit` leaves the user's `[thinking] budget` alone. Two
//! types express that: [`ConfigLayer`] is all-`Option` and says only what its file
//! said, while [`Config`] is fully resolved. Collapsing them into one is what makes
//! layered configuration surprising.
//!
//! **A typo that is silently ignored.** `deny_unknown_fields` everywhere, and
//! unknown `SUPRA_CONFIG_*` variables are refused too. A misspelled key that
//! appears to be applied and changes nothing is the archetypal configuration bug.
//!
//! **A value that only fails much later.** Every bound is checked at load, so
//! `[cohort] limit = 0` fails before the first request rather than at cohort spawn.
//! The one exception is named rather than hidden: the per-model minimum reasoning
//! budget is a provider fact T13 owns, and T13 must check it at startup too.
//!
//! # Two properties worth knowing
//!
//! **Resolution cannot fail.** Layers are validated individually, so combining them
//! is a total function - which also means a precedence bug cannot hide behind an
//! error path.
//!
//! **The result is immutable.** [`Config`] has no setter. That is what "the thinking
//! budget is frozen per session" amounts to in practice: not a rule to remember, but
//! the absence of a way to break it.
//!
//! # Two security decisions
//!
//! **No field can hold a credential.** The schema has `api_key_env` and
//! `api_key_keyring` - the *name* of an environment variable or keyring entry - and
//! no field that takes a key. A file that cannot hold a secret cannot leak one. A
//! value that looks like a key rather than a variable name is refused, because
//! otherwise supra would look up an environment variable literally named `sk-...`,
//! report a missing credential, and send the reader looking in the wrong place.
//!
//! **The project layer is not trusted.** A repository is cloned from anywhere, so
//! `.supra/config.toml` may not name a provider, an endpoint, or a credential
//! source, and its permission mode may only make the session **stricter**. Under
//! plain precedence a repository could ship `mode = "yolo"` and get it. See
//! [`resolve`] for the asymmetry and its cost.
//!
//! # Usage
//!
//! ```no_run
//! use supra_config::Loader;
//!
//! let config = Loader::from_environment().load()?;
//! println!("mode: {}", config.permission_mode().label());
//! # Ok::<(), supra_config::ConfigError>(())
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]
// Tests assert with `.expect()` and `panic!`, and report skips on stderr because T8
// supra_log does not exist yet to receive them. Scoped to `cfg(test)` so no allow
// reaches a shipped path.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

pub mod discover;
pub mod error;
pub mod layer;
pub mod resolve;
pub mod source;

use std::path::{Path, PathBuf};

pub use error::ConfigError;
pub use layer::{
    CohortLayer, ConfigLayer, MAX_COMPACTION_THRESHOLD_PERCENT, MIN_COMPACTION_THRESHOLD_PERCENT,
    ModeSetting, PermissionLayer, PromptLayer, ProviderLayer, ThinkingLayer,
};
pub use resolve::{
    Config, DEFAULT_COMPACTION_THRESHOLD_PERCENT, DEFAULT_THINKING_BUDGET, Provider, Setting, resolve,
};
pub use source::ConfigSource;

/// Prefix for configuration environment variables.
///
/// Deliberately narrower than `SUPRA_`: the build already uses `SUPRA_CXX`,
/// `SUPRA_CPP_BUILD_DIR`, and `SUPRA_CMAKE_BUILD_TYPE`, and T4 uses
/// `SUPRA_SANDBOX_*`. Rejecting unknown `SUPRA_*` variables - which fail-fast
/// requires - would then break a developer's own shell. A reserved sub-prefix keeps
/// both properties.
pub const ENV_PREFIX: &str = "SUPRA_CONFIG_";

/// Every accepted environment variable, paired with the setting it sets.
///
/// Exhaustive on purpose: anything under [`ENV_PREFIX`] that is not in this table is
/// refused, so a misspelled variable behaves like a misspelled key rather than being
/// ignored.
pub const ENV_SETTINGS: [(&str, Setting); 4] = [
    ("SUPRA_CONFIG_THINKING_BUDGET", Setting::ThinkingBudget),
    ("SUPRA_CONFIG_COHORT_LIMIT", Setting::CohortLimit),
    ("SUPRA_CONFIG_PERMISSION_MODE", Setting::PermissionMode),
    ("SUPRA_CONFIG_COMPACTION_THRESHOLD_PERCENT", Setting::CompactionThresholdPercent),
];

/// Assembles the layers and resolves them.
///
/// Every input is injectable. That is not only for testing convenience: a loader that
/// reaches for the real environment and the real filesystem internally cannot be
/// exercised without mutating process-global state, and `set_var` is both `unsafe` in
/// this edition and racy across parallel tests. Making the inputs parameters means
/// the precedence rules are tested as arithmetic.
#[derive(Clone, Debug, Default)]
pub struct Loader {
    user_path: Option<PathBuf>,
    project_start: Option<PathBuf>,
    environment: Vec<(String, String)>,
    cli: Option<ConfigLayer>,
    session: Option<ConfigLayer>,
}

impl Loader {
    /// A loader that reads nothing at all.
    ///
    /// Every layer must be supplied explicitly. `load` on an untouched instance
    /// yields the builtin defaults.
    #[must_use]
    pub fn isolated() -> Self {
        Self::default()
    }

    /// A loader wired to the real environment: the user's config file, the nearest
    /// project file above the working directory, and the process environment.
    ///
    /// A working directory that cannot be read leaves the project layer absent rather
    /// than failing: supra is still usable, and a hard failure here would make an
    /// unrelated permission problem look like a configuration error.
    #[must_use]
    pub fn from_environment() -> Self {
        Self {
            user_path: discover::user_config_path(),
            project_start: std::env::current_dir().ok(),
            environment: std::env::vars().filter(|(name, _)| name.starts_with(ENV_PREFIX)).collect(),
            cli: None,
            session: None,
        }
    }

    /// Read the user layer from `path` instead of the discovered location.
    #[must_use]
    pub fn with_user_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.user_path = Some(path.into());
        self
    }

    /// Search for the project layer upward from `directory`.
    #[must_use]
    pub fn with_project_start(mut self, directory: impl Into<PathBuf>) -> Self {
        self.project_start = Some(directory.into());
        self
    }

    /// Use these variables as the environment layer.
    ///
    /// Names outside [`ENV_PREFIX`] are ignored, so a caller may pass a whole
    /// environment without filtering it first.
    #[must_use]
    pub fn with_environment<I, K, V>(mut self, variables: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.environment = variables.into_iter().map(|(name, value)| (name.into(), value.into())).collect();
        self
    }

    /// Supply the layer built from command-line flags.
    #[must_use]
    pub fn with_cli(mut self, layer: ConfigLayer) -> Self {
        self.cli = Some(layer);
        self
    }

    /// Supply the layer set during the session.
    #[must_use]
    pub fn with_session(mut self, layer: ConfigLayer) -> Self {
        self.session = Some(layer);
        self
    }

    /// Read, validate, and resolve every layer.
    ///
    /// # Errors
    ///
    /// Any [`ConfigError`]. Each names the layer, the setting, and the remedy: a
    /// message that says "invalid config" has moved the work to the reader rather
    /// than doing it.
    pub fn load(self) -> Result<Config, ConfigError> {
        let mut layers: Vec<(ConfigSource, ConfigLayer)> = Vec::new();

        if let Some(path) = &self.user_path {
            if let Some(text) = discover::read_private(ConfigSource::User, path)? {
                layers.push((
                    ConfigSource::User,
                    Self::parse_and_validate(&text, ConfigSource::User, path.clone())?,
                ));
            }
        }

        if let Some(start) = &self.project_start {
            if let Some(path) = discover::project_config_path(start) {
                if let Some(text) = discover::read_shared(ConfigSource::Project, &path)? {
                    layers.push((
                        ConfigSource::Project,
                        Self::parse_and_validate(&text, ConfigSource::Project, path)?,
                    ));
                }
            }
        }

        if let Some(layer) = env_layer(&self.environment)? {
            layer.validate(ConfigSource::Env)?;
            layers.push((ConfigSource::Env, layer));
        }

        if let Some(layer) = self.cli {
            layer.validate(ConfigSource::Cli)?;
            layers.push((ConfigSource::Cli, layer));
        }

        if let Some(layer) = self.session {
            layer.validate(ConfigSource::Session)?;
            layers.push((ConfigSource::Session, layer));
        }

        Ok(resolve(&layers))
    }

    fn parse_and_validate(
        text: &str,
        source: ConfigSource,
        path: PathBuf,
    ) -> Result<ConfigLayer, ConfigError> {
        let layer = ConfigLayer::parse(text, source, path)?;
        layer.validate(source)?;
        Ok(layer)
    }
}

/// Build a layer from `SUPRA_CONFIG_*` variables.
///
/// Returns `Ok(None)` when none are set, so an unset environment contributes silence
/// rather than a layer full of defaults.
///
/// # Errors
///
/// [`ConfigError::InvalidEnv`] for an unrecognised variable under [`ENV_PREFIX`] or a
/// value that does not parse.
pub fn env_layer(variables: &[(String, String)]) -> Result<Option<ConfigLayer>, ConfigError> {
    let mut layer = ConfigLayer::default();
    let mut any = false;

    for (name, value) in variables {
        if !name.starts_with(ENV_PREFIX) {
            continue;
        }
        let setting = ENV_SETTINGS
            .iter()
            .find_map(|(candidate, setting)| (candidate == name).then_some(*setting))
            .ok_or_else(|| ConfigError::InvalidEnv {
                variable: name.clone(),
                detail: format!(
                    "unrecognised configuration variable; accepted names are {}",
                    ENV_SETTINGS.iter().map(|(candidate, _)| *candidate).collect::<Vec<&str>>().join(", ")
                ),
            })?;

        any = true;
        apply_env(&mut layer, name, value, setting)?;
    }

    Ok(any.then_some(layer))
}

fn apply_env(layer: &mut ConfigLayer, name: &str, value: &str, setting: Setting) -> Result<(), ConfigError> {
    let invalid = |detail: String| ConfigError::InvalidEnv { variable: name.to_owned(), detail };

    match setting {
        Setting::ThinkingBudget => {
            let budget = value
                .trim()
                .parse::<u32>()
                .map_err(|error| invalid(format!("expected a number of tokens, or 0 to disable: {error}")))?;
            layer.thinking.get_or_insert_with(ThinkingLayer::default).budget = Some(budget);
        }
        Setting::CohortLimit => {
            let limit = value
                .trim()
                .parse::<usize>()
                .map_err(|error| invalid(format!("expected a peer count: {error}")))?;
            layer.cohort.get_or_insert_with(CohortLayer::default).limit = Some(limit);
        }
        Setting::PermissionMode => {
            let mode = ModeSetting::try_from(value.trim().to_owned()).map_err(invalid)?;
            layer.permission.get_or_insert_with(PermissionLayer::default).mode = Some(mode);
        }
        Setting::CompactionThresholdPercent => {
            let percent = value
                .trim()
                .parse::<u8>()
                .map_err(|error| invalid(format!("expected a percentage: {error}")))?;
            layer.prompt.get_or_insert_with(PromptLayer::default).compaction_threshold_percent =
                Some(percent);
        }
    }
    Ok(())
}

/// Where the user's configuration file is, or would be.
///
/// Re-exported at the crate root because `--help` and any "edit my config" affordance
/// need it without knowing about the discovery module.
#[must_use]
pub fn user_config_path() -> Option<PathBuf> {
    discover::user_config_path()
}

/// Where the nearest project configuration file is, if any.
#[must_use]
pub fn project_config_path(start: &Path) -> Option<PathBuf> {
    discover::project_config_path(start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use supra_types::{DEFAULT_PEER_LIMIT, Mode};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("supra-config-load-{name}"));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("scratch directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn write_user(&self, text: &str) -> PathBuf {
            let path = self.0.join("user.toml");
            fs::write(&path, text).expect("write user config");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
            }
            path
        }

        fn write_project(&self, text: &str) -> PathBuf {
            let directory = self.0.join(discover::PROJECT_DIR);
            fs::create_dir_all(&directory).expect("project dir");
            let path = directory.join(discover::CONFIG_FILE);
            fs::write(&path, text).expect("write project config");
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn cli(text: &str) -> ConfigLayer {
        ConfigLayer::parse(text, ConfigSource::Cli, PathBuf::from("<cli>")).expect("fixture")
    }

    #[test]
    fn an_isolated_loader_yields_the_builtin_defaults() {
        let config = Loader::isolated().load().expect("defaults always resolve");
        assert_eq!(config.cohort_limit(), DEFAULT_PEER_LIMIT);
        assert_eq!(config.permission_mode(), Mode::Auto);
        assert_eq!(config.thinking_budget(), DEFAULT_THINKING_BUDGET);
        assert_eq!(config.compaction_threshold_percent(), DEFAULT_COMPACTION_THRESHOLD_PERCENT);
        for setting in Setting::ALL {
            assert_eq!(config.source_of(setting), ConfigSource::Builtin);
        }
    }

    #[test]
    fn all_five_readable_layers_compose() {
        let scratch = Scratch::new("compose");
        let user = scratch.write_user("[thinking]\nbudget = 1024\n[cohort]\nlimit = 4\n");
        scratch.write_project("[permission]\nmode = \"ask\"\n");

        let config = Loader::isolated()
            .with_user_path(user)
            .with_project_start(scratch.path())
            .with_environment([("SUPRA_CONFIG_COHORT_LIMIT", "9")])
            .with_cli(cli("[prompt]\ncompaction_threshold_percent = 95\n"))
            .with_session(cli("[thinking]\nbudget = 4096\n"))
            .load()
            .expect("valid");

        assert_eq!(config.thinking_budget(), 4096, "session beats user");
        assert_eq!(config.source_of(Setting::ThinkingBudget), ConfigSource::Session);

        assert_eq!(config.cohort_limit(), 9, "env beats user");
        assert_eq!(config.source_of(Setting::CohortLimit), ConfigSource::Env);

        assert_eq!(config.permission_mode(), Mode::Ask, "the project tightened");
        assert_eq!(config.source_of(Setting::PermissionMode), ConfigSource::Project);

        assert_eq!(config.compaction_threshold_percent(), 95);
        assert_eq!(config.source_of(Setting::CompactionThresholdPercent), ConfigSource::Cli);
    }

    #[test]
    fn a_missing_user_file_is_not_an_error() {
        let scratch = Scratch::new("no-user");
        let config = Loader::isolated()
            .with_user_path(scratch.path().join("absent.toml"))
            .load()
            .expect("an absent layer is silence");
        assert_eq!(config.cohort_limit(), DEFAULT_PEER_LIMIT);
    }

    #[test]
    #[cfg(unix)]
    fn a_world_readable_user_file_fails_the_load() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = Scratch::new("world-readable");
        let path = scratch.write_user("[cohort]\nlimit = 4\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");

        let error = Loader::isolated().with_user_path(&path).load().expect_err("too permissive");
        assert_eq!(error.layer(), Some(ConfigSource::User));
        assert!(error.to_string().contains("chmod 600"), "{error}");
    }

    #[test]
    fn a_bad_value_in_any_layer_fails_the_load_with_that_layer_named() {
        let scratch = Scratch::new("bad-value");
        scratch.write_project("[cohort]\nlimit = 0\n");

        let error =
            Loader::isolated().with_project_start(scratch.path()).load().expect_err("0 is not a cohort");
        assert_eq!(error.layer(), Some(ConfigSource::Project));
        assert!(error.to_string().contains("cohort.limit"), "{error}");
    }

    #[test]
    fn a_project_file_naming_a_provider_fails_the_load() {
        // The trust boundary, end to end: a cloned repository cannot redirect prompts.
        let scratch = Scratch::new("project-provider");
        scratch.write_project("[providers.exfil]\nendpoint = \"https://attacker.example\"\n");

        let error = Loader::isolated()
            .with_project_start(scratch.path())
            .load()
            .expect_err("a project may not set providers");
        assert_eq!(error.layer(), Some(ConfigSource::Project));
        assert!(error.to_string().contains("where prompts are sent"), "{error}");
    }

    #[test]
    fn an_unset_environment_contributes_nothing() {
        let layer = env_layer(&[]).expect("no variables is fine");
        assert!(layer.is_none(), "silence, not a layer of defaults");
    }

    #[test]
    fn variables_outside_the_prefix_are_ignored() {
        // SUPRA_CXX and SUPRA_CPP_BUILD_DIR are real build variables. Refusing
        // unknown SUPRA_* names would break a developer's own shell, which is why the
        // configuration prefix is narrower.
        let layer = env_layer(&[
            ("SUPRA_CXX".to_owned(), "clang++".to_owned()),
            ("SUPRA_CPP_BUILD_DIR".to_owned(), "/tmp/build".to_owned()),
            ("PATH".to_owned(), "/usr/bin".to_owned()),
        ])
        .expect("unrelated variables are not our business");
        assert!(layer.is_none());
    }

    #[test]
    fn an_unknown_configuration_variable_is_refused_with_the_accepted_names() {
        let error = env_layer(&[("SUPRA_CONFIG_COHORT_LIMTI".to_owned(), "4".to_owned())])
            .expect_err("a typo must not be ignored");
        let text = error.to_string();
        assert!(text.contains("SUPRA_CONFIG_COHORT_LIMTI"), "{text}");
        assert!(text.contains("SUPRA_CONFIG_COHORT_LIMIT"), "the correction: {text}");
        assert_eq!(error.layer(), Some(ConfigSource::Env));
    }

    #[test]
    fn every_declared_variable_reaches_its_setting() {
        // Exhaustive over the table, so a variable that is declared but never wired
        // in cannot pass unnoticed.
        let values = [
            ("SUPRA_CONFIG_THINKING_BUDGET", "2048"),
            ("SUPRA_CONFIG_COHORT_LIMIT", "5"),
            ("SUPRA_CONFIG_PERMISSION_MODE", "plan"),
            ("SUPRA_CONFIG_COMPACTION_THRESHOLD_PERCENT", "94"),
        ];
        assert_eq!(values.len(), ENV_SETTINGS.len(), "the fixture must cover the table");

        for (name, _) in ENV_SETTINGS {
            assert!(values.iter().any(|(candidate, _)| *candidate == name), "{name} untested");
        }

        let config = Loader::isolated().with_environment(values).load().expect("valid");
        assert_eq!(config.thinking_budget(), 2048);
        assert_eq!(config.cohort_limit(), 5);
        assert_eq!(config.permission_mode(), Mode::Plan);
        assert_eq!(config.compaction_threshold_percent(), 94);
        for setting in Setting::ALL {
            assert_eq!(config.source_of(setting), ConfigSource::Env, "{setting:?}");
        }
    }

    #[test]
    fn a_malformed_environment_value_names_the_variable_and_what_was_expected() {
        let error = env_layer(&[("SUPRA_CONFIG_COHORT_LIMIT".to_owned(), "many".to_owned())])
            .expect_err("not a number");
        let text = error.to_string();
        assert!(text.contains("SUPRA_CONFIG_COHORT_LIMIT"), "{text}");
        assert!(text.contains("expected a peer count"), "{text}");

        let error = env_layer(&[("SUPRA_CONFIG_PERMISSION_MODE".to_owned(), "reckless".to_owned())])
            .expect_err("not a mode");
        assert!(error.to_string().contains("plan, ask, auto, yolo"), "{error}");
    }

    #[test]
    fn environment_values_tolerate_surrounding_whitespace() {
        // A shell export or a CI variable file routinely carries a stray space, and
        // failing on it would be a fail-fast that helps nobody.
        let config = Loader::isolated()
            .with_environment([("SUPRA_CONFIG_COHORT_LIMIT", "  7  ")])
            .load()
            .expect("valid");
        assert_eq!(config.cohort_limit(), 7);
    }

    #[test]
    fn an_out_of_range_environment_value_is_still_bounded() {
        // Parsing and validating are separate steps; the second must not be skipped
        // for the environment layer.
        let error = Loader::isolated()
            .with_environment([("SUPRA_CONFIG_COHORT_LIMIT", "999")])
            .load()
            .expect_err("above the ceiling");
        assert_eq!(error.layer(), Some(ConfigSource::Env));
        assert!(error.to_string().contains("hard ceiling"), "{error}");
    }

    #[test]
    fn the_environment_cannot_loosen_what_the_project_tightened() {
        // Env is operator-controlled, so it participates in ordinary precedence - but
        // the project's tightening still applies afterwards.
        let scratch = Scratch::new("env-vs-project");
        scratch.write_project("[permission]\nmode = \"plan\"\n");

        let config = Loader::isolated()
            .with_project_start(scratch.path())
            .with_environment([("SUPRA_CONFIG_PERMISSION_MODE", "yolo")])
            .load()
            .expect("valid");
        assert_eq!(config.permission_mode(), Mode::Plan);
        assert_eq!(config.source_of(Setting::PermissionMode), ConfigSource::Project);
    }

    #[test]
    fn the_crate_root_paths_are_the_discovery_module_paths() {
        // Guards the re-exports against drifting from the discovery module.
        let start = Path::new("/nonexistent-supra-config-probe");
        assert_eq!(project_config_path(start), discover::project_config_path(start));
        assert_eq!(user_config_path(), discover::user_config_path());
    }

    #[test]
    fn a_provider_survives_the_whole_pipeline() {
        let scratch = Scratch::new("provider");
        let user = scratch.write_user(
            "[providers.anthropic]\nendpoint = \"https://api.anthropic.com\"\n\
             model = \"claude\"\napi_key_env = \"ANTHROPIC_API_KEY\"\n",
        );

        let config = Loader::isolated().with_user_path(user).load().expect("valid");
        let provider = config.providers().get("anthropic").expect("configured");
        assert_eq!(provider.endpoint.as_deref(), Some("https://api.anthropic.com"));
        assert_eq!(provider.model.as_deref(), Some("claude"));
        assert_eq!(provider.api_key_env.as_deref(), Some("ANTHROPIC_API_KEY"));
        assert_eq!(provider.api_key_keyring, None);
        assert_eq!(config.provider_source("anthropic"), Some(ConfigSource::User));
    }

    #[test]
    fn a_credential_in_the_user_file_is_refused_end_to_end() {
        let scratch = Scratch::new("literal-key");
        let user = scratch.write_user("[providers.p]\napi_key = \"sk-would-be-a-real-key\"\n");

        let error = Loader::isolated().with_user_path(user).load().expect_err("refused");
        let text = error.to_string();
        assert!(text.contains("never reads a literal credential"), "{text}");
        assert!(!text.contains("would-be-a-real-key"), "the value must not be echoed: {text}");
    }
}
