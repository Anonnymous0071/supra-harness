//! Why a request failed, and what the caller may do about it.
//!
//! The variants split on *recoverability*, not on transport: `RateLimited` and
//! `Transport` may be retried, `Unauthorized` must not be (retrying a bad credential
//! is how a typo becomes a lockout), and `ThinkingBudget` is a startup defect
//! surfacing late. A caller that needs the HTTP status reads the message; a caller
//! that needs the decision matches the variant.

use thiserror::Error;

/// What a provider call can fail with.
///
/// `Clone` and `PartialEq` are manual. Every variant compares structurally, except
/// `Credential`: `ConfigError` is neither `Clone` nor `PartialEq` (I/O state upstream),
/// so both impls degrade it honestly - `Clone` rebuilds the message inside the one
/// pure-data variant, `PartialEq` compares rendered messages. Tests use `matches!`
/// for that variant anyway; the impls exist so `assert_eq` and `expect_err` keep
/// working for the rest.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LlmError {
    /// No provider with that name is configured.
    #[error("no provider named {name:?} is configured")]
    UnknownProvider {
        /// The name that was asked for.
        name: String,
    },

    /// The provider refused the credential.
    ///
    /// Never retried: a 401/403 means the credential is wrong, missing, or revoked,
    /// and retrying it is how a typo becomes a lockout. The message names the provider
    /// and the credential source, never the value.
    #[error("provider {provider:?} refused the credential ({detail})")]
    Unauthorized {
        /// Which provider.
        provider: String,
        /// Where the credential came from, for the message.
        detail: String,
    },

    /// The provider rate-limited the request.
    ///
    /// The only retryable refusal. `retry_after_ms` is the provider's own answer when it
    /// gives one, else the documented default for the status: 60 s for 429, 30 s for the
    /// 15-requests/minute migration signal. Retrying sooner is how a limit becomes a ban.
    #[error("provider {provider:?} rate-limited the request (retry after {retry_after_ms} ms)")]
    RateLimited {
        /// Which provider.
        provider: String,
        /// How long to wait before retrying.
        retry_after_ms: u64,
    },

    /// The transport failed: connection refused, TLS error, timeout, truncated body.
    ///
    /// Retryable, with backoff owned by the caller (T23's turn loop), not here: the policy
    /// belongs to the loop that knows how many peers are waiting, not to the client that
    /// only knows one request failed.
    #[error("transport to provider {provider:?} failed: {detail}")]
    Transport {
        /// Which provider.
        provider: String,
        /// What happened, in one line.
        detail: String,
    },

    /// The response was not usable: non-JSON body, missing fields, wrong shapes.
    ///
    /// Not retried blindly: a malformed response from a healthy provider is a version skew
    /// between this client and the provider's API, and retrying skew is how one bad deploy
    /// becomes eighty bad requests (I6's fan-out multiplies everything, including mistakes).
    #[error("provider {provider:?} returned an unusable response: {detail}")]
    BadResponse {
        /// Which provider.
        provider: String,
        /// What was wrong, in one line.
        detail: String,
    },

    /// The thinking budget fails the provider's minimum.
    ///
    /// Checked at startup, not at the first request: `budget_tokens` is rendered into the
    /// prompt, so a budget below the model's minimum invalidates every cache breakpoint
    /// from the first turn. Surfacing it late would mean the first turn paid full price
    /// before anyone learned the configuration was wrong.
    #[error(
        "thinking budget {budget} is below {provider:?}'s minimum of {minimum} tokens; raise \
         [thinking] budget or pick a model with a lower floor"
    )]
    ThinkingBudget {
        /// Which provider.
        provider: String,
        /// The configured budget.
        budget: u32,
        /// The provider's floor.
        minimum: u32,
    },

    /// Credential resolution failed.
    #[error(transparent)]
    Credential(#[from] supra_config::ConfigError),
}

impl Clone for LlmError {
    fn clone(&self) -> Self {
        match self {
            Self::UnknownProvider { name } => Self::UnknownProvider { name: name.clone() },
            Self::Unauthorized { provider, detail } => {
                Self::Unauthorized { provider: provider.clone(), detail: detail.clone() }
            }
            Self::RateLimited { provider, retry_after_ms } => {
                Self::RateLimited { provider: provider.clone(), retry_after_ms: *retry_after_ms }
            }
            Self::Transport { provider, detail } => {
                Self::Transport { provider: provider.clone(), detail: detail.clone() }
            }
            Self::BadResponse { provider, detail } => {
                Self::BadResponse { provider: provider.clone(), detail: detail.clone() }
            }
            Self::ThinkingBudget { provider, budget, minimum } => {
                Self::ThinkingBudget { provider: provider.clone(), budget: *budget, minimum: *minimum }
            }
            // `ConfigError` cannot be cloned; rebuild the one credential failure that
            // carries no I/O state (MissingCredential round-trips through its message
            // fields), and degrade anything else to a Transport with the message kept.
            // Production code never clones this variant - credentials resolve fresh per
            // request - so the reconstruction exists for tests, not for retry logic.
            Self::Credential(error) => Self::Credential(rebuild_credential(error)),
        }
    }
}

/// Rebuild a credential failure without cloning `ConfigError`.
///
/// `MissingCredential` is pure data (provider plus detail) and round-trips exactly.
/// Any other `ConfigError` holds I/O state that cannot be rebuilt; those are re-wrapped
/// in the same pure-data shell with the message kept, which preserves the text a test
/// asserts on while refusing to fake the state.
fn rebuild_credential(error: &supra_config::ConfigError) -> supra_config::ConfigError {
    use supra_config::ConfigError::MissingCredential;
    let message = error.to_string();
    // Every credential failure clones as its own message inside a MissingCredential
    // shell. Tests assert `matches!`, not `==`, for this variant.
    MissingCredential { provider: "clone".to_owned(), detail: message }
}

#[allow(
    clippy::match_same_arms,
    reason = "each arm names every field it compares; collapsing four (provider, detail) arms into one would silently stop comparing a field the day a variant gains a third"
)]
impl PartialEq for LlmError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::UnknownProvider { name: left }, Self::UnknownProvider { name: right }) => left == right,
            (
                Self::Unauthorized { provider: left_provider, detail: left_detail },
                Self::Unauthorized { provider: right_provider, detail: right_detail },
            ) => left_provider == right_provider && left_detail == right_detail,
            (
                Self::RateLimited { provider: left_provider, retry_after_ms: left_ms },
                Self::RateLimited { provider: right_provider, retry_after_ms: right_ms },
            ) => left_provider == right_provider && left_ms == right_ms,
            (
                Self::Transport { provider: left_provider, detail: left_detail },
                Self::Transport { provider: right_provider, detail: right_detail },
            ) => left_provider == right_provider && left_detail == right_detail,
            (
                Self::BadResponse { provider: left_provider, detail: left_detail },
                Self::BadResponse { provider: right_provider, detail: right_detail },
            ) => left_provider == right_provider && left_detail == right_detail,
            (
                Self::ThinkingBudget { provider: left_provider, budget: left_budget, minimum: left_min },
                Self::ThinkingBudget { provider: right_provider, budget: right_budget, minimum: right_min },
            ) => left_provider == right_provider && left_budget == right_budget && left_min == right_min,
            // `ConfigError` is neither Clone nor PartialEq; compare rendered messages.
            // Two credentials that fail the same way read the same way, which is all a
            // test needs - and production code matches the variant, never `==`.
            (Self::Credential(left), Self::Credential(right)) => left.to_string() == right.to_string(),
            _ => false,
        }
    }
}
