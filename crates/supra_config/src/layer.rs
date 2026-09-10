//! The file schema: one layer as it appears on disk, before precedence.
//!
//! # Why every field is optional
//!
//! Precedence is **per field**, not per file. A project file that sets only
//! `[cohort] limit` must not erase the user's `[thinking] budget`, so a layer
//! records *what it says* and stays silent about everything else. That is why this
//! type is all `Option` and [`crate::Config`] is not: they are different shapes
//! answering different questions, and collapsing them into one is the mistake that
//! makes layered configuration surprising.
//!
//! An absent `Option` field needs no `#[serde(default)]` - serde already treats it
//! as `None`, which was confirmed against `toml` 1.1 rather than assumed.
//!
//! # Why unknown fields are refused
//!
//! `deny_unknown_fields` everywhere. A misspelled key that is silently ignored is
//! the archetypal configuration bug: the setting appears to be applied, the
//! behaviour does not change, and nothing says why. The `toml` parser's message
//! already names the offending key, its line and column, and the keys that were
//! expected instead, so refusing costs nothing in diagnosability.
//!
//! The consequence is that a later stage adding a setting must edit this file. That
//! is deliberate: one schema means one place to look, one `--help` to generate, and
//! an extension that shows up as a visible diff rather than a silently tolerated key.
//!
//! # Why no field can hold a credential
//!
//! There is no `api_key` in this schema, only `api_key_env` and `api_key_keyring` -
//! the *name* of an environment variable or of a keyring entry. A configuration file
//! that cannot hold a secret cannot leak one, whether by being committed, pasted
//! into a bug report, or read by another user.
//!
//! `api_key` and `auth_token` are nevertheless *declared*, as fields whose only
//! behaviour is to be rejected with that explanation. `deny_unknown_fields` would
//! already refuse them, but with a generic message; the reader who typed one needs
//! to be told where the credential should go instead.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, de};
use supra_types::{Mode, PEER_CEILING};

use crate::error::ConfigError;
use crate::source::ConfigSource;

/// Lowest accepted compaction threshold, as a percentage of the context window.
///
/// Invariant I4 sets the band at 92-95%, deliberately far above the 80% that a
/// cache-unaware harness uses: every avoided compaction is one full prefix rewrite
/// not paid for. Below this the economics the design rests on stop holding, so a
/// lower value is refused rather than accepted with a warning.
pub const MIN_COMPACTION_THRESHOLD_PERCENT: u8 = 92;

/// Highest accepted compaction threshold.
///
/// Above this there is too little headroom to finish a turn after the decision to
/// compact is taken.
pub const MAX_COMPACTION_THRESHOLD_PERCENT: u8 = 95;

/// A permission mode as written in a file.
///
/// The accepted spellings are [`Mode::label`], read from [`Mode::ALL`], so the file
/// syntax cannot drift from the mode set T6 defines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct ModeSetting(Mode);

impl ModeSetting {
    /// The mode this names.
    #[must_use]
    pub const fn mode(self) -> Mode {
        self.0
    }
}

impl TryFrom<String> for ModeSetting {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Mode::ALL.into_iter().find(|mode| mode.label() == text).map(Self).ok_or_else(|| {
            let accepted: Vec<&str> = Mode::ALL.iter().map(|mode| mode.label()).collect();
            format!("expected one of {}, found {text:?}", accepted.join(", "))
        })
    }
}

/// A field that exists only to be refused, with an explanation.
///
/// See the module documentation: `deny_unknown_fields` would reject `api_key`
/// anyway, but "unknown field" does not tell the reader where to put the credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoLiteralCredential;

impl<'de> Deserialize<'de> for NoLiteralCredential {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Consume the value before failing. A deserializer left part-way through can
        // report a confusing "invalid type" instead of the message written here.
        let _ = de::IgnoredAny::deserialize(deserializer);
        Err(de::Error::custom(
            "supra never reads a literal credential from a configuration file. \
             Use `api_key_env = \"YOUR_ENV_VAR\"` to name an environment variable, \
             or `api_key_keyring = \"entry-name\"` to name an OS keyring entry",
        ))
    }
}

/// Reasoning budget settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThinkingLayer {
    /// Reasoning tokens the model may spend per turn, or 0 to disable reasoning.
    ///
    /// Read once at startup and then frozen for the session, because
    /// `budget_tokens` is rendered into the prompt and changing it invalidates every
    /// cache breakpoint. There is no slash command for it; a different budget means
    /// a new session.
    ///
    /// T7 checks only that the value is a `u32`. The per-model minimum is a provider
    /// fact that T13 owns, and T13 must check it **at startup** rather than at the
    /// first request, or this setting stops being fail-fast.
    pub budget: Option<u32>,
}

/// Peer cohort settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CohortLayer {
    /// Largest cohort any tier may field, from 1 to 80.
    ///
    /// A cap on scrutiny rather than a target: a single-file edit still runs two
    /// peers under a limit of 80. A limit that falls between two tiers reduces the
    /// tier - see `supra_types::admit`.
    pub limit: Option<usize>,
}

/// Permission settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionLayer {
    /// One of `plan`, `ask`, `auto`, `yolo`.
    ///
    /// From the project layer this may only make the session **stricter**; see
    /// [`crate::resolve`].
    pub mode: Option<ModeSetting>,
}

/// Prompt ledger settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptLayer {
    /// Context-window usage at which a new generation is written, 92 to 95.
    pub compaction_threshold_percent: Option<u8>,
}

/// One provider entry.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderLayer {
    /// Base URL. Must be `https`, except on a loopback host.
    ///
    /// For an SDK-wired provider this overrides the SDK's default base:
    /// a custom gateway or an OpenAI-compatible proxy. Leave it unset
    /// for the provider's own API.
    pub endpoint: Option<String>,
    /// Model identifier to send.
    ///
    /// Leave it unset for the provider's current cheapest default; pin
    /// it for a fixed model.
    pub model: Option<String>,
    /// **Name** of an environment variable holding the credential.
    pub api_key_env: Option<String>,
    /// **Name** of an OS keyring entry holding the credential.
    pub api_key_keyring: Option<String>,
    /// Declared only to be refused. See [`NoLiteralCredential`].
    pub api_key: Option<NoLiteralCredential>,
    /// Declared only to be refused. See [`NoLiteralCredential`].
    pub auth_token: Option<NoLiteralCredential>,
}

/// One configuration layer, exactly as its file or its environment states it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigLayer {
    /// `[thinking]`
    pub thinking: Option<ThinkingLayer>,
    /// `[cohort]`
    pub cohort: Option<CohortLayer>,
    /// `[permission]`
    pub permission: Option<PermissionLayer>,
    /// `[prompt]`
    pub prompt: Option<PromptLayer>,
    /// `[providers.<name>]`
    ///
    /// A `BTreeMap` rather than a `HashMap`: iteration order is part of the
    /// behaviour, because a provider list that reorders between runs would make
    /// diagnostics and any derived cache key non-deterministic.
    pub providers: Option<BTreeMap<String, ProviderLayer>>,
}

impl ConfigLayer {
    /// Parse a layer from TOML text.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Invalid`], carrying the parser's cause and the line and column
    /// it occurred at - but **not** the source line itself. See
    /// [`describe_toml_error`] for why that omission is deliberate.
    pub fn parse(text: &str, layer: ConfigSource, path: std::path::PathBuf) -> Result<Self, ConfigError> {
        toml::from_str(text).map_err(|error| ConfigError::Invalid {
            layer,
            path,
            detail: describe_toml_error(text, &error),
        })
    }

    /// Check everything about this layer that does not depend on other layers.
    ///
    /// Running this before resolution is what makes resolution **infallible**: a
    /// layer that reaches [`crate::resolve`] has already had every bound, every
    /// value shape, and every per-layer permission checked.
    ///
    /// # Errors
    ///
    /// [`ConfigError::OutOfRange`] for a numeric bound, [`ConfigError::Rejected`] for
    /// a malformed value, [`ConfigError::NotPermittedFromLayer`] when this source may
    /// not set the setting at all.
    pub fn validate(&self, layer: ConfigSource) -> Result<(), ConfigError> {
        self.validate_cohort(layer)?;
        self.validate_prompt(layer)?;
        self.validate_providers(layer)?;
        Ok(())
    }

    fn validate_cohort(&self, layer: ConfigSource) -> Result<(), ConfigError> {
        let Some(limit) = self.cohort.and_then(|cohort| cohort.limit) else {
            return Ok(());
        };
        if (1..=PEER_CEILING).contains(&limit) {
            return Ok(());
        }
        Err(ConfigError::OutOfRange {
            layer,
            setting: "cohort.limit",
            value: limit.to_string(),
            low: "1".to_owned(),
            high: PEER_CEILING.to_string(),
            because: "a cohort needs at least one peer, and 80 is a hard ceiling that no \
                      configuration raises",
        })
    }

    fn validate_prompt(&self, layer: ConfigSource) -> Result<(), ConfigError> {
        let Some(threshold) = self.prompt.and_then(|prompt| prompt.compaction_threshold_percent) else {
            return Ok(());
        };
        if (MIN_COMPACTION_THRESHOLD_PERCENT..=MAX_COMPACTION_THRESHOLD_PERCENT).contains(&threshold) {
            return Ok(());
        }
        Err(ConfigError::OutOfRange {
            layer,
            setting: "prompt.compaction_threshold_percent",
            value: threshold.to_string(),
            low: MIN_COMPACTION_THRESHOLD_PERCENT.to_string(),
            high: MAX_COMPACTION_THRESHOLD_PERCENT.to_string(),
            because: "compacting earlier pays for a full prefix rewrite that was avoidable; \
                      compacting later leaves too little headroom to finish a turn",
        })
    }

    fn validate_providers(&self, layer: ConfigSource) -> Result<(), ConfigError> {
        let Some(providers) = &self.providers else {
            return Ok(());
        };

        // A repository is cloned from anywhere, so it may not name the endpoint a
        // prompt is sent to, nor where the credential for it comes from.
        if !layer.is_operator_controlled() {
            return Err(ConfigError::NotPermittedFromLayer {
                layer,
                setting: "providers".to_owned(),
                because: "a repository must not be able to choose where prompts are sent or \
                          which credential is used; put provider settings in your own config, \
                          the environment, or a flag",
            });
        }

        for (name, provider) in providers {
            Self::validate_provider_name(layer, name)?;
            Self::validate_provider(layer, name, provider)?;
        }
        Ok(())
    }

    fn validate_provider_name(layer: ConfigSource, name: &str) -> Result<(), ConfigError> {
        // `toml` accepts `[providers.""]`, so the empty name has to be refused here.
        if name.is_empty() {
            return Err(ConfigError::Rejected {
                layer,
                setting: "providers".to_owned(),
                value: "\"\"".to_owned(),
                because: "a provider needs a name to be selected by".to_owned(),
            });
        }
        if name.chars().any(char::is_whitespace) {
            return Err(ConfigError::Rejected {
                layer,
                setting: format!("providers.{name}"),
                value: name.to_owned(),
                because: "a provider name may not contain whitespace; it is used as an \
                          identifier on the command line"
                    .to_owned(),
            });
        }
        Ok(())
    }

    fn validate_provider(
        layer: ConfigSource,
        name: &str,
        provider: &ProviderLayer,
    ) -> Result<(), ConfigError> {
        if provider.api_key_env.is_some() && provider.api_key_keyring.is_some() {
            return Err(ConfigError::Rejected {
                layer,
                setting: format!("providers.{name}"),
                value: "api_key_env + api_key_keyring".to_owned(),
                because: "set exactly one credential source, so there is no question which \
                          one is in use"
                    .to_owned(),
            });
        }

        if let Some(variable) = &provider.api_key_env {
            check_env_var_name(layer, &format!("providers.{name}.api_key_env"), variable)?;
        }
        if let Some(entry) = &provider.api_key_keyring {
            if entry.trim().is_empty() {
                return Err(ConfigError::Rejected {
                    layer,
                    setting: format!("providers.{name}.api_key_keyring"),
                    value: redact(entry),
                    because: "a keyring entry needs a name".to_owned(),
                });
            }
        }
        if let Some(endpoint) = &provider.endpoint {
            check_endpoint(layer, &format!("providers.{name}.endpoint"), endpoint)?;
        }
        if let Some(model) = &provider.model {
            if model.trim().is_empty() {
                return Err(ConfigError::Rejected {
                    layer,
                    setting: format!("providers.{name}.model"),
                    value: model.clone(),
                    because: "an empty model identifier would be sent to the provider as-is".to_owned(),
                });
            }
        }
        Ok(())
    }
}

/// Refuse anything that is not shaped like an environment variable name.
///
/// The mistake this catches is pasting the credential itself into `api_key_env`.
/// Without the check, supra would look up an environment variable literally named
/// `sk-...`, find nothing, and report a missing credential - sending the reader
/// looking in the wrong place while the real key sat in a file on disk.
fn check_env_var_name(layer: ConfigSource, setting: &str, value: &str) -> Result<(), ConfigError> {
    let shaped = !value.is_empty()
        && value.starts_with(|first: char| first.is_ascii_uppercase() || first == '_')
        && value.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');

    if shaped {
        return Ok(());
    }
    Err(ConfigError::Rejected {
        layer,
        setting: setting.to_owned(),
        value: redact(value),
        because: "this field takes the NAME of an environment variable, such as \
                  \"ANTHROPIC_API_KEY\", not the credential itself"
            .to_owned(),
    })
}

/// Refuse a plaintext endpoint outside loopback.
///
/// A custom provider reachable over `http` would carry every prompt, and every
/// credential sent with it, in clear. Loopback is exempt because a local proxy or a
/// test double has no network to be observed on.
fn check_endpoint(layer: ConfigSource, setting: &str, value: &str) -> Result<(), ConfigError> {
    if let Some(rest) = value.strip_prefix("https://") {
        if rest.is_empty() {
            return Err(ConfigError::Rejected {
                layer,
                setting: setting.to_owned(),
                value: value.to_owned(),
                because: "an endpoint needs a host".to_owned(),
            });
        }
        return Ok(());
    }

    if let Some(rest) = value.strip_prefix("http://") {
        if is_loopback_authority(authority_host(rest)) {
            return Ok(());
        }
        return Err(ConfigError::Rejected {
            layer,
            setting: setting.to_owned(),
            value: value.to_owned(),
            because: "plaintext http would send prompts and credentials in clear; use https, \
                      or a loopback host for a local proxy"
                .to_owned(),
        });
    }

    Err(ConfigError::Rejected {
        layer,
        setting: setting.to_owned(),
        value: value.to_owned(),
        because: "an endpoint must start with https:// (or http:// on loopback)".to_owned(),
    })
}

/// The host part of an authority, with a bracketed IPv6 literal unwrapped.
///
/// Splitting on `:` alone is wrong for IPv6: `[::1]:8080` would yield `[`, so
/// `http://[::1]:8080` - an ordinary local proxy - was rejected as though it pointed at
/// the open internet, with a message about sending credentials in clear. Found by an
/// audit after T7 shipped.
///
/// The **first fix for that was itself a bypass**, caught by its own adversarial test:
/// unwrapping the brackets and ignoring whatever followed made
/// `http://[::1].evil.example` read as loopback, so an attacker-controlled host would
/// have been served plaintext. After the closing bracket the only thing permitted is a
/// numeric port; anything else means the authority is malformed and is not loopback.
///
/// Returns an empty string for anything malformed, which no loopback test accepts.
fn authority_host(rest: &str) -> &str {
    // The authority ends at the first path, query, or fragment delimiter.
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return "";
    }

    if let Some(after_bracket) = authority.strip_prefix('[') {
        let Some(close) = after_bracket.find(']') else {
            // Unclosed bracket: not a host at all.
            return "";
        };
        let (host, remainder) = after_bracket.split_at(close);
        // `remainder` starts with the `]` itself.
        let after_close = remainder.get(1..).unwrap_or_default();
        let port_is_well_formed = match after_close.strip_prefix(':') {
            Some(port) => !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()),
            None => after_close.is_empty(),
        };
        if !port_is_well_formed {
            return "";
        }
        return host;
    }

    // Userinfo is not supported in an endpoint, so anything before an `@` stays in the
    // host and fails the loopback test rather than being parsed away.
    authority.split(':').next().unwrap_or_default()
}

/// Whether a host is loopback.
///
/// `localhost` by name, and anything the standard library itself considers loopback -
/// which is the whole of `127.0.0.0/8` and `::1`, and correctly excludes a *hostname*
/// like `127.evil.example` that merely begins with the right digits. A hand-rolled
/// `starts_with("127.")` would have accepted that.
fn is_loopback_authority(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>().is_ok_and(|address| address.is_loopback())
}

/// Describe a TOML failure without quoting the file.
///
/// The `toml` crate's own `Display` is excellent for a human: it prints the offending
/// **source line** with a caret under the exact span. That is precisely why it cannot
/// be used here.
///
/// A configuration file may contain a credential the reader mistakenly put there -
/// that mistake is the reason `api_key` exists only to be refused. If the refusal
/// then echoes the line it was refusing, the credential lands in stderr, in the T8
/// log, and in whatever bug report is filed next. The leak the schema was designed to
/// prevent would be reintroduced by the error message announcing it.
///
/// So the location and the cause are reported, and the content is not. The reader
/// opens the file at the line; the log holds a description.
///
/// A mistyped scalar's parser cause text includes the value (`invalid type:
/// string "four", expected usize`); that value is redacted so a credential
/// parked in the wrong field cannot ride out in the message.
fn describe_toml_error(text: &str, error: &toml::de::Error) -> String {
    let message = strip_deserialized_value(error.message());
    let Some(span) = error.span() else {
        return message;
    };
    let Some((line, column)) = line_and_column(text, span.start) else {
        return message;
    };
    format!("line {line}, column {column}: {message}")
}

/// Remove the value from a wrong-type deserialisation message.
///
/// The shape is `invalid type: string "value", expected usize`: the first
/// double-quoted segment after `invalid type:` is the value the reader
/// supplied, and a credential parked in the wrong field would ride out in
/// the message. Only that segment is stripped; every other quoted span -
/// accepted mode spellings, field names - survives.
fn strip_deserialized_value(message: &str) -> String {
    let Some(marker) = message.find("invalid type:") else {
        return message.to_owned();
    };
    let after = &message[marker..];
    let Some(open) = after.find('"') else {
        return message.to_owned();
    };
    let value_end = &after[open + 1..];
    let Some(close) = value_end.find('"') else {
        return message.to_owned();
    };
    let mut stripped = String::with_capacity(message.len());
    stripped.push_str(&message[..marker]);
    stripped.push_str("invalid type: <redacted>");
    stripped.push_str(&value_end[close + 1..]);
    stripped
}

/// One-based line and column of a byte offset.
///
/// `None` when the offset is out of range or not on a character boundary. Both are
/// defensive: the input is an arbitrary file, and a panic while reporting an error is
/// the one failure that leaves a reader with nothing at all.
fn line_and_column(text: &str, offset: usize) -> Option<(usize, usize)> {
    let prefix = text.get(..offset)?;
    let line = prefix.matches('\n').count() + 1;
    let column = prefix.rsplit('\n').next().unwrap_or_default().chars().count() + 1;
    Some((line, column))
}

/// Shorten a value for an error message.
///
/// A rejected value may be the credential the reader mistakenly pasted, so the
/// message shows enough to identify the field and no more. It is the *field* that
/// needs naming, not the secret.
///
/// Counts **characters** throughout. An earlier version guarded on `len()` - bytes - and
/// truncated with `chars().take()`, so a two-character CJK value was six bytes, passed
/// the guard, and was then echoed in full. An audit caught it.
fn redact(value: &str) -> String {
    const KEEP: usize = 4;
    let total = value.chars().count();
    if total <= KEEP {
        return "\u{2026}".to_owned();
    }
    let head: String = value.chars().take(KEEP).collect();
    format!("{head}\u{2026} ({total} characters)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn parse(text: &str) -> Result<ConfigLayer, ConfigError> {
        ConfigLayer::parse(text, ConfigSource::User, PathBuf::from("/test/config.toml"))
    }

    fn parse_and_validate(text: &str, layer: ConfigSource) -> Result<ConfigLayer, ConfigError> {
        let parsed = ConfigLayer::parse(text, layer, PathBuf::from("/test/config.toml"))?;
        parsed.validate(layer)?;
        Ok(parsed)
    }

    #[test]
    fn an_empty_file_is_a_layer_that_says_nothing() {
        // Not an error, and not a layer full of defaults either: silence, so that a
        // lower-precedence value survives.
        let layer = parse("").expect("an empty file is valid");
        assert_eq!(layer, ConfigLayer::default());
        assert!(layer.thinking.is_none());
        assert!(layer.providers.is_none());
    }

    #[test]
    fn a_partial_file_stays_silent_about_what_it_omits() {
        // The property per-field precedence depends on: setting one section must not
        // imply anything about the others.
        let layer = parse("[cohort]\nlimit = 4\n").expect("valid");
        assert_eq!(layer.cohort.and_then(|c| c.limit), Some(4));
        assert!(layer.thinking.is_none(), "an unmentioned section is silent, not defaulted");
        assert!(layer.permission.is_none());
        assert!(layer.prompt.is_none());
    }

    #[test]
    fn an_empty_section_is_still_silent_about_its_fields() {
        let layer = parse("[thinking]\n").expect("valid");
        assert_eq!(layer.thinking, Some(ThinkingLayer { budget: None }));
    }

    #[test]
    fn a_misspelled_key_is_refused_and_the_message_names_the_alternatives() {
        // The archetypal configuration bug: silently ignored, appears applied,
        // behaviour unchanged, nothing says why.
        let error = parse("[cohort]\nlimt = 4\n").expect_err("a typo must fail");
        let text = error.to_string();
        assert!(text.contains("unknown field `limt`"), "{text}");
        assert!(text.contains("expected `limit`"), "the alternatives: {text}");
        assert!(text.contains("line 2"), "the location: {text}");
    }

    #[test]
    fn a_misspelled_section_is_refused() {
        let error = parse("[cohorts]\nlimit = 4\n").expect_err("a typo must fail");
        assert!(error.to_string().contains("unknown field `cohorts`"), "{error}");
    }

    #[test]
    fn a_duplicate_key_is_refused() {
        let error = parse("[cohort]\nlimit = 1\nlimit = 2\n").expect_err("ambiguous");
        assert!(error.to_string().contains("duplicate key"), "{error}");
    }

    #[test]
    fn mode_spellings_come_from_the_mode_type_itself() {
        // Every label T6 defines must parse, so the file syntax cannot drift from the
        // mode set.
        for mode in Mode::ALL {
            let text = format!("[permission]\nmode = \"{}\"\n", mode.label());
            let layer = parse(&text).expect("every label must parse");
            assert_eq!(layer.permission.and_then(|p| p.mode).map(ModeSetting::mode), Some(mode));
        }
    }

    #[test]
    fn an_unknown_mode_lists_the_accepted_spellings() {
        let error = parse("[permission]\nmode = \"reckless\"\n").expect_err("not a mode");
        let text = error.to_string();
        assert!(text.contains("plan, ask, auto, yolo"), "{text}");
        assert!(text.contains("reckless"), "the offending value: {text}");
    }

    #[test]
    fn a_literal_credential_is_refused_with_the_field_to_use_instead() {
        // The accident this exists for: the reader pastes the key into config.
        for field in ["api_key", "auth_token"] {
            let text = format!("[providers.custom]\n{field} = \"sk-not-a-real-key\"\n");
            let error =
                parse(&text).map(|layer| panic!("{field} must be refused, got {layer:?}")).unwrap_err();
            let message = error.to_string();
            assert!(message.contains("never reads a literal credential"), "{field}: {message}");
            assert!(message.contains("api_key_env"), "{field} must name the remedy: {message}");
            assert!(message.contains("api_key_keyring"), "{field} must name both remedies: {message}");
        }
    }

    #[test]
    fn a_credential_pasted_into_the_env_var_field_is_refused() {
        // The near miss the shape check catches: without it supra would look up an
        // environment variable literally named `sk-...`, report a missing credential,
        // and send the reader looking in the wrong place.
        let error = parse_and_validate(
            "[providers.custom]\napi_key_env = \"sk-abcdefghijklmnop\"\n",
            ConfigSource::User,
        )
        .expect_err("a literal key is not a variable name");
        let text = error.to_string();
        assert!(text.contains("takes the NAME"), "{text}");
        assert!(!text.contains("abcdefghijklmnop"), "the value must not be echoed: {text}");
        assert!(text.contains("characters)"), "but its length may be: {text}");
    }

    #[test]
    fn a_well_shaped_env_var_name_is_accepted() {
        for name in ["ANTHROPIC_API_KEY", "_PRIVATE", "K2", "A_B_C_1"] {
            let text = format!("[providers.p]\napi_key_env = \"{name}\"\n");
            parse_and_validate(&text, ConfigSource::User)
                .unwrap_or_else(|error| panic!("{name} should be accepted: {error}"));
        }
        for name in ["lowercase", "has space", "has-dash", "1LEADING", ""] {
            let text = format!("[providers.p]\napi_key_env = \"{name}\"\n");
            assert!(parse_and_validate(&text, ConfigSource::User).is_err(), "{name:?} should be refused");
        }
    }

    #[test]
    fn two_credential_sources_are_refused_as_ambiguous() {
        let error = parse_and_validate(
            "[providers.p]\napi_key_env = \"K\"\napi_key_keyring = \"e\"\n",
            ConfigSource::User,
        )
        .expect_err("ambiguous");
        assert!(error.to_string().contains("exactly one credential source"), "{error}");
    }

    #[test]
    fn neither_credential_source_is_allowed() {
        // A provider entry may exist only to pin a model; T12 owns the default
        // credential lookup for a named provider.
        parse_and_validate("[providers.p]\nmodel = \"m\"\n", ConfigSource::User)
            .expect("a model-only entry is valid");
    }

    #[test]
    fn a_plaintext_endpoint_is_refused_except_on_loopback() {
        for endpoint in ["https://api.example.com", "http://localhost:8080", "http://127.0.0.1"] {
            let text = format!("[providers.p]\nendpoint = \"{endpoint}\"\n");
            parse_and_validate(&text, ConfigSource::User)
                .unwrap_or_else(|error| panic!("{endpoint} should be accepted: {error}"));
        }

        for endpoint in ["http://api.example.com", "ftp://x", "api.example.com", "https://"] {
            let text = format!("[providers.p]\nendpoint = \"{endpoint}\"\n");
            assert!(parse_and_validate(&text, ConfigSource::User).is_err(), "{endpoint} should be refused");
        }
    }

    #[test]
    fn a_plaintext_endpoint_message_explains_the_exposure() {
        let error =
            parse_and_validate("[providers.p]\nendpoint = \"http://evil.example\"\n", ConfigSource::User)
                .expect_err("plaintext");
        assert!(error.to_string().contains("in clear"), "{error}");
    }

    #[test]
    fn a_project_layer_may_not_choose_the_provider() {
        // The trust boundary: a cloned repository must not be able to redirect
        // prompts or select which credential is used.
        let error = parse_and_validate(
            "[providers.exfil]\nendpoint = \"https://attacker.example\"\n",
            ConfigSource::Project,
        )
        .expect_err("a project may not set providers");
        let text = error.to_string();
        assert!(text.contains("project config may not set providers"), "{text}");
        assert!(text.contains("where prompts are sent"), "the reason: {text}");
    }

    #[test]
    fn an_operator_controlled_layer_may_choose_the_provider() {
        for layer in ConfigSource::ALL.into_iter().filter(|s| s.is_operator_controlled()) {
            parse_and_validate("[providers.p]\nendpoint = \"https://a.example\"\n", layer)
                .unwrap_or_else(|error| panic!("{layer:?} should be allowed: {error}"));
        }
    }

    #[test]
    fn the_cohort_limit_is_bounded_by_the_hard_ceiling() {
        parse_and_validate("[cohort]\nlimit = 1\n", ConfigSource::User).expect("1 is valid");
        let text = format!("[cohort]\nlimit = {PEER_CEILING}\n");
        parse_and_validate(&text, ConfigSource::User).expect("the ceiling is valid");

        let error = parse_and_validate("[cohort]\nlimit = 0\n", ConfigSource::User)
            .expect_err("a cohort needs a peer");
        assert!(error.to_string().contains("1..=80"), "{error}");

        let text = format!("[cohort]\nlimit = {}\n", PEER_CEILING + 1);
        let error = parse_and_validate(&text, ConfigSource::User).expect_err("above the ceiling");
        assert!(error.to_string().contains("hard ceiling"), "{error}");
    }

    #[test]
    fn a_negative_cohort_limit_is_refused_by_the_parser() {
        let error = parse("[cohort]\nlimit = -1\n").expect_err("not a usize");
        assert!(error.to_string().contains("invalid value"), "{error}");
    }

    #[test]
    fn the_compaction_threshold_is_held_inside_its_band() {
        for threshold in MIN_COMPACTION_THRESHOLD_PERCENT..=MAX_COMPACTION_THRESHOLD_PERCENT {
            let text = format!("[prompt]\ncompaction_threshold_percent = {threshold}\n");
            parse_and_validate(&text, ConfigSource::User)
                .unwrap_or_else(|error| panic!("{threshold} should be accepted: {error}"));
        }

        // 80 is the figure a cache-unaware harness uses, and the one this design
        // exists to move away from.
        let error = parse_and_validate("[prompt]\ncompaction_threshold_percent = 80\n", ConfigSource::User)
            .expect_err("too early");
        let text = error.to_string();
        assert!(text.contains("92..=95"), "{text}");
        assert!(text.contains("full prefix rewrite"), "the reason: {text}");

        assert!(
            parse_and_validate("[prompt]\ncompaction_threshold_percent = 99\n", ConfigSource::User).is_err(),
            "too late leaves no headroom"
        );
    }

    #[test]
    fn provider_names_must_be_usable_as_identifiers() {
        let error = parse_and_validate("[providers.\"\"]\nmodel = \"m\"\n", ConfigSource::User)
            .expect_err("toml accepts an empty key, so this layer must not");
        assert!(error.to_string().contains("needs a name"), "{error}");

        let error = parse_and_validate("[providers.\"two words\"]\nmodel = \"m\"\n", ConfigSource::User)
            .expect_err("whitespace");
        assert!(error.to_string().contains("whitespace"), "{error}");
    }

    #[test]
    fn provider_iteration_order_is_deterministic() {
        // Declared last, iterated first: order comes from the map, not the file, so
        // diagnostics and any derived key are stable across runs.
        let layer =
            parse("[providers.zulu]\nmodel = \"z\"\n[providers.alpha]\nmodel = \"a\"\n").expect("valid");
        let names: Vec<&String> =
            layer.providers.as_ref().map(|map| map.keys().collect()).unwrap_or_default();
        assert_eq!(names, vec!["alpha", "zulu"]);
    }

    #[test]
    fn an_empty_model_is_refused() {
        let error = parse_and_validate("[providers.p]\nmodel = \"  \"\n", ConfigSource::User)
            .expect_err("whitespace only");
        assert!(error.to_string().contains("empty model identifier"), "{error}");
    }

    #[test]
    fn a_parse_error_never_echoes_the_line_it_is_refusing() {
        // The regression guard for a real leak. The `toml` crate's own Display prints
        // the offending source line with a caret under it, so using it verbatim put
        // the credential straight into the message announcing that credentials are
        // not allowed. This test failed until the excerpt was removed.
        let secret = "sk-this-must-never-appear-in-a-log";

        // A refused credential field.
        let text = format!("[providers.p]\napi_key = \"{secret}\"\n");
        let message = parse(&text).expect_err("refused").to_string();
        assert!(!message.contains(secret), "the credential leaked: {message}");
        assert!(message.contains("never reads a literal credential"), "{message}");
        assert!(message.contains("line 2"), "the location must survive: {message}");

        // A syntax error on a line that happens to carry a secret.
        let text = format!("[providers.p\napi_key_env = \"{secret}\"\n");
        let message = parse(&text).expect_err("refused").to_string();
        assert!(!message.contains(secret), "the credential leaked: {message}");

        // An unknown key beside a secret.
        let text = format!("[providers.p]\nnonsense = \"{secret}\"\n");
        let message = parse(&text).expect_err("refused").to_string();
        assert!(!message.contains(secret), "the credential leaked: {message}");
        assert!(message.contains("unknown field `nonsense`"), "{message}");
    }

    #[test]
    fn a_wrong_type_error_never_echoes_the_value() {
        let secret = "sk-super-secret-value";
        let text = format!("[cohort]\nlimit = \"{secret}\"\n");
        let message = parse(&text).expect_err("wrong type").to_string();
        assert!(!message.contains(secret), "the value leaked: {message}");
        assert!(message.contains("expected usize"), "the cause must survive: {message}");
    }

    #[test]
    fn a_location_is_reported_for_every_parse_failure() {
        // The reported line is the *first* failure, which is why each fixture keeps
        // everything before the offending line valid.
        for (text, expected_line) in [
            ("[cohort]\nlimt = 1\n", 2),
            ("[cohort]\nlimit = \"x\"\n", 2),
            ("bad = 1\n", 1),
            ("[cohort]\nlimit = 1\n[prompt]\ncompaction_threshold_percen = 92\n", 4),
        ] {
            let message = parse(text).expect_err("invalid").to_string();
            assert!(
                message.contains(&format!("line {expected_line}")),
                "expected line {expected_line} in: {message}"
            );
        }
    }

    #[test]
    fn line_and_column_handles_multibyte_and_out_of_range_offsets() {
        // Called while reporting an error, so it must not panic on arbitrary input.
        let text = "a\u{4E2D}b\nsecond";
        assert_eq!(line_and_column(text, 0), Some((1, 1)));
        assert_eq!(line_and_column(text, 1), Some((1, 2)));
        // Column counts characters, not bytes: the CJK character is three bytes.
        assert_eq!(line_and_column(text, 4), Some((1, 3)));
        assert_eq!(line_and_column(text, 6), Some((2, 1)));

        // Inside a multi-byte character, and past the end.
        assert_eq!(line_and_column(text, 2), None);
        assert_eq!(line_and_column(text, 9_999), None);
    }

    #[test]
    fn an_ipv6_loopback_endpoint_is_accepted_in_every_form() {
        // A real bug, found by an audit after T7 shipped. Splitting the authority on `:`
        // alone yields `[` for `[::1]:8080`, so an ordinary local proxy over IPv6 was
        // rejected with a message about sending credentials in clear.
        for endpoint in [
            "http://[::1]",
            "http://[::1]:8080",
            "http://[::1]:8080/v1",
            "http://[::1]/v1?x=1",
            "http://[0:0:0:0:0:0:0:1]:9",
        ] {
            let text = format!("[providers.p]\nendpoint = \"{endpoint}\"\n");
            parse_and_validate(&text, ConfigSource::User)
                .unwrap_or_else(|error| panic!("{endpoint} should be accepted: {error}"));
        }
    }

    #[test]
    fn the_whole_loopback_range_counts_not_just_one_address() {
        // Loopback is 127.0.0.0/8, and the standard library already knows that. A
        // hand-rolled list would have accepted only 127.0.0.1.
        for endpoint in ["http://127.0.0.1", "http://127.0.0.2:8080", "http://127.1.2.3/v1"] {
            let text = format!("[providers.p]\nendpoint = \"{endpoint}\"\n");
            parse_and_validate(&text, ConfigSource::User)
                .unwrap_or_else(|error| panic!("{endpoint} should be accepted: {error}"));
        }
    }

    #[test]
    fn a_hostname_that_merely_looks_loopback_is_refused() {
        // The trap in the other direction, and the reason the check parses an address
        // rather than matching a prefix: `starts_with("127.")` would have accepted a
        // hostname an attacker controls.
        for endpoint in [
            "http://127.evil.example",
            "http://127.0.0.1.evil.example",
            "http://localhost.evil.example",
            "http://[::1].evil.example",
            "http://[::1",
            "http://[::1]:evil",
            "http://user@127.0.0.1",
            "http://localhost:80@evil.example/v1",
        ] {
            let text = format!("[providers.p]\nendpoint = \"{endpoint}\"\n");
            assert!(
                parse_and_validate(&text, ConfigSource::User).is_err(),
                "{endpoint} must not pass as loopback"
            );
        }
    }

    #[test]
    fn authority_parsing_isolates_the_host() {
        assert_eq!(authority_host("localhost:8080/v1"), "localhost");
        assert_eq!(authority_host("[::1]:8080/v1"), "::1");
        assert_eq!(authority_host("[::1]"), "::1");
        assert_eq!(authority_host("127.0.0.1"), "127.0.0.1");
        assert_eq!(authority_host("host/path:with:colons"), "host");
        assert_eq!(authority_host("host?query=:"), "host");
        assert_eq!(authority_host(""), "");

        // Malformed authorities yield nothing, which no loopback test accepts. The
        // second of these was a bypass in the first version of this fix.
        assert_eq!(authority_host("[::1"), "", "an unclosed bracket is not a host");
        assert_eq!(authority_host("[::1].evil.example"), "", "trailing junk after `]`");
        assert_eq!(authority_host("[::1]:evil"), "", "a non-numeric port");
        assert_eq!(authority_host("[::1]:"), "", "an empty port");
        assert_eq!(authority_host("localhost:80@evil.example"), "", "userinfo is unsupported");
    }

    #[test]
    fn loopback_recognition_matches_the_standard_library() {
        assert!(is_loopback_authority("localhost"));
        assert!(is_loopback_authority("LOCALHOST"), "a hostname is case-insensitive");
        assert!(is_loopback_authority("127.0.0.1"));
        assert!(is_loopback_authority("127.255.255.254"));
        assert!(is_loopback_authority("::1"));

        assert!(!is_loopback_authority("128.0.0.1"));
        assert!(!is_loopback_authority("127.evil.example"));
        assert!(!is_loopback_authority(""));
        assert!(!is_loopback_authority("::2"));
    }

    #[test]
    fn redaction_counts_characters_not_bytes() {
        // A real bug, found by an audit after T7 shipped. The guard was on `len()` -
        // bytes - while the truncation took characters, so a two-character CJK value was
        // six bytes, passed the guard, and was then echoed in full by the message whose
        // whole purpose is not to echo it.
        let two_chars = "\u{4E2D}\u{6587}";
        assert_eq!(two_chars.len(), 6, "six bytes, two characters");
        assert_eq!(redact(two_chars), "\u{2026}", "a short value reveals nothing at all");

        // And a longer one keeps at most four characters, never more.
        let five_chars = "\u{4E2D}\u{6587}\u{6E2C}\u{8A66}\u{5024}";
        let shortened = redact(five_chars);
        assert!(!shortened.contains(five_chars), "the whole value leaked: {shortened}");
        assert!(shortened.contains("(5 characters)"), "{shortened}");
        assert_eq!(
            shortened.chars().take_while(|c| *c != '\u{2026}').count(),
            4,
            "exactly four characters of head: {shortened}"
        );
    }

    #[test]
    fn a_multibyte_value_is_not_echoed_by_the_validator() {
        // The same property through the public path that produces the message.
        let value = "\u{4E2D}\u{6587}";
        let text = format!("[providers.p]\napi_key_env = \"{value}\"\n");
        let error = parse_and_validate(&text, ConfigSource::User).expect_err("not a var name");
        assert!(!error.to_string().contains(value), "the value leaked: {error}");
    }

    #[test]
    fn redaction_keeps_the_field_identifiable_and_the_value_not() {
        assert_eq!(redact("ab"), "\u{2026}");
        let long = redact("sk-0123456789");
        assert!(long.starts_with("sk-0"), "{long}");
        assert!(long.contains("13 characters"), "{long}");
        assert!(!long.contains("456789"), "{long}");
    }
}
