//! Where a setting came from, and who is trusted to set what.
//!
//! # The ladder
//!
//! Section 6 of the architecture document fixes the precedence for permission
//! rules: `session > cli > project > user > builtin`. Configuration uses the same
//! ladder with **one addition**, [`ConfigSource::Env`], between `project` and
//! `cli`.
//!
//! Environment variables sit there because they describe the environment of this
//! invocation - a CI runner, a container - which is more specific than a
//! checked-in project file and less explicit than a flag someone typed. Without
//! them, supra could not be configured in a container at all.
//!
//! The addition lives here rather than in [`supra_types::RuleSource`] on purpose. A
//! permission rule cannot come from an environment variable, so widening that enum
//! would give it a variant that is unreachable in its own domain. Instead, the two
//! ladders agree on their shared five, and
//! [`ConfigSource::corresponding_rule_source`] plus a test pins that agreement so
//! they cannot drift.
//!
//! # The trust boundary
//!
//! Exactly one source is **not** controlled by the person running supra:
//! [`ConfigSource::Project`]. A project file arrives with the repository, and a
//! repository can be cloned from anywhere. Everything else - the built-in defaults,
//! the user's own file, the environment, the flags, the session - reflects a
//! deliberate act by the operator.
//!
//! That asymmetry is why [`ConfigSource::is_operator_controlled`] exists, and why
//! the project layer may tighten a setting but never loosen one. A repository that
//! could raise its own permissions by shipping a config file would be a
//! straightforward vulnerability.

use supra_types::RuleSource;

/// Which layer a setting came from.
///
/// Ordered by precedence, ascending, so `Ord` picks the winner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConfigSource {
    /// Compiled-in defaults. Always present, so every setting resolves.
    Builtin,
    /// The user's own file, `$XDG_CONFIG_HOME/supra/config.toml`.
    User,
    /// The repository's file, `.supra/config.toml`.
    ///
    /// The only source that is not operator-controlled. See the module docs.
    Project,
    /// `SUPRA_*` environment variables.
    Env,
    /// Flags on this invocation.
    Cli,
    /// Set during the session, for instance by a slash command.
    Session,
}

impl ConfigSource {
    /// Every source, ascending by precedence.
    pub const ALL: [Self; 6] =
        [Self::Builtin, Self::User, Self::Project, Self::Env, Self::Cli, Self::Session];

    /// Human-readable name, as it appears in an error message or in `/config`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::User => "user",
            Self::Project => "project",
            Self::Env => "env",
            Self::Cli => "cli",
            Self::Session => "session",
        }
    }

    /// Whether the operator chose this source deliberately.
    ///
    /// False only for [`Self::Project`]. A setting from a non-operator-controlled
    /// source may make the session safer but never more permissive.
    #[must_use]
    pub const fn is_operator_controlled(self) -> bool {
        !matches!(self, Self::Project)
    }

    /// The permission-rule source this corresponds to, where one exists.
    ///
    /// `None` for [`Self::Env`], which has no counterpart because a permission rule
    /// cannot come from an environment variable. A test asserts that the five that
    /// do correspond appear in the same relative order in both ladders.
    #[must_use]
    pub const fn corresponding_rule_source(self) -> Option<RuleSource> {
        match self {
            Self::Builtin => Some(RuleSource::Builtin),
            Self::User => Some(RuleSource::User),
            Self::Project => Some(RuleSource::Project),
            Self::Env => None,
            Self::Cli => Some(RuleSource::Cli),
            Self::Session => Some(RuleSource::Session),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shared_ladder_agrees_with_the_permission_ladder() {
        // The two enums are separate so that `RuleSource` keeps exactly the five
        // sources section 6 documents. This test is what stops them from drifting:
        // the five shared sources must appear in the same relative order in both.
        let shared: Vec<(ConfigSource, RuleSource)> = ConfigSource::ALL
            .into_iter()
            .filter_map(|source| source.corresponding_rule_source().map(|rule| (source, rule)))
            .collect();

        assert_eq!(shared.len(), 5, "exactly one config source has no rule counterpart");

        for pair in shared.windows(2) {
            let (config_lower, rule_lower) = pair[0];
            let (config_upper, rule_upper) = pair[1];
            assert!(config_lower < config_upper, "{config_lower:?} must precede {config_upper:?}");
            assert!(
                rule_lower < rule_upper,
                "the rule ladder disagrees: {rule_lower:?} does not precede {rule_upper:?}"
            );
        }
    }

    #[test]
    fn env_is_the_only_addition_and_it_sits_between_project_and_cli() {
        let without_counterpart: Vec<ConfigSource> = ConfigSource::ALL
            .into_iter()
            .filter(|source| source.corresponding_rule_source().is_none())
            .collect();
        assert_eq!(without_counterpart, vec![ConfigSource::Env]);

        assert!(ConfigSource::Project < ConfigSource::Env);
        assert!(ConfigSource::Env < ConfigSource::Cli);
    }

    #[test]
    fn only_the_project_layer_is_untrusted() {
        // The trust boundary, stated as a test because the tightening rule in
        // `resolve` depends on exactly this set.
        let untrusted: Vec<ConfigSource> =
            ConfigSource::ALL.into_iter().filter(|source| !source.is_operator_controlled()).collect();
        assert_eq!(untrusted, vec![ConfigSource::Project]);
    }

    #[test]
    fn precedence_is_the_declared_order() {
        assert!(ConfigSource::Builtin < ConfigSource::User);
        assert!(ConfigSource::User < ConfigSource::Project);
        assert!(ConfigSource::Cli < ConfigSource::Session);

        // And `max` picks the winner, which is how `resolve` uses it.
        let winner = ConfigSource::ALL.into_iter().max();
        assert_eq!(winner, Some(ConfigSource::Session));
    }

    #[test]
    fn labels_are_the_spellings_used_in_diagnostics() {
        let labels: Vec<&str> = ConfigSource::ALL.iter().map(|s| s.label()).collect();
        assert_eq!(labels, vec!["builtin", "user", "project", "env", "cli", "session"]);
    }
}
