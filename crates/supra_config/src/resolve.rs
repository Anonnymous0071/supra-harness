//! Per-field precedence, with provenance, and one asymmetry.
//!
//! # Resolution is infallible
//!
//! Every layer is validated before it arrives here, so this module cannot fail. That
//! is a deliberate split: everything that *can* be wrong about a value is wrong about
//! it in isolation, and combining valid layers is a total function. It also means a
//! precedence bug cannot hide behind an error path.
//!
//! # Per field, not per file
//!
//! Layers are walked in ascending precedence and the last one that *says something*
//! wins that field. A project file setting only `[cohort] limit` leaves the user's
//! `[thinking] budget` untouched, because a silent field is silence rather than a
//! default.
//!
//! # The one asymmetry
//!
//! Plain precedence would let the project layer *raise* permissiveness: a cloned
//! repository could ship `[permission] mode = "yolo"` and get it, because `project`
//! outranks `user`. So the permission mode has an extra rule - a layer that is not
//! operator-controlled may only make the session **stricter**.
//!
//! This mirrors the permission-rule decision in section 6, where a project `Deny`
//! survives a session `Allow`. The costs are asymmetric in the same way: a repository
//! that can tighten is an inconvenience with a documented remedy, while a repository
//! that can loosen is a vulnerability.
//!
//! The consequence is real and worth stating plainly: a project pinning `ask` beats
//! an explicit `--yolo` on the command line. An operator who wants to overrule the
//! repository needs a flag that says so - `--ignore-project-config`, which belongs to
//! T30 with its own confirmation, exactly as `--sandbox off` does.

use std::collections::BTreeMap;

use supra_types::{DEFAULT_PEER_LIMIT, Mode};

use crate::error::ConfigError;
use crate::layer::ConfigLayer;
use crate::source::ConfigSource;

/// Default reasoning budget: reasoning off.
///
/// Off rather than on because the budget is frozen for the session and billed as
/// output. A user who wants reasoning asks for it; one who never mentions it should
/// not discover it on an invoice.
pub const DEFAULT_THINKING_BUDGET: u32 = 0;

/// Default compaction threshold, inside the 92-95 band invariant I4 sets.
///
/// The middle of the band rather than either edge: 92 pays for an avoidable rewrite
/// sooner than necessary, and 95 leaves little room to finish a turn once the
/// decision is taken.
pub const DEFAULT_COMPACTION_THRESHOLD_PERCENT: u8 = 93;

/// A scalar setting, for provenance lookups.
///
/// Providers are addressed by name instead; see [`Config::provider_source`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Setting {
    /// `thinking.budget`
    ThinkingBudget,
    /// `cohort.limit`
    CohortLimit,
    /// `permission.mode`
    PermissionMode,
    /// `prompt.compaction_threshold_percent`
    CompactionThresholdPercent,
}

impl Setting {
    /// Every scalar setting.
    pub const ALL: [Self; 4] =
        [Self::ThinkingBudget, Self::CohortLimit, Self::PermissionMode, Self::CompactionThresholdPercent];

    /// Dotted path, as written in a file.
    #[must_use]
    pub const fn path(self) -> &'static str {
        match self {
            Self::ThinkingBudget => "thinking.budget",
            Self::CohortLimit => "cohort.limit",
            Self::PermissionMode => "permission.mode",
            Self::CompactionThresholdPercent => "prompt.compaction_threshold_percent",
        }
    }
}

/// One resolved provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provider {
    /// Base URL, if the configuration pinned one.
    pub endpoint: Option<String>,
    /// Model identifier, if the configuration pinned one.
    pub model: Option<String>,
    /// Name of the environment variable holding the credential.
    pub api_key_env: Option<String>,
    /// Name of the OS keyring entry holding the credential.
    pub api_key_keyring: Option<String>,
}

/// The resolved configuration for a session.
///
/// Immutable by construction. There is no setter and no `&mut` accessor, which is
/// what "the thinking budget is frozen per session" means in practice: not a rule
/// someone has to remember, but the absence of a way to break it. A different budget
/// means a new session, and the type says so.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    thinking_budget: u32,
    cohort_limit: usize,
    permission_mode: Mode,
    compaction_threshold_percent: u8,
    providers: BTreeMap<String, Provider>,
    scalar_sources: [(Setting, ConfigSource); 4],
    provider_sources: BTreeMap<String, ConfigSource>,
}

impl Config {
    /// Reasoning tokens per turn, or 0 when reasoning is off.
    #[must_use]
    pub const fn thinking_budget(&self) -> u32 {
        self.thinking_budget
    }

    /// Largest cohort any tier may field.
    #[must_use]
    pub const fn cohort_limit(&self) -> usize {
        self.cohort_limit
    }

    /// Consent mode for this session.
    #[must_use]
    pub const fn permission_mode(&self) -> Mode {
        self.permission_mode
    }

    /// Context usage at which a new generation is written.
    #[must_use]
    pub const fn compaction_threshold_percent(&self) -> u8 {
        self.compaction_threshold_percent
    }

    /// Configured providers, in name order.
    #[must_use]
    pub const fn providers(&self) -> &BTreeMap<String, Provider> {
        &self.providers
    }

    /// Which layer supplied a scalar setting.
    ///
    /// Always answers: the builtin layer supplies every setting, so resolution is
    /// total. This is what lets `/config` say *why* a value is what it is, which is
    /// the question a user actually has when a setting appears to be ignored.
    #[must_use]
    pub fn source_of(&self, setting: Setting) -> ConfigSource {
        self.scalar_sources
            .iter()
            .find_map(|(candidate, source)| (*candidate == setting).then_some(*source))
            .unwrap_or(ConfigSource::Builtin)
    }

    /// Which layer introduced a provider, if it is configured at all.
    #[must_use]
    pub fn provider_source(&self, name: &str) -> Option<ConfigSource> {
        self.provider_sources.get(name).copied()
    }

    /// Resolve one provider's credential to a [`supra_secrets::SecretString`].
    ///
    /// The resolution ladder mirrors T12's: `api_key_env` is read from the process environment,
    /// `api_key_keyring` is read through [`supra_secrets::SecretManager`], which tries the OS keyring and then
    /// the encrypted file. T7's validation already guarantees exactly one source is named, so
    /// this function chooses rather than prioritises - and that is why it cannot fail on
    /// ambiguity. It fails only when the named source has nothing to give.
    ///
    /// The result is a [`supra_secrets::SecretString`], not a `String`: the caller receives a value whose
    /// `Debug` and `Display` reveal nothing, so the credential cannot leak through a diagnostic
    /// between here and the HTTP layer. T8's redactor is the net beneath, not the mechanism.
    ///
    /// `manager` is a parameter rather than constructed here because probing the OS keyring is
    /// I/O, and resolution is a pure function over already-read layers - which is what makes
    /// the precedence rules testable without a filesystem, a keyring, or a passphrase. T13
    /// opens one manager per session and threads it through.
    ///
    /// # Errors
    ///
    /// [`ConfigError::MissingCredential`] when the named environment variable is unset or the
    /// secrets ladder has no such entry. The message names the source *by name* - never the
    /// value - and says where to put the credential instead.
    pub fn provider_secret(
        &self,
        name: &str,
        manager: &supra_secrets::SecretManager,
    ) -> Result<supra_secrets::SecretString, ConfigError> {
        let provider = self.providers.get(name).ok_or_else(|| ConfigError::MissingCredential {
            provider: name.to_owned(),
            detail: "no such provider is configured".to_owned(),
        })?;

        if let Some(variable) = &provider.api_key_env {
            match std::env::var(variable) {
                Ok(value) if !value.is_empty() => {
                    return Ok(supra_secrets::SecretString::new(value));
                }
                Ok(_) => {
                    return Err(ConfigError::MissingCredential {
                        provider: name.to_owned(),
                        detail: format!(
                            "environment variable {variable:?} is set but empty; unset it or give \
                             it a value"
                        ),
                    });
                }
                Err(_) => {
                    return Err(ConfigError::MissingCredential {
                        provider: name.to_owned(),
                        detail: format!(
                            "environment variable {variable:?} is not set; export it or point the \
                             provider at a keyring entry with api_key_keyring"
                        ),
                    });
                }
            }
        }

        if let Some(entry) = &provider.api_key_keyring {
            // The keyring entry name doubles as the account: T7 validates it is non-empty, and
            // a distinct service keeps supra's entries from colliding with another
            // application's. The full `service/account` pair is reported on failure so the user
            // can look the entry up in their keyring manager.
            return manager
                .get(crate::credential::KEYRING_SERVICE, entry)
                .map(supra_secrets::SecretString::new)
                .map_err(|error| ConfigError::MissingCredential {
                    provider: name.to_owned(),
                    detail: format!("keyring entry {entry:?}: {error}"),
                });
        }

        Err(ConfigError::MissingCredential {
            provider: name.to_owned(),
            detail: "the provider names no credential source; set api_key_env or api_key_keyring".to_owned(),
        })
    }
}

/// Combine validated layers into one configuration.
///
/// `layers` may arrive in any order and need not be complete; each is paired with the
/// source it came from. Ties cannot occur because [`ConfigSource`] is totally ordered
/// and each source appears at most once in a load.
///
/// Every layer must already have passed [`ConfigLayer::validate`]. Passing an
/// unvalidated layer does not corrupt anything - the result is simply a value the
/// caller was supposed to have refused - which is why loading always validates first.
#[must_use]
pub fn resolve(layers: &[(ConfigSource, ConfigLayer)]) -> Config {
    let mut ordered: Vec<&(ConfigSource, ConfigLayer)> = layers.iter().collect();
    ordered.sort_by_key(|(source, _)| *source);

    let mut thinking_budget = (DEFAULT_THINKING_BUDGET, ConfigSource::Builtin);
    let mut cohort_limit = (DEFAULT_PEER_LIMIT, ConfigSource::Builtin);
    let mut permission_mode = (Mode::default(), ConfigSource::Builtin);
    let mut threshold = (DEFAULT_COMPACTION_THRESHOLD_PERCENT, ConfigSource::Builtin);

    let mut providers: BTreeMap<String, Provider> = BTreeMap::new();
    let mut provider_sources: BTreeMap<String, ConfigSource> = BTreeMap::new();

    for (source, layer) in &ordered {
        if let Some(budget) = layer.thinking.and_then(|thinking| thinking.budget) {
            thinking_budget = (budget, *source);
        }
        if let Some(limit) = layer.cohort.and_then(|cohort| cohort.limit) {
            cohort_limit = (limit, *source);
        }
        if let Some(mode) = layer.permission.and_then(|permission| permission.mode) {
            // Only an operator-controlled layer participates in ordinary precedence
            // for the mode. A layer that is not operator-controlled is handled by the
            // tightening pass below, and must be skipped here: applying it now and
            // narrowing afterwards would leave a loosened value in place, because a
            // pass that can only tighten cannot undo a loosening.
            if source.is_operator_controlled() {
                permission_mode = (mode.mode(), *source);
            }
        }
        if let Some(percent) = layer.prompt.and_then(|prompt| prompt.compaction_threshold_percent) {
            threshold = (percent, *source);
        }
        if let Some(entries) = &layer.providers {
            for (name, entry) in entries {
                // A later layer replaces a provider wholesale rather than merging
                // field by field. Half of one endpoint and half of another is not a
                // provider anyone configured, and a partial merge is how a
                // credential ends up paired with the wrong host.
                providers.insert(
                    name.clone(),
                    Provider {
                        endpoint: entry.endpoint.clone(),
                        model: entry.model.clone(),
                        api_key_env: entry.api_key_env.clone(),
                        api_key_keyring: entry.api_key_keyring.clone(),
                    },
                );
                provider_sources.insert(name.clone(), *source);
            }
        }
    }

    // The asymmetry. Applied after normal precedence so it can only ever narrow the
    // outcome, and attributed to the layer that narrowed it.
    for (source, layer) in &ordered {
        if source.is_operator_controlled() {
            continue;
        }
        if let Some(requested) = layer.permission.and_then(|permission| permission.mode) {
            let requested = requested.mode();
            if strictness(requested) > strictness(permission_mode.0) {
                permission_mode = (requested, *source);
            }
        }
    }

    Config {
        thinking_budget: thinking_budget.0,
        cohort_limit: cohort_limit.0,
        permission_mode: permission_mode.0,
        compaction_threshold_percent: threshold.0,
        providers,
        scalar_sources: [
            (Setting::ThinkingBudget, thinking_budget.1),
            (Setting::CohortLimit, cohort_limit.1),
            (Setting::PermissionMode, permission_mode.1),
            (Setting::CompactionThresholdPercent, threshold.1),
        ],
        provider_sources,
    }
}

/// How restrictive a mode is; higher is stricter.
///
/// Derived from the position of the mode in [`Mode::ALL`], which T6 documents as
/// ordered from most to least restrictive and pins with a test. Reading the order
/// from there rather than restating it here means the two cannot disagree.
fn strictness(mode: Mode) -> usize {
    let last = Mode::ALL.len() - 1;
    Mode::ALL.iter().position(|candidate| *candidate == mode).map_or(0, |index| last - index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer::{
        CohortLayer, MAX_COMPACTION_THRESHOLD_PERCENT, MIN_COMPACTION_THRESHOLD_PERCENT, PermissionLayer,
        PromptLayer, ThinkingLayer,
    };
    use std::path::PathBuf;

    fn layer(text: &str) -> ConfigLayer {
        ConfigLayer::parse(text, ConfigSource::User, PathBuf::from("/test/config.toml"))
            .expect("the fixture must parse")
    }

    /// Set the process environment for one test body, serialised against every other test in
    /// this module. `set_var`/`remove_var` are `unsafe` in the current toolchain; the module
    /// lock is what makes concurrent use sound. Same pattern as T12's `ENV_GUARD`.
    static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[allow(
        clippy::undocumented_unsafe_blocks,
        reason = "serialised by the module lock; see the doc comment on ENV_GUARD"
    )]
    fn with_env(var: &str, value: &str, body: impl FnOnce()) {
        let _lock = ENV_GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // SAFETY: under the module lock, and no test thread reads the variable outside it.
        unsafe {
            std::env::set_var(var, value);
            body();
            std::env::remove_var(var);
        }
    }

    /// A manager whose file rung points at a scratch vault, so no test touches a real keyring
    /// or the user's fallback file - even on a machine where the OS keyring answers.
    fn scratch_manager(name: &str) -> (supra_secrets::SecretManager, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!("supra-config-secret-{name}.enc"));
        let _ = std::fs::remove_file(&path);
        (supra_secrets::SecretManager::open_with_file_store(path.clone()), path)
    }

    #[test]
    fn with_no_layers_every_setting_comes_from_the_builtin_defaults() {
        let config = resolve(&[]);
        assert_eq!(config.thinking_budget(), DEFAULT_THINKING_BUDGET);
        assert_eq!(config.cohort_limit(), DEFAULT_PEER_LIMIT);
        assert_eq!(config.permission_mode(), Mode::Auto);
        assert_eq!(config.compaction_threshold_percent(), DEFAULT_COMPACTION_THRESHOLD_PERCENT);
        assert!(config.providers().is_empty());

        for setting in Setting::ALL {
            assert_eq!(config.source_of(setting), ConfigSource::Builtin, "{setting:?}");
        }
    }

    #[test]
    fn the_defaults_are_the_documented_ones() {
        // Reasoning off by default because the budget is frozen and billed as output.
        assert_eq!(DEFAULT_THINKING_BUDGET, 0);
        // The peer limit default comes from T6, not from a second copy of the number.
        assert_eq!(DEFAULT_PEER_LIMIT, 16);
        // Auto, which is only defensible because the journal exists.
        assert_eq!(Mode::default(), Mode::Auto);
        // Inside I4's band.
        assert!(
            (MIN_COMPACTION_THRESHOLD_PERCENT..=MAX_COMPACTION_THRESHOLD_PERCENT)
                .contains(&DEFAULT_COMPACTION_THRESHOLD_PERCENT)
        );
    }

    #[test]
    fn a_higher_layer_wins_one_field_without_touching_the_others() {
        // The property that makes layering usable: a project file that mentions one
        // setting must not silently reset the rest.
        let user = layer("[cohort]\nlimit = 4\n[thinking]\nbudget = 2048\n");
        let project = layer("[cohort]\nlimit = 8\n");

        let config = resolve(&[(ConfigSource::User, user), (ConfigSource::Project, project)]);
        assert_eq!(config.cohort_limit(), 8);
        assert_eq!(config.source_of(Setting::CohortLimit), ConfigSource::Project);

        assert_eq!(config.thinking_budget(), 2048, "the user's budget must survive");
        assert_eq!(config.source_of(Setting::ThinkingBudget), ConfigSource::User);
    }

    #[test]
    fn layers_resolve_in_precedence_order_whatever_order_they_arrive_in() {
        // Callers assemble layers as they discover them; the result must not depend on
        // that discovery order.
        let build = |sources: Vec<ConfigSource>| {
            let layers: Vec<(ConfigSource, ConfigLayer)> = sources
                .into_iter()
                .enumerate()
                .map(|(index, source)| (source, layer(&format!("[cohort]\nlimit = {}\n", index + 1))))
                .collect();
            resolve(&layers)
        };

        // Session is the highest precedence, so it wins regardless of position.
        let forward = build(vec![ConfigSource::User, ConfigSource::Session]);
        assert_eq!(forward.source_of(Setting::CohortLimit), ConfigSource::Session);
        assert_eq!(forward.cohort_limit(), 2);

        let reversed = build(vec![ConfigSource::Session, ConfigSource::User]);
        assert_eq!(reversed.source_of(Setting::CohortLimit), ConfigSource::Session);
        assert_eq!(reversed.cohort_limit(), 1);
    }

    #[test]
    fn every_source_can_win_a_field() {
        // Walks the whole ladder: each source in turn, beating everything below it.
        for (index, winner) in ConfigSource::ALL.into_iter().enumerate() {
            let layers: Vec<(ConfigSource, ConfigLayer)> = ConfigSource::ALL[..=index]
                .iter()
                .map(|source| {
                    let limit = usize::from(*source == winner) + 1;
                    (*source, layer(&format!("[cohort]\nlimit = {limit}\n")))
                })
                .collect();
            let config = resolve(&layers);
            assert_eq!(config.source_of(Setting::CohortLimit), winner, "{winner:?}");
        }
    }

    #[test]
    fn a_project_layer_may_tighten_the_mode() {
        // A repository asking for more care than the user's default is honoured.
        let user = layer("[permission]\nmode = \"auto\"\n");
        let project = layer("[permission]\nmode = \"ask\"\n");
        let config = resolve(&[(ConfigSource::User, user), (ConfigSource::Project, project)]);
        assert_eq!(config.permission_mode(), Mode::Ask);
        assert_eq!(config.source_of(Setting::PermissionMode), ConfigSource::Project);
    }

    #[test]
    fn a_project_layer_may_not_loosen_the_mode() {
        // The vulnerability this closes: a cloned repository shipping
        // `mode = "yolo"` outranks the user's file under plain precedence.
        let user = layer("[permission]\nmode = \"auto\"\n");
        let project = layer("[permission]\nmode = \"yolo\"\n");
        let config = resolve(&[(ConfigSource::User, user), (ConfigSource::Project, project)]);
        assert_eq!(config.permission_mode(), Mode::Auto, "the repository must not win");
        assert_eq!(config.source_of(Setting::PermissionMode), ConfigSource::User);
    }

    #[test]
    fn a_project_layer_cannot_loosen_even_against_the_builtin_default() {
        let project = layer("[permission]\nmode = \"yolo\"\n");
        let config = resolve(&[(ConfigSource::Project, project)]);
        assert_eq!(config.permission_mode(), Mode::Auto);
        assert_eq!(config.source_of(Setting::PermissionMode), ConfigSource::Builtin);
    }

    #[test]
    fn project_tightening_survives_an_explicit_cli_loosening() {
        // Stated as a test because it is surprising and deliberate. It follows the
        // same rule as a project `Deny` surviving a session `Allow`; the operator's
        // remedy is a flag that says "ignore the project", which T30 owns.
        let cli = layer("[permission]\nmode = \"yolo\"\n");
        let project = layer("[permission]\nmode = \"plan\"\n");
        let config = resolve(&[(ConfigSource::Cli, cli), (ConfigSource::Project, project)]);
        assert_eq!(config.permission_mode(), Mode::Plan);
        assert_eq!(config.source_of(Setting::PermissionMode), ConfigSource::Project);
    }

    #[test]
    fn the_tightening_rule_never_makes_a_session_more_permissive() {
        // Exhaustive over three axes: which operator layer states the mode, what it
        // states, and what the project requests. The operator layer must include
        // `User`, which sits *below* `Project` in precedence - that is the case an
        // earlier version of this test missed, and the case where the first
        // implementation was wrong: it applied the project's mode in the ordinary
        // precedence pass, and a pass that can only tighten cannot undo a loosening.
        let operator_layers: Vec<ConfigSource> =
            ConfigSource::ALL.into_iter().filter(|s| s.is_operator_controlled()).collect();

        for operator_source in operator_layers {
            for operator in Mode::ALL {
                for requested in Mode::ALL {
                    let operator_layer = layer(&format!("[permission]\nmode = \"{}\"\n", operator.label()));
                    let project_layer = layer(&format!("[permission]\nmode = \"{}\"\n", requested.label()));
                    let config =
                        resolve(&[(operator_source, operator_layer), (ConfigSource::Project, project_layer)]);
                    assert!(
                        strictness(config.permission_mode()) >= strictness(operator),
                        "{operator_source:?} {operator:?} + project {requested:?} produced {:?}",
                        config.permission_mode()
                    );
                }
            }
        }
    }

    #[test]
    fn the_project_layer_wins_exactly_when_it_is_stricter() {
        // The other half of the rule: tightening must actually take effect, or the
        // asymmetry would collapse into "the project layer is ignored".
        for operator in Mode::ALL {
            for requested in Mode::ALL {
                let config = resolve(&[
                    (ConfigSource::User, layer(&format!("[permission]\nmode = \"{}\"\n", operator.label()))),
                    (
                        ConfigSource::Project,
                        layer(&format!("[permission]\nmode = \"{}\"\n", requested.label())),
                    ),
                ]);

                let expected =
                    if strictness(requested) > strictness(operator) { requested } else { operator };
                assert_eq!(
                    config.permission_mode(),
                    expected,
                    "operator {operator:?} + project {requested:?}"
                );

                let expected_source = if strictness(requested) > strictness(operator) {
                    ConfigSource::Project
                } else {
                    ConfigSource::User
                };
                assert_eq!(config.source_of(Setting::PermissionMode), expected_source);
            }
        }
    }

    #[test]
    fn strictness_orders_the_modes_as_the_mode_type_documents() {
        // Mode::ALL is documented as most to least restrictive, so strictness must
        // decrease along it. If T6 reorders, this fails rather than silently
        // inverting the tightening rule.
        let ranks: Vec<usize> = Mode::ALL.iter().map(|mode| strictness(*mode)).collect();
        assert_eq!(ranks, vec![3, 2, 1, 0]);
        assert!(strictness(Mode::Plan) > strictness(Mode::Ask));
        assert!(strictness(Mode::Ask) > strictness(Mode::Auto));
        assert!(strictness(Mode::Auto) > strictness(Mode::Yolo));
    }

    #[test]
    fn a_provider_is_replaced_wholesale_not_merged() {
        // A partial merge is how a credential ends up paired with the wrong host.
        let user = layer("[providers.p]\nendpoint = \"https://user.example\"\napi_key_env = \"USER_KEY\"\n");
        let cli = layer("[providers.p]\nendpoint = \"https://cli.example\"\n");

        let config = resolve(&[(ConfigSource::User, user), (ConfigSource::Cli, cli)]);
        let provider = config.providers().get("p").expect("configured");
        assert_eq!(provider.endpoint.as_deref(), Some("https://cli.example"));
        assert_eq!(
            provider.api_key_env, None,
            "the user's credential must not be carried onto a different endpoint"
        );
        assert_eq!(config.provider_source("p"), Some(ConfigSource::Cli));
    }

    #[test]
    fn providers_from_different_layers_coexist() {
        let user = layer("[providers.a]\nmodel = \"m1\"\n");
        let cli = layer("[providers.b]\nmodel = \"m2\"\n");
        let config = resolve(&[(ConfigSource::User, user), (ConfigSource::Cli, cli)]);

        let names: Vec<&String> = config.providers().keys().collect();
        assert_eq!(names, vec!["a", "b"]);
        assert_eq!(config.provider_source("a"), Some(ConfigSource::User));
        assert_eq!(config.provider_source("b"), Some(ConfigSource::Cli));
        assert_eq!(config.provider_source("absent"), None);
    }

    #[test]
    fn provenance_answers_for_every_scalar_setting() {
        let session = layer(
            "[thinking]\nbudget = 4096\n[cohort]\nlimit = 3\n[permission]\nmode = \"ask\"\n\
             [prompt]\ncompaction_threshold_percent = 94\n",
        );
        let config = resolve(&[(ConfigSource::Session, session)]);
        for setting in Setting::ALL {
            assert_eq!(config.source_of(setting), ConfigSource::Session, "{setting:?}");
        }
        assert_eq!(config.thinking_budget(), 4096);
        assert_eq!(config.cohort_limit(), 3);
        assert_eq!(config.permission_mode(), Mode::Ask);
        assert_eq!(config.compaction_threshold_percent(), 94);
    }

    #[test]
    fn setting_paths_are_the_spellings_used_in_files() {
        let paths: Vec<&str> = Setting::ALL.iter().map(|setting| setting.path()).collect();
        assert_eq!(
            paths,
            vec!["thinking.budget", "cohort.limit", "permission.mode", "prompt.compaction_threshold_percent",]
        );
    }

    #[test]
    fn an_explicit_zero_budget_is_a_choice_not_a_silence() {
        // 0 disables reasoning, and a layer that sets it must be recorded as having
        // done so - otherwise `/config` cannot distinguish "off by default" from "off
        // because the project asked".
        let project = layer("[thinking]\nbudget = 0\n");
        let user = layer("[thinking]\nbudget = 8192\n");
        let config = resolve(&[(ConfigSource::User, user), (ConfigSource::Project, project)]);
        assert_eq!(config.thinking_budget(), 0);
        assert_eq!(config.source_of(Setting::ThinkingBudget), ConfigSource::Project);
    }

    #[test]
    fn a_config_has_no_way_to_be_mutated() {
        // The structural half of "the thinking budget is frozen per session": the
        // value can be read and cloned, and there is no setter to find.
        let config = resolve(&[(ConfigSource::User, layer("[thinking]\nbudget = 1024\n"))]);
        let copy = config.clone();
        assert_eq!(copy, config);
        assert_eq!(copy.thinking_budget(), 1024);
    }

    #[test]
    fn empty_layers_are_indistinguishable_from_absent_ones() {
        let with_empties = resolve(&[
            (ConfigSource::User, ConfigLayer::default()),
            (ConfigSource::Project, layer("[cohort]\n")),
        ]);
        assert_eq!(with_empties, resolve(&[]));
    }

    // ------------------------------------------------------------------
    // Credential resolution: `provider_secret`
    // ------------------------------------------------------------------
    //
    // These tests need a manager, but they must never touch the machine's real keyring or
    // the user's fallback file. Every one opens a scratch manager - and skips its body where
    // the OS keyring answers, because there the writes below would land in real credentials.
    // On a headless machine the file rung is primary and the full ladder is exercised.

    #[test]
    fn an_env_credential_resolves_to_a_secret_that_reveals_nothing() {
        let config = resolve(&[(
            ConfigSource::User,
            layer("[providers.p]\napi_key_env = \"SUPRA_TEST_RESOLVE_KEY\"\n"),
        )]);
        let (manager, _path) = scratch_manager("env-resolves");

        with_env("SUPRA_TEST_RESOLVE_KEY", "sk-test-value-123", || {
            let secret = config.provider_secret("p", &manager).expect("the variable is set");
            assert_eq!(secret.expose(), "sk-test-value-123");
            assert_eq!(format!("{secret:?}"), "Secret { value: \"[REDACTED]\" }");
            assert_eq!(format!("{secret}"), "[REDACTED]");
        });
    }

    #[test]
    fn an_unset_env_variable_is_a_missing_credential_naming_the_variable() {
        let config = resolve(&[(
            ConfigSource::User,
            layer("[providers.p]\napi_key_env = \"SUPRA_TEST_RESOLVE_ABSENT\"\n"),
        )]);
        let (manager, path) = scratch_manager("env-absent");
        let _ = std::fs::remove_file(&path);

        // Belt and suspenders: the variable must be absent, not merely unset by convention.
        // SAFETY: under the module lock; see `with_env`.
        #[allow(
            clippy::undocumented_unsafe_blocks,
            reason = "serialised by the module lock; see the doc comment on ENV_GUARD"
        )]
        let _lock = ENV_GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        unsafe {
            std::env::remove_var("SUPRA_TEST_RESOLVE_ABSENT");
        }
        let error = config.provider_secret("p", &manager).expect_err("nothing is set");
        let text = error.to_string();
        assert!(matches!(error, crate::error::ConfigError::MissingCredential { .. }), "{text}");
        assert!(text.contains("\"p\""), "the provider: {text}");
        assert!(text.contains("SUPRA_TEST_RESOLVE_ABSENT"), "the variable by name: {text}");
        assert!(!text.contains("sk-"), "no value to leak, but assert the shape: {text}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_empty_env_variable_is_missing_not_empty() {
        // An empty credential would authenticate against nothing and fail at the provider with
        // an opaque 401. Failing here, naming the variable, is the actionable outcome.
        let config = resolve(&[(
            ConfigSource::User,
            layer("[providers.p]\napi_key_env = \"SUPRA_TEST_RESOLVE_EMPTY\"\n"),
        )]);
        let (manager, path) = scratch_manager("env-empty");
        let _ = std::fs::remove_file(&path);

        with_env("SUPRA_TEST_RESOLVE_EMPTY", "", || {
            let error = config.provider_secret("p", &manager).expect_err("empty is missing");
            let text = error.to_string();
            assert!(matches!(error, crate::error::ConfigError::MissingCredential { .. }), "{text}");
            assert!(text.contains("empty"), "{text}");
        });
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_keyring_credential_resolves_through_the_fallback_file() {
        // The full T12 ladder behind one call: config names the entry, the manager reads it
        // from the vault. Gated on the file rung being primary so the test never touches real
        // credentials on a keyring machine.
        let config =
            resolve(&[(ConfigSource::User, layer("[providers.p]\napi_key_keyring = \"test-entry\"\n"))]);
        let (manager, path) = scratch_manager("keyring-resolves");
        let _ = std::fs::remove_file(&path);
        if manager.primary_backend() != supra_secrets::Backend::EncryptedFile {
            return;
        }

        with_env(supra_secrets::file_store::MASTER_KEY_ENV, "resolve-test-key", || {
            manager.set(crate::KEYRING_SERVICE, "test-entry", "sk-keyring-value").expect("seed");
            let secret = config.provider_secret("p", &manager).expect("the entry exists");
            assert_eq!(secret.expose(), "sk-keyring-value");
            assert_eq!(format!("{secret:?}"), "Secret { value: \"[REDACTED]\" }");
        });
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_keyring_entry_is_a_missing_credential_naming_the_entry() {
        let config =
            resolve(&[(ConfigSource::User, layer("[providers.p]\napi_key_keyring = \"absent-entry\"\n"))]);
        let (manager, path) = scratch_manager("keyring-absent");
        let _ = std::fs::remove_file(&path);
        if manager.primary_backend() != supra_secrets::Backend::EncryptedFile {
            return;
        }

        with_env(supra_secrets::file_store::MASTER_KEY_ENV, "resolve-test-key", || {
            // Seed an unrelated entry so the vault exists: without it the manager reports
            // NoStore (nothing configured) rather than NotFound (configured, entry absent),
            // and the test would assert the wrong distinction.
            manager.set(crate::KEYRING_SERVICE, "other-entry", "value").expect("seed");
            let error = config.provider_secret("p", &manager).expect_err("no such entry");
            let text = error.to_string();
            assert!(matches!(error, crate::error::ConfigError::MissingCredential { .. }), "{text}");
            assert!(text.contains("absent-entry"), "the entry by name: {text}");
        });
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_provider_with_no_credential_source_is_missing() {
        // T7's validation already refuses a provider naming both sources; a provider naming
        // neither passes validation (it may gain one from a higher layer) and fails here.
        let config = resolve(&[(ConfigSource::User, layer("[providers.p]\nmodel = \"m\"\n"))]);
        let (manager, path) = scratch_manager("no-source");
        let _ = std::fs::remove_file(&path);

        let error = config.provider_secret("p", &manager).expect_err("no source named");
        let text = error.to_string();
        assert!(matches!(error, crate::error::ConfigError::MissingCredential { .. }), "{text}");
        assert!(text.contains("api_key_env"), "the remedy names both sources: {text}");
        assert!(text.contains("api_key_keyring"), "the remedy names both sources: {text}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_unknown_provider_is_missing_by_name() {
        let config = resolve(&[]);
        let (manager, path) = scratch_manager("unknown-provider");
        let _ = std::fs::remove_file(&path);

        let error = config.provider_secret("ghost", &manager).expect_err("no such provider");
        let text = error.to_string();
        assert!(matches!(error, crate::error::ConfigError::MissingCredential { .. }), "{text}");
        assert!(text.contains("\"ghost\""), "{text}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_credential_carries_no_layer_and_no_path() {
        // The provider may combine several layers and the failure is about the world, not any
        // one file - so attribution is `None`, and callers use `provider_source()` instead.
        let error = crate::error::ConfigError::MissingCredential {
            provider: "p".to_owned(),
            detail: "nothing".to_owned(),
        };
        assert_eq!(error.layer(), None);
        assert_eq!(error.path(), None);
        let text = error.to_string();
        assert!(text.contains("\"p\""), "{text}");
    }

    #[test]
    fn layer_types_round_trip_their_shapes() {
        // Guards against a section being declared but never wired into resolution -
        // the failure mode where a documented setting silently does nothing.
        let full = layer(
            "[thinking]\nbudget = 1\n[cohort]\nlimit = 2\n[permission]\nmode = \"plan\"\n\
             [prompt]\ncompaction_threshold_percent = 92\n",
        );
        assert_eq!(full.thinking, Some(ThinkingLayer { budget: Some(1) }));
        assert_eq!(full.cohort, Some(CohortLayer { limit: Some(2) }));
        assert_eq!(full.prompt, Some(PromptLayer { compaction_threshold_percent: Some(92) }));
        assert!(matches!(full.permission, Some(PermissionLayer { mode: Some(_) })));

        let config = resolve(&[(ConfigSource::User, full)]);
        assert_eq!(config.thinking_budget(), 1);
        assert_eq!(config.cohort_limit(), 2);
        assert_eq!(config.permission_mode(), Mode::Plan);
        assert_eq!(config.compaction_threshold_percent(), 92);
    }
}
