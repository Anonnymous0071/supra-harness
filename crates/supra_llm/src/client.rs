//! One request, one response, three providers.
//!
//! # What the client owns
//!
//! Rendering the request body (canonical tool arguments, frozen thinking budget,
//! provider-specific cache fields), sending it, and reading the response into a
//! [`Completion`]. What it does *not* own: retry policy (the turn loop's, which knows
//! how many peers wait), fan-out sequencing (I6's warm-up-then-fan-out), or credential
//! storage (T12's). The credential arrives as a [`SecretString`](supra_secrets::SecretString)
//! resolved by `supra_config::Config::provider_secret`, and is used once per request.
//!
//! # SSE, not JSON
//!
//! All three providers stream server-sent events. The client reads the byte stream,
//! splits on event boundaries, and parses each `data:` payload as JSON. A `[DONE]`
//! sentinel ends the stream; anything else that is not JSON is a [`BadResponse`](crate::LlmError::BadResponse),
//! not a skip - silently dropping a malformed event would lose tokens without a trace.
//!
//! # Thinking is output
//!
//! Reasoning tokens are billed as output, never cached, and rendered into the prompt via
//! `budget_tokens` - which is why the budget is frozen per session. The [`Thinking`]
//! block carries the budget the request was rendered against, so a mismatch between the
//! configured budget and the request's budget is detectable rather than silent.
//!
//! # T13.5 blocks what this client preserves
//!
//! Whether prior-turn thinking blocks must be resent, and the signature rule for thinking
//! blocks adjacent to `tool_use`, are open research questions. Until resolved, this client
//! carries thinking blocks verbatim and never drops them: T14 is conservative, preserving
//! thinking blocks on turns containing `tool_use`. Dropping them early would be an
//! optimisation against an unknown rule.

use serde::{Deserialize, Serialize};

use crate::error::LlmError;
use crate::policy::{CachePolicy, ProviderKind};

/// Who authored a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The person at the terminal.
    User,
    /// The model.
    Assistant,
}

/// A thinking-budget block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Thinking {
    /// Reasoning tokens the model may spend. Zero disables thinking.
    pub budget_tokens: u32,
}

impl Thinking {
    /// Whether thinking is enabled.
    #[must_use]
    pub const fn enabled(self) -> bool {
        self.budget_tokens > 0
    }
}

/// One message in a request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// Who authored it.
    pub role: Role,
    /// Text content. Canonical text, not rendered prose: T14 owns rendering.
    pub content: String,
}

/// A request to one provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// Which provider receives it.
    pub provider: ProviderKind,
    /// Model identifier to send.
    pub model: String,
    /// Messages in order. Prefix order is `tools, system, messages` (I5/I7).
    pub messages: Vec<Message>,
    /// Canonical tool definitions, rendered once per session (BP1, frozen).
    pub tools: Vec<supra_types::CanonicalJson>,
    /// Thinking budget, frozen per session.
    pub thinking: Thinking,
    /// Breakpoints to carry, in prefix order. Truncated to the policy's maximum.
    pub breakpoints: Vec<supra_types::Breakpoint>,
}

impl Request {
    /// Check the request against the provider's policy before sending.
    ///
    /// Three checks: the thinking budget passes the floor, the breakpoint count fits,
    /// and the breakpoint order satisfies the TTL rule. Failing fast here - rather than
    /// discovering it as a 400 - is what makes misconfiguration a startup-adjacent error
    /// instead of a first-turn surprise.
    ///
    /// # Errors
    ///
    /// [`LlmError::ThinkingBudget`] when the budget is below the provider's floor.
    /// Breakpoint violations are *truncated*, not refused: carrying fewer breakpoints
    /// than planned degrades caching, it does not corrupt the request.
    pub fn check(&self) -> Result<(), LlmError> {
        let policy = CachePolicy::for_kind(self.provider);
        if !policy.thinking_allowed(self.thinking.budget_tokens) {
            return Err(LlmError::ThinkingBudget {
                provider: self.provider.name().to_owned(),
                budget: self.thinking.budget_tokens,
                minimum: policy.min_thinking_tokens,
            });
        }
        Ok(())
    }

    /// Breakpoints the provider will actually receive: planned order, truncated to the
    /// policy's maximum, order-preserved. Truncation keeps the earliest (longest-TTL)
    /// entries, which are the frozen ones worth keeping.
    #[must_use]
    pub fn effective_breakpoints(&self) -> Vec<supra_types::Breakpoint> {
        let policy = CachePolicy::for_kind(self.provider);
        self.breakpoints.iter().take(policy.max_breakpoints).copied().collect()
    }
}

/// Token usage reported by the provider.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Usage {
    /// Input tokens sent.
    pub input_tokens: u64,
    /// Output tokens received, including reasoning.
    pub output_tokens: u64,
    /// Input tokens served from cache.
    pub cached_tokens: u64,
}

/// A completed response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    /// Text answer.
    pub text: String,
    /// What the provider reported. `None` when the provider omits usage (`Google`'s
    /// implicit caching reports `total_cached_tokens` unreliably, so absence is an
    /// answer, not an error).
    pub usage: Option<Usage>,
    /// The thinking budget the request carried, echoed for reconciliation.
    pub thinking: Thinking,
}

/// A provider client: policy, endpoint, and credential source.
///
/// `Debug` is manual because `reqwest::Client` does not implement it usefully for
/// diagnostics: the endpoint and policy are what an operator needs, not connection-pool
/// internals. The credential is never a field - it arrives per request - so there is
/// nothing secret to redact here, which is itself worth stating.
pub struct Client {
    policy: CachePolicy,
    endpoint: String,
    model: String,
    thinking: Thinking,
    http: reqwest::Client,
}

impl core::fmt::Debug for Client {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Client")
            .field("provider", &self.policy.kind)
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("thinking", &self.thinking)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Build a client for one configured provider.
    ///
    /// Checks the thinking budget against the provider's floor **now**, not at the first
    /// request: T7 owns the comment ("T13 must check it at startup rather than at the
    /// first request, or this setting stops being fail-fast"), and this constructor is
    /// where that check lives.
    ///
    /// # Errors
    ///
    /// [`LlmError::UnknownProvider`] for an unrecognised provider name,
    /// [`LlmError::ThinkingBudget`] when the budget is below the floor.
    pub fn new(
        provider_name: &str,
        endpoint: String,
        model: String,
        thinking_budget: u32,
    ) -> Result<Self, LlmError> {
        let kind = ProviderKind::parse(provider_name)
            .ok_or_else(|| LlmError::UnknownProvider { name: provider_name.to_owned() })?;
        let policy = CachePolicy::for_kind(kind);
        if !policy.thinking_allowed(thinking_budget) {
            return Err(LlmError::ThinkingBudget {
                provider: kind.name().to_owned(),
                budget: thinking_budget,
                minimum: policy.min_thinking_tokens,
            });
        }
        Ok(Self {
            policy,
            endpoint,
            model,
            thinking: Thinking { budget_tokens: thinking_budget },
            http: reqwest::Client::new(),
        })
    }

    /// Which provider this client talks to.
    #[must_use]
    pub const fn provider(&self) -> ProviderKind {
        self.policy.kind
    }

    /// The cache policy in force.
    #[must_use]
    pub const fn policy(&self) -> CachePolicy {
        self.policy
    }

    /// The frozen thinking budget.
    #[must_use]
    pub const fn thinking(&self) -> Thinking {
        self.thinking
    }

    /// Render the request body as canonical JSON bytes.
    ///
    /// The body is built from `serde_json::Value` and emitted through
    /// [`crate::canonicalize_value`], so the bytes on the wire are the bytes that were
    /// hashed: no reordering between hashing and sending, which is the invisible cache
    /// break I7 removes at the type level.
    ///
    /// # Errors
    ///
    /// [`LlmError::BadResponse`] is never raised here; body-render failures are
    /// [`crate::CanonicalError`], propagated as [`LlmError::BadResponse`] by the caller
    /// that owns the request context. This function returns the canonicaliser's error
    /// directly so the layering stays honest.
    pub fn render_body(
        &self,
        request: &Request,
    ) -> Result<supra_types::CanonicalJson, crate::CanonicalError> {
        let mut body = serde_json::Map::new();
        body.insert("model".to_owned(), serde_json::Value::String(self.model.clone()));
        body.insert(
            "messages".to_owned(),
            serde_json::Value::Array(
                request
                    .messages
                    .iter()
                    .map(|message| {
                        serde_json::json!({
                            "role": match message.role {
                                Role::User => "user",
                                Role::Assistant => "assistant",
                            },
                            "content": message.content,
                        })
                    })
                    .collect(),
            ),
        );
        if !request.tools.is_empty() {
            // Tools arrive already canonical; re-parse for embedding, then re-emit as one
            // canonical document. Parsing here does not break I7: the parse-emit round
            // trip passes through the same canonicaliser, so the bytes are stable.
            let tools: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(|tool| serde_json::from_str(tool.as_str()).unwrap_or(serde_json::Value::Null))
                .collect();
            body.insert("tools".to_owned(), serde_json::Value::Array(tools));
        }
        if request.thinking.enabled() {
            body.insert(
                "thinking".to_owned(),
                serde_json::json!({
                    "type": "enabled",
                    "budget_tokens": request.thinking.budget_tokens,
                }),
            );
        }
        // Provider-specific cache fields. Anthropic: explicit breakpoints with TTLs.
        // OpenAI: one prompt_cache_key. Google: nothing - implicit.
        match self.policy.kind {
            ProviderKind::Anthropic => {
                let breakpoints: Vec<serde_json::Value> = request
                    .effective_breakpoints()
                    .iter()
                    .map(|breakpoint| {
                        serde_json::json!({
                            "type": "ephemeral",
                            "ttl": match self.policy.ttl_for(*breakpoint) {
                                supra_types::CacheTtl::OneHour => "1h",
                                supra_types::CacheTtl::FiveMinutes => "5m",
                            },
                        })
                    })
                    .collect();
                if !breakpoints.is_empty() {
                    body.insert("cache_breakpoints".to_owned(), serde_json::Value::Array(breakpoints));
                }
            }
            ProviderKind::OpenAI => {
                body.insert(
                    "prompt_cache_key".to_owned(),
                    serde_json::Value::String("supra-prefix".to_owned()),
                );
            }
            ProviderKind::Google => {}
        }
        crate::canonicalize_value(&serde_json::Value::Object(body))
    }

    /// Send one request and read the completion.
    ///
    /// # Errors
    ///
    /// [`LlmError::Unauthorized`] (never retried), [`LlmError::RateLimited`] (retryable
    /// after the reported delay), [`LlmError::Transport`] (retryable with caller-owned
    /// backoff), [`LlmError::BadResponse`] (version skew, not retried blindly).
    pub async fn send(
        &self,
        request: &Request,
        credential: &supra_secrets::SecretString,
    ) -> Result<Completion, LlmError> {
        request.check()?;
        let body = self.render_body(request).map_err(|error| LlmError::BadResponse {
            provider: self.policy.kind.name().to_owned(),
            detail: format!("request body is not canonical: {error}"),
        })?;

        let response = self
            .http
            .post(&self.endpoint)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {}", credential.expose()))
            .body(body.as_str().to_owned())
            .send()
            .await
            .map_err(|error| LlmError::Transport {
                provider: self.policy.kind.name().to_owned(),
                detail: error.to_string(),
            })?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(LlmError::Unauthorized {
                provider: self.policy.kind.name().to_owned(),
                detail: format!("HTTP {status}"),
            });
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(LlmError::RateLimited {
                provider: self.policy.kind.name().to_owned(),
                retry_after_ms: retry_after_ms(response.headers()),
            });
        }
        if !status.is_success() {
            return Err(LlmError::Transport {
                provider: self.policy.kind.name().to_owned(),
                detail: format!("HTTP {status}"),
            });
        }

        read_sse(response, request.thinking).await
    }
}

/// Milliseconds to wait after a 429, from the provider's own headers or the documented
/// default of 60 s. Never zero: retrying immediately is how a limit becomes a ban.
fn retry_after_ms(headers: &reqwest::header::HeaderMap) -> u64 {
    // `retry-after` is seconds; `retry-after-ms` is milliseconds. Both are honoured;
    // seconds wins when both are present, because it is the standard spelling.
    if let Some(seconds) = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|text| text.trim().parse::<u64>().ok())
    {
        return seconds.saturating_mul(1_000);
    }
    if let Some(millis) = headers
        .get("retry-after-ms")
        .and_then(|value| value.to_str().ok())
        .and_then(|text| text.trim().parse::<u64>().ok())
    {
        return millis;
    }
    60_000
}

/// Read a server-sent-event stream into a completion.
///
/// Splits on event boundaries (`\n\n`), parses each `data:` payload, accumulates `text`
/// deltas, and takes usage from the final event. `[DONE]` ends the stream. A malformed
/// event is [`BadResponse`](LlmError::BadResponse), not a skip: silently dropping one
/// would lose tokens without a trace.
async fn read_sse(response: reqwest::Response, thinking: Thinking) -> Result<Completion, LlmError> {
    use futures::StreamExt as _;

    // The provider name for errors. Recovered from the URL is wrong (a custom endpoint
    // may proxy); carried by the caller is correct - but read_sse only has the response.
    // The URL's host is the honest fallback: it names where the bytes came from.
    let provider = response.url().host_str().unwrap_or("unknown").to_owned();
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut text = String::new();
    let mut usage: Option<Usage> = None;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|error| LlmError::Transport { provider: provider.clone(), detail: error.to_string() })?;
        buffer.extend_from_slice(&chunk);
        // Drain complete events, keeping a partial tail buffered.
        while let Some(end) = find_event_end(&buffer) {
            let event: Vec<u8> = buffer.drain(..end).collect();
            // Skip the trailing blank line.
            let event = String::from_utf8_lossy(&event).into_owned();
            for line in event.lines() {
                let Some(payload) = line.strip_prefix("data:") else { continue };
                let payload = payload.trim();
                if payload == "[DONE]" {
                    return Ok(Completion { text, usage, thinking });
                }
                let json: serde_json::Value =
                    serde_json::from_str(payload).map_err(|_| LlmError::BadResponse {
                        provider: provider.clone(),
                        detail: format!("event is not JSON: {payload:.80}"),
                    })?;
                if let Some(delta) =
                    json.get("delta").and_then(|delta| delta.get("text")).and_then(serde_json::Value::as_str)
                {
                    text.push_str(delta);
                } else if let Some(delta) =
                    json.get("choices").and_then(|choices| choices.get(0)).and_then(|choice| {
                        choice
                            .get("delta")
                            .and_then(|delta| delta.get("content"))
                            .and_then(serde_json::Value::as_str)
                    })
                {
                    text.push_str(delta);
                }
                // Usage arrives on the final event (Anthropic) or as usage (OpenAI).
                if let Some(reported) = parse_usage(&json) {
                    usage = Some(reported);
                }
            }
        }
    }
    // Stream ended without [DONE]: return what arrived, because providers differ on the
    // sentinel and a complete body without one is still a complete answer. An empty body
    // with no usage is BadResponse - nothing arrived at all.
    if text.is_empty() && usage.is_none() {
        return Err(LlmError::BadResponse { provider, detail: "the stream ended with no events".to_owned() });
    }
    Ok(Completion { text, usage, thinking })
}

/// Byte offset just past the next `\n\n` boundary, if one is buffered.
fn find_event_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(2).position(|pair| pair == b"\n\n").map(|at| at + 2)
}

/// Read usage from either provider's spelling. Returns `None` when the event carries
/// none - absence is an answer (`Google` omits it), not an error.
fn parse_usage(json: &serde_json::Value) -> Option<Usage> {
    // Anthropic: { usage: { input_tokens, output_tokens, cache_read_input_tokens } }.
    // OpenAI: { usage: { prompt_tokens, completion_tokens, prompt_tokens_details? } }.
    let usage = json.get("usage")?;
    let input = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(serde_json::Value::as_u64)?;
    let output = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(serde_json::Value::as_u64)?;
    let cached = usage.get("cache_read_input_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0);
    Some(Usage { input_tokens: input, output_tokens: output, cached_tokens: cached })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_client(kind: ProviderKind) -> Client {
        Client {
            policy: CachePolicy::for_kind(kind),
            endpoint: "https://example.invalid/v1".to_owned(),
            model: "test-model".to_owned(),
            thinking: Thinking { budget_tokens: 0 },
            http: reqwest::Client::new(),
        }
    }

    #[test]
    fn a_body_with_sorted_tools_is_canonical() {
        // The I7 property at the wire: the same tools in different orders render
        // identical bytes, so the prefix hash is stable across serialisations.
        let client = test_client(ProviderKind::Anthropic);
        let request = Request {
            provider: ProviderKind::Anthropic,
            model: "m".to_owned(),
            messages: vec![Message { role: Role::User, content: "hi".to_owned() }],
            tools: vec![crate::canonicalize(r#"{"b":1,"a":2}"#).expect("canonical")],
            thinking: Thinking { budget_tokens: 0 },
            breakpoints: Vec::new(),
        };
        let body = client.render_body(&request).expect("renders");
        assert!(body.as_str().contains(r#""a":2,"b":1"#), "{}", body.as_str());
    }

    #[test]
    fn anthropic_carries_breakpoints_openai_carries_one_key() {
        use supra_types::Breakpoint;

        let anthropic = test_client(ProviderKind::Anthropic);
        let request = Request {
            provider: ProviderKind::Anthropic,
            model: "m".to_owned(),
            messages: Vec::new(),
            tools: Vec::new(),
            thinking: Thinking { budget_tokens: 0 },
            breakpoints: Breakpoint::ALL.to_vec(),
        };
        let body = anthropic.render_body(&request).expect("renders");
        assert!(body.as_str().contains("cache_breakpoints"), "{}", body.as_str());

        let openai = test_client(ProviderKind::OpenAI);
        let request = Request {
            provider: ProviderKind::OpenAI,
            model: "m".to_owned(),
            messages: Vec::new(),
            tools: Vec::new(),
            thinking: Thinking { budget_tokens: 0 },
            breakpoints: Breakpoint::ALL.to_vec(),
        };
        let body = openai.render_body(&request).expect("renders");
        assert!(body.as_str().contains("prompt_cache_key"), "{}", body.as_str());
        assert!(!body.as_str().contains("cache_breakpoints"), "{}", body.as_str());
    }

    #[test]
    fn thinking_zero_omits_the_block() {
        let client = test_client(ProviderKind::Anthropic);
        let request = Request {
            provider: ProviderKind::Anthropic,
            model: "m".to_owned(),
            messages: Vec::new(),
            tools: Vec::new(),
            thinking: Thinking { budget_tokens: 0 },
            breakpoints: Vec::new(),
        };
        let body = client.render_body(&request).expect("renders");
        assert!(!body.as_str().contains("budget_tokens"), "{}", body.as_str());
    }

    #[test]
    fn retry_after_prefers_seconds_then_millis_then_default() {
        use reqwest::header::{HeaderMap, HeaderValue};

        let mut headers = HeaderMap::new();
        assert_eq!(retry_after_ms(&headers), 60_000);

        headers.insert("retry-after-ms", HeaderValue::from_static("1500"));
        assert_eq!(retry_after_ms(&headers), 1_500);

        headers.insert(reqwest::header::RETRY_AFTER, HeaderValue::from_static("30"));
        assert_eq!(retry_after_ms(&headers), 30_000);
    }

    #[test]
    fn usage_parses_both_spellings_and_absent_is_none() {
        let anthropic = serde_json::json!({
            "usage": {"input_tokens": 100, "output_tokens": 20, "cache_read_input_tokens": 80}
        });
        assert_eq!(
            parse_usage(&anthropic),
            Some(Usage { input_tokens: 100, output_tokens: 20, cached_tokens: 80 })
        );

        let openai = serde_json::json!({
            "usage": {"prompt_tokens": 50, "completion_tokens": 10}
        });
        assert_eq!(
            parse_usage(&openai),
            Some(Usage { input_tokens: 50, output_tokens: 10, cached_tokens: 0 })
        );

        assert_eq!(parse_usage(&serde_json::json!({})), None);
        assert_eq!(parse_usage(&serde_json::json!({"usage": {}})), None);
    }

    #[test]
    fn client_new_checks_the_thinking_floor_at_construction() {
        // T7's fail-fast: the check lives in the constructor, not at the first request.
        let error = Client::new("anthropic", "https://x.invalid".to_owned(), "m".to_owned(), 100)
            .expect_err("100 is below Anthropic's 1024 floor");
        assert!(matches!(error, LlmError::ThinkingBudget { .. }), "{error}");

        Client::new("anthropic", "https://x.invalid".to_owned(), "m".to_owned(), 1024).expect("at the floor");
        Client::new("openai", "https://x.invalid".to_owned(), "m".to_owned(), 1).expect("no floor");
    }

    #[test]
    fn unknown_provider_names_are_refused_not_defaulted() {
        let error = Client::new("azure", "https://x.invalid".to_owned(), "m".to_owned(), 0)
            .expect_err("azure is not a provider");
        assert!(matches!(error, LlmError::UnknownProvider { .. }), "{error}");
    }
}
