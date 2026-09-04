//! Errors, each carrying the remedy.
//!
//! Fail-fast is only useful if the failure is actionable. Every variant names the
//! file, the setting, and what to do - a message that says "invalid config" has
//! moved the work to the user rather than doing it.
//!
//! There is deliberately no variant for "setting missing". A missing setting is not
//! an error: the builtin layer always supplies a value, so resolution is total.

use std::path::PathBuf;

use crate::source::ConfigSource;

/// Why configuration could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file exists but could not be read.
    #[error("cannot read {} config at {}: {source}", layer.label(), path.display())]
    Unreadable {
        /// Which layer.
        layer: ConfigSource,
        /// Which file.
        path: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// The path is not a regular file.
    ///
    /// Checked because opening a FIFO and reading it would block for ever, turning a
    /// misconfiguration into a hang with no diagnosis.
    #[error(
        "{} config at {} is not a regular file; \
         remove it or point supra elsewhere",
        layer.label(),
        path.display()
    )]
    NotARegularFile {
        /// Which layer.
        layer: ConfigSource,
        /// Which path.
        path: PathBuf,
    },

    /// A private config file is readable by someone other than its owner.
    #[error(
        "{} config at {} is mode {:04o}, readable beyond its owner; \
         run `chmod 600 {}`",
        layer.label(),
        path.display(),
        mode,
        path.display()
    )]
    TooPermissive {
        /// Which layer.
        layer: ConfigSource,
        /// Which file.
        path: PathBuf,
        /// The mode as reported by the open handle.
        mode: u32,
    },

    /// The file is not valid TOML, or violates the schema.
    ///
    /// The message is the `toml` crate's own, which already reports the line, the
    /// column, the offending text, and - for an unknown field - the fields that were
    /// expected instead.
    #[error("{} config at {} is invalid:\n{detail}", layer.label(), path.display())]
    Invalid {
        /// Which layer.
        layer: ConfigSource,
        /// Which file.
        path: PathBuf,
        /// The parser's report, verbatim.
        detail: String,
    },

    /// An environment variable held something the schema rejects.
    #[error("{variable} is not a valid value for this setting: {detail}")]
    InvalidEnv {
        /// The variable name.
        variable: String,
        /// What was wrong.
        detail: String,
    },

    /// A setting was outside its permitted range.
    #[error(
        "{} sets {setting} to {value}, outside the permitted range {low}..={high}: {because}",
        layer.label()
    )]
    OutOfRange {
        /// Which layer set it.
        layer: ConfigSource,
        /// Dotted setting path, as written in the file.
        setting: &'static str,
        /// The offending value.
        value: String,
        /// Lowest accepted value.
        low: String,
        /// Highest accepted value.
        high: String,
        /// Why the range is what it is.
        because: &'static str,
    },

    /// A setting was structurally wrong in a way a range cannot express.
    #[error("{} sets {setting} to {value}: {because}", layer.label())]
    Rejected {
        /// Which layer set it.
        layer: ConfigSource,
        /// Dotted setting path.
        setting: String,
        /// The offending value, or a redacted stand-in.
        value: String,
        /// Why it was refused, and what to write instead.
        because: String,
    },

    /// A layer set something it is not permitted to set.
    #[error(
        "{} config may not set {setting}: {because}",
        layer.label()
    )]
    NotPermittedFromLayer {
        /// Which layer overstepped.
        layer: ConfigSource,
        /// Dotted setting path.
        setting: String,
        /// Why that layer may not set it.
        because: &'static str,
    },

    /// A provider names a credential source that has nothing to give.
    ///
    /// This is the one resolution-time failure that is not a validation failure: the schema
    /// guarantees exactly one source is *named*, but only the lookup can tell whether the
    /// named source *holds* a credential. The message carries the provider and the source by
    /// name - never the value - and says where to put the credential instead.
    #[error("provider {provider:?} has no usable credential: {detail}")]
    MissingCredential {
        /// Which provider.
        provider: String,
        /// What was looked up and where the credential should go instead. Plain data, not a
        /// source chain: there is no underlying error here, only an absent value. Named
        /// `detail` rather than `source` because thiserror treats `source` as the error-chain
        /// accessor, and a `String` is not an error.
        detail: String,
    },
}

impl ConfigError {
    /// The layer the failure is attributed to, when one applies.
    #[must_use]
    pub const fn layer(&self) -> Option<ConfigSource> {
        match self {
            Self::Unreadable { layer, .. }
            | Self::NotARegularFile { layer, .. }
            | Self::TooPermissive { layer, .. }
            | Self::Invalid { layer, .. }
            | Self::OutOfRange { layer, .. }
            | Self::Rejected { layer, .. }
            | Self::NotPermittedFromLayer { layer, .. } => Some(*layer),
            Self::InvalidEnv { .. } => Some(ConfigSource::Env),
            // No layer: the provider exists in the resolved config, which may combine several
            // layers, and the failure is about the world (an unset variable, an empty keyring),
            // not about any one file. Callers that need attribution use provider_source().
            Self::MissingCredential { .. } => None,
        }
    }

    /// The file the failure is attributed to, when one applies.
    #[must_use]
    pub const fn path(&self) -> Option<&PathBuf> {
        match self {
            Self::Unreadable { path, .. }
            | Self::NotARegularFile { path, .. }
            | Self::TooPermissive { path, .. }
            | Self::Invalid { path, .. } => Some(path),
            Self::InvalidEnv { .. }
            | Self::OutOfRange { .. }
            | Self::Rejected { .. }
            | Self::NotPermittedFromLayer { .. }
            | Self::MissingCredential { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_message_names_the_layer_and_the_remedy() {
        let path = PathBuf::from("/tmp/supra/config.toml");

        let too_permissive =
            ConfigError::TooPermissive { layer: ConfigSource::User, path: path.clone(), mode: 0o644 };
        let text = too_permissive.to_string();
        assert!(text.contains("user"), "{text}");
        assert!(text.contains("0644"), "the actual mode, in octal: {text}");
        assert!(text.contains("chmod 600"), "the remedy: {text}");

        let out_of_range = ConfigError::OutOfRange {
            layer: ConfigSource::Project,
            setting: "cohort.limit",
            value: "0".to_owned(),
            low: "1".to_owned(),
            high: "80".to_owned(),
            because: "a cohort needs at least one peer",
        };
        let text = out_of_range.to_string();
        assert!(text.contains("project"), "{text}");
        assert!(text.contains("cohort.limit"), "{text}");
        assert!(text.contains("1..=80"), "{text}");
        assert!(text.contains("at least one peer"), "the reason: {text}");
    }

    #[test]
    fn attribution_is_available_without_parsing_the_message() {
        // The TUI and the log want the layer and the path as data, not as a substring
        // of a sentence.
        let path = PathBuf::from("/x/config.toml");
        let error =
            ConfigError::Invalid { layer: ConfigSource::Project, path: path.clone(), detail: "d".to_owned() };
        assert_eq!(error.layer(), Some(ConfigSource::Project));
        assert_eq!(error.path(), Some(&path));

        let error = ConfigError::InvalidEnv {
            variable: "SUPRA_COHORT_LIMIT".to_owned(),
            detail: "not a number".to_owned(),
        };
        assert_eq!(error.layer(), Some(ConfigSource::Env));
        assert_eq!(error.path(), None);
    }

    #[test]
    fn a_rejected_value_says_what_to_write_instead() {
        let error = ConfigError::Rejected {
            layer: ConfigSource::User,
            setting: "providers.custom.api_key_env".to_owned(),
            value: "sk-REDACTED".to_owned(),
            because: "this field takes the NAME of an environment variable".to_owned(),
        };
        let text = error.to_string();
        assert!(text.contains("takes the NAME"), "{text}");
    }
}
