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
//! # T13.5 resolved: what this client preserves
//!
//! Anthropic's rules, verified from official documentation (thinking, tool-use, API
//! reference, context-editing pages):
//!
//! - **Required:** within a tool-use turn, thinking blocks must be passed back complete
//!   and unmodified, alongside the `tool_use` block they accompanied (400 otherwise).
//!   The `signature` verifies Claude generated the block; `redacted_thinking` blocks
//!   pass back unchanged the same way.
//! - **Within the latest assistant message**, consecutive thinking blocks must match
//!   generation order exactly - including `redacted_thinking`.
//! - **Recommended:** across turns, pass everything back; the API filters and bills
//!   only shown blocks.
//! - **Allowed:** outside tool use, omit prior turns' thinking (silently accepted).
//! - Keep-all models bill retained thinking as input; last-turn-only models strip
//!   automatically. Clearing via `clear_thinking_20251015` invalidates cache at the
//!   clearing point - so T14 evicts losslessly instead of asking the provider to clear.
//!
//! Until T14 lands, this client carries thinking blocks verbatim and never drops them.

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

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

/// One ordered content block in a provider message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentBlock {
    /// Visible text and any provider citations attached to it.
    Text {
        /// The text exactly as received or sent.
        text: String,
        /// Provider citation objects, retained verbatim and in order.
        citations: Vec<serde_json::Value>,
    },
    /// Claude reasoning that must be replayed with its signature unchanged.
    Thinking {
        /// Reasoning text.
        thinking: String,
        /// Provider signature authenticating the reasoning block.
        signature: String,
    },
    /// Provider-redacted reasoning that must still be replayed unchanged.
    RedactedThinking {
        /// Opaque provider payload.
        data: String,
    },
    /// A model request to invoke a tool.
    ToolUse {
        /// Provider-assigned tool-use identifier.
        id: String,
        /// Registered tool name.
        name: String,
        /// Tool input exactly as structured by the provider.
        input: serde_json::Value,
    },
    /// A host result corresponding to a prior tool use.
    ToolResult {
        /// Identifier of the tool use this answers.
        tool_use_id: String,
        /// Result content: either a string or an ordered array of provider blocks.
        content: serde_json::Value,
        /// Whether execution failed.
        is_error: bool,
    },
    /// A provider block this version does not interpret, retained for lossless replay.
    Unknown {
        /// The complete provider object.
        raw: serde_json::Value,
    },
}

impl ContentBlock {
    /// Construct a visible text block.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into(), citations: Vec::new() }
    }

    fn wire_value(&self) -> serde_json::Value {
        match self {
            Self::Text { text, citations } => {
                let mut value = serde_json::json!({"type": "text", "text": text});
                if !citations.is_empty() {
                    value["citations"] = serde_json::Value::Array(citations.clone());
                }
                value
            }
            Self::Thinking { thinking, signature } => {
                serde_json::json!({"type": "thinking", "thinking": thinking, "signature": signature})
            }
            Self::RedactedThinking { data } => {
                serde_json::json!({"type": "redacted_thinking", "data": data})
            }
            Self::ToolUse { id, name, input } => {
                serde_json::json!({"type": "tool_use", "id": id, "name": name, "input": input})
            }
            Self::ToolResult { tool_use_id, content, is_error } => {
                let mut value = serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": content,
                });
                if *is_error {
                    value["is_error"] = serde_json::Value::Bool(true);
                }
                value
            }
            Self::Unknown { raw } => raw.clone(),
        }
    }

    fn from_wire_value(value: serde_json::Value) -> Result<Self, String> {
        let kind = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "content block has no string type".to_owned())?;
        let string = |field: &str| {
            value
                .get(field)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("{kind} content block has no string {field}"))
        };
        match kind {
            "text" => Ok(Self::Text {
                text: string("text")?,
                citations: value
                    .get("citations")
                    .and_then(serde_json::Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            }),
            "thinking" => {
                Ok(Self::Thinking { thinking: string("thinking")?, signature: string("signature")? })
            }
            "redacted_thinking" => Ok(Self::RedactedThinking { data: string("data")? }),
            "tool_use" => Ok(Self::ToolUse {
                id: string("id")?,
                name: string("name")?,
                input: value.get("input").cloned().unwrap_or(serde_json::Value::Null),
            }),
            "tool_result" => Ok(Self::ToolResult {
                tool_use_id: string("tool_use_id")?,
                content: value.get("content").cloned().unwrap_or(serde_json::Value::String(String::new())),
                is_error: value.get("is_error").and_then(serde_json::Value::as_bool).unwrap_or(false),
            }),
            _ => Ok(Self::Unknown { raw: value }),
        }
    }
}

impl Serialize for ContentBlock {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.wire_value().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ContentBlock {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        Self::from_wire_value(value).map_err(D::Error::custom)
    }
}

/// One message in a request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// Who authored it.
    pub role: Role,
    /// Ordered content. Replay-required thinking and tool blocks retain their exact order.
    pub content: Vec<ContentBlock>,
}

impl Message {
    /// Construct a one-block text message.
    #[must_use]
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self { role, content: vec![ContentBlock::text(text)] }
    }

    /// Join only visible text blocks in their original order.
    #[must_use]
    pub fn text_content(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
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

/// Why a provider stopped generating.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The model completed the turn normally.
    EndTurn,
    /// The model emitted tool-use blocks and requires results.
    ToolUse,
    /// The configured output-token limit truncated the answer.
    MaxTokens,
    /// A configured stop sequence ended generation.
    StopSequence,
    /// Claude paused a long-running server-tool turn and expects continuation.
    PauseTurn,
    /// Claude refused the request.
    Refusal,
    /// The model's context window was exceeded while generating.
    ModelContextWindowExceeded,
    /// The provider removed content under its safety policy.
    ContentFilter,
    /// A provider-specific terminal reason not otherwise classified.
    Other(String),
}

/// A completed provider response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Completion {
    /// Ordered assistant blocks, including replay-required thinking and tool use.
    pub content: Vec<ContentBlock>,
    /// Why generation stopped. Callers continue `ToolUse` and must not seal `MaxTokens`.
    pub stop_reason: StopReason,
    /// Custom stop sequence, when [`StopReason::StopSequence`] applies.
    pub stop_sequence: Option<String>,
    /// Model identifier reported by the provider.
    pub model: String,
    /// Provider request identifier, when available.
    pub request_id: Option<String>,
    /// What the provider reported. `None` when the provider omits usage (`Google`'s
    /// implicit caching reports `total_cached_tokens` unreliably, so absence is an
    /// answer, not an error).
    pub usage: Option<Usage>,
    /// The thinking budget the request carried, echoed for reconciliation.
    pub thinking: Thinking,
}

impl Completion {
    /// Join visible text blocks in generation order.
    #[must_use]
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Construct a terminal text completion for non-provider callers and tests.
    #[must_use]
    pub fn end_turn(text: impl Into<String>, usage: Option<Usage>, thinking: Thinking) -> Self {
        Self {
            content: vec![ContentBlock::text(text)],
            stop_reason: StopReason::EndTurn,
            stop_sequence: None,
            model: String::new(),
            request_id: None,
            usage,
            thinking,
        }
    }
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
    /// An empty `model` selects the provider's current cheapest default
    /// ([`DEFAULT_OPENAI_MODEL`](crate::providers::DEFAULT_OPENAI_MODEL) /
    /// [`DEFAULT_ANTHROPIC_MODEL`](crate::providers::DEFAULT_ANTHROPIC_MODEL));
    /// a pinned model is sent as-is. An empty `endpoint` keeps the SDK's
    /// own default base; a pinned endpoint overrides it (custom gateway).
    ///
    /// Use [`Client::from_config`] when the endpoint, model, and budget
    /// come from a resolved [`Config`](supra_config::Config) instead of
    /// literals: it reads all three from the provider entry, so a
    /// `[providers.<name>]` table fully drives the client it names.
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
        let model = if model.trim().is_empty() {
            match kind {
                ProviderKind::Anthropic => crate::providers::DEFAULT_ANTHROPIC_MODEL.to_owned(),
                ProviderKind::OpenAI => crate::providers::DEFAULT_OPENAI_MODEL.to_owned(),
                ProviderKind::Google => model,
            }
        } else {
            model
        };
        Ok(Self {
            policy,
            endpoint,
            model,
            thinking: Thinking { budget_tokens: thinking_budget },
            http: reqwest::Client::new(),
        })
    }

    /// Build a client for one provider named in a resolved configuration.
    ///
    /// Endpoint, model, and thinking budget come from the provider entry
    /// and the resolved scalars; the credential stays out — it resolves
    /// per request through
    /// [`Config::provider_secret`](supra_config::Config::provider_secret),
    /// exactly as [`Client::send`] already does.
    ///
    /// # Errors
    ///
    /// As [`Client::new`].
    pub fn from_config(config: &supra_config::Config, provider_name: &str) -> Result<Self, LlmError> {
        ProviderKind::parse(provider_name)
            .ok_or_else(|| LlmError::UnknownProvider { name: provider_name.to_owned() })?;
        let providers = config.providers();
        let entry = providers.get(provider_name);
        Self::new(
            provider_name,
            entry.and_then(|provider| provider.endpoint.clone()).unwrap_or_default(),
            entry.and_then(|provider| provider.model.clone()).unwrap_or_default(),
            config.thinking_budget(),
        )
    }

    /// Send one request through the client, resolving the credential
    /// from the configuration on the way.
    ///
    /// This is the one-call path a turn drives: provider entry for the
    /// request's kind, secret from the named source, SDK transport
    /// underneath. The request's own provider field selects the entry;
    /// the client's policy still gates thinking and breakpoints.
    ///
    /// # Errors
    ///
    /// [`LlmError::Credential`] when the named source has nothing to
    /// give; otherwise as [`Client::send`].
    pub async fn send_with_config(
        &self,
        config: &supra_config::Config,
        manager: &supra_secrets::SecretManager,
        request: &Request,
    ) -> Result<Completion, LlmError> {
        let credential =
            config.provider_secret(request.provider.name(), manager).map_err(LlmError::Credential)?;
        self.send(request, &credential).await
    }

    /// The model identifier this client sends.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The configured endpoint override, if any.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
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
        match self.policy.kind {
            ProviderKind::Anthropic => Self::render_anthropic(request, &mut body),
            ProviderKind::OpenAI => Self::render_openai(request, &mut body),
            ProviderKind::Google => Self::render_google(request, &mut body),
        }
        crate::canonicalize_value(&serde_json::Value::Object(body))
    }

    fn render_anthropic(request: &Request, body: &mut serde_json::Map<String, serde_json::Value>) {
        body.insert("max_tokens".to_owned(), serde_json::Value::from(32_000));
        body.insert("stream".to_owned(), serde_json::Value::Bool(true));
        body.insert("messages".to_owned(), Self::messages_value(request));
        if !request.tools.is_empty() {
            if let Ok(tools) = Self::tools_value(request) {
                body.insert("tools".to_owned(), tools);
            }
        }
        if request.thinking.enabled() {
            let thinking = if anthropic_supports_adaptive_thinking(
                body.get("model").and_then(serde_json::Value::as_str).unwrap_or_default(),
            ) {
                serde_json::json!({"type": "adaptive"})
            } else {
                serde_json::json!({
                    "type": "enabled",
                    "budget_tokens": request.thinking.budget_tokens,
                })
            };
            body.insert("thinking".to_owned(), thinking);
        }
        Self::apply_anthropic_cache_controls(request, body);
    }

    fn apply_anthropic_cache_controls(
        request: &Request,
        body: &mut serde_json::Map<String, serde_json::Value>,
    ) {
        let policy = CachePolicy::for_kind(ProviderKind::Anthropic);
        for breakpoint in request.effective_breakpoints() {
            let cache_control = serde_json::json!({
                "type": "ephemeral",
                "ttl": match policy.ttl_for(breakpoint) {
                    supra_types::CacheTtl::OneHour => "1h",
                    supra_types::CacheTtl::FiveMinutes => "5m",
                },
            });
            match breakpoint {
                supra_types::Breakpoint::Bp1Tools => {
                    if let Some(tool) = body
                        .get_mut("tools")
                        .and_then(serde_json::Value::as_array_mut)
                        .and_then(|tools| tools.last_mut())
                        .and_then(serde_json::Value::as_object_mut)
                    {
                        tool.insert("cache_control".to_owned(), cache_control);
                    }
                }
                supra_types::Breakpoint::Bp2System | supra_types::Breakpoint::Bp3MemoryIndex => {
                    // `Request` does not carry system/memory blocks separately yet. Do not
                    // invent a top-level cache-control shape the API does not accept.
                }
                supra_types::Breakpoint::Bp4PreviousTurn => {
                    if let Some(block) = body
                        .get_mut("messages")
                        .and_then(serde_json::Value::as_array_mut)
                        .and_then(|messages| messages.last_mut())
                        .and_then(|message| message.get_mut("content"))
                        .and_then(serde_json::Value::as_array_mut)
                        .and_then(|content| content.last_mut())
                        .and_then(serde_json::Value::as_object_mut)
                    {
                        block.insert("cache_control".to_owned(), cache_control);
                    }
                }
            }
        }
    }

    fn render_openai(request: &Request, body: &mut serde_json::Map<String, serde_json::Value>) {
        body.insert("messages".to_owned(), Self::messages_value(request));
        if !request.tools.is_empty() {
            if let Ok(tools) = Self::tools_value(request) {
                body.insert("tools".to_owned(), tools);
            }
        }
        if request.thinking.enabled() {
            body.insert("reasoning_effort".to_owned(), serde_json::Value::String("medium".to_owned()));
        }
        body.insert("prompt_cache_key".to_owned(), serde_json::Value::String("supra-prefix".to_owned()));
    }

    fn render_google(request: &Request, body: &mut serde_json::Map<String, serde_json::Value>) {
        let contents: Vec<serde_json::Value> = request
            .messages
            .iter()
            .map(|message| {
                let parts: Vec<serde_json::Value> = message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text, .. } => Some(serde_json::json!({"text": text})),
                        ContentBlock::ToolUse { name, input, .. } => {
                            Some(serde_json::json!({"functionCall": {"name": name, "args": input}}))
                        }
                        ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                            Some(serde_json::json!({"functionResponse": {
                                "name": tool_use_id,
                                "response": {"content": content, "is_error": is_error},
                            }}))
                        }
                        ContentBlock::Thinking { .. }
                        | ContentBlock::RedactedThinking { .. }
                        | ContentBlock::Unknown { .. } => None,
                    })
                    .collect();
                serde_json::json!({
                    "role": match message.role {
                        Role::User => "user",
                        Role::Assistant => "model",
                    },
                    "parts": parts,
                })
            })
            .collect();
        body.insert("contents".to_owned(), serde_json::Value::Array(contents));
        if !request.tools.is_empty() {
            if let Ok(tools) = Self::tools_value(request) {
                body.insert("tools".to_owned(), serde_json::json!([{"function_declarations": tools}]));
            }
        }
        if request.thinking.enabled() {
            body.insert(
                "generationConfig".to_owned(),
                serde_json::json!({
                    "thinkingConfig": {"thinkingBudget": request.thinking.budget_tokens},
                }),
            );
        }
    }

    fn messages_value(request: &Request) -> serde_json::Value {
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
        )
    }

    fn tools_value(request: &Request) -> Result<serde_json::Value, crate::CanonicalError> {
        let mut tools = Vec::new();
        for tool in &request.tools {
            let parsed: serde_json::Value = serde_json::from_str(tool.as_str())
                .map_err(|error| crate::CanonicalError::Invalid { detail: error.to_string() })?;
            tools.push(parsed);
        }
        Ok(serde_json::Value::Array(tools))
    }

    /// Send one request and read the completion.
    ///
    /// Anthropic uses the canonical request bytes produced here directly;
    /// `OpenAI` travels through its official SDK. Google keeps the
    /// hand-rolled transport (no official Rust SDK exists), with the same
    /// body rendering, status mapping, and SSE reading as before. The error
    /// contract is identical on every path.
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
        match self.policy.kind {
            ProviderKind::Anthropic => {
                crate::providers::send_anthropic(
                    self.http.clone(),
                    request,
                    sdk_endpoint(&self.endpoint),
                    &self.model,
                    request.thinking,
                    credential,
                )
                .await
            }
            ProviderKind::OpenAI => {
                crate::providers::send_openai(
                    request,
                    sdk_endpoint(&self.endpoint),
                    &self.model,
                    request.thinking,
                    Some("supra-prefix"),
                    credential,
                )
                .await
            }
            ProviderKind::Google => self.send_legacy(request, credential).await,
        }
    }

    /// The hand-rolled transport, now Google-only.
    async fn send_legacy(
        &self,
        request: &Request,
        credential: &supra_secrets::SecretString,
    ) -> Result<Completion, LlmError> {
        let body = self.render_body(request).map_err(|error| LlmError::BadResponse {
            provider: self.policy.kind.name().to_owned(),
            detail: format!("request body is not canonical: {error}"),
        })?;

        let response = self
            .http
            .post(&self.endpoint)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .headers(auth_headers(self.policy.kind, credential))
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

        read_google_sse(response, request.thinking).await
    }
}

/// Adaptive thinking is accepted by current Claude 4.6+ and 5-series models.
/// Older model families still require the budget-based request shape.
fn anthropic_supports_adaptive_thinking(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    ["claude-opus-5", "claude-sonnet-5", "claude-fable-5", "claude-opus-4-6", "claude-sonnet-4-6"]
        .iter()
        .any(|family| model.contains(family))
}

/// The endpoint as an SDK base override: `None` when the configuration
/// pins nothing (the SDK default applies), otherwise the configured
/// base. An empty string pins nothing too — it is the absence of a
/// value, not a base URL.
fn sdk_endpoint(endpoint: &str) -> Option<&str> {
    (!endpoint.trim().is_empty()).then_some(endpoint)
}

/// The auth headers each provider expects: Anthropic wants `x-api-key` plus its version
/// header; the others take Bearer credentials.
fn auth_headers(kind: ProviderKind, credential: &supra_secrets::SecretString) -> reqwest::header::HeaderMap {
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

    let mut headers = HeaderMap::new();
    let value = credential.expose();
    match kind {
        ProviderKind::Anthropic => {
            headers.insert(
                HeaderName::from_static("x-api-key"),
                HeaderValue::from_str(value)
                    .unwrap_or_else(|_| HeaderValue::from_static("invalid-credential")),
            );
            headers
                .insert(HeaderName::from_static("anthropic-version"), HeaderValue::from_static("2023-06-01"));
        }
        ProviderKind::OpenAI | ProviderKind::Google => {
            if let Ok(parsed) = HeaderValue::from_str(&format!("Bearer {value}")) {
                headers.insert(reqwest::header::AUTHORIZATION, parsed);
            }
        }
    }
    headers
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
///
/// A stream that ends without a terminal marker is refused even when text arrived:
/// partial text returned as a success is a wrong answer wearing a green light.
#[allow(clippy::too_many_lines, reason = "the streaming parser keeps one explicit terminal-state machine")]
async fn read_google_sse(response: reqwest::Response, thinking: Thinking) -> Result<Completion, LlmError> {
    use futures::StreamExt as _;

    // The provider name for errors. Recovered from the URL is wrong (a custom endpoint
    // may proxy); carried by the caller is correct - but read_sse only has the response.
    // The URL's host is the honest fallback: it names where the bytes came from.
    let provider = response.url().host_str().unwrap_or("unknown").to_owned();
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut text = String::new();
    let mut usage: Option<Usage> = None;
    let mut stop_reason = None;
    let mut model = String::new();
    let mut request_id = None;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|error| LlmError::Transport { provider: provider.clone(), detail: error.to_string() })?;
        buffer.extend_from_slice(&chunk);
        // Drain complete events, keeping a partial tail buffered.
        while let Some(end) = find_event_end(&buffer) {
            let event: Vec<u8> = buffer.drain(..end).collect();
            let event = String::from_utf8_lossy(&event).into_owned();
            for line in event.lines() {
                let Some(payload) = line.strip_prefix("data:") else { continue };
                let payload = payload.trim();
                if payload == "[DONE]" {
                    return Ok(Completion {
                        content: vec![ContentBlock::text(text)],
                        stop_reason: stop_reason.unwrap_or(StopReason::EndTurn),
                        stop_sequence: None,
                        model,
                        request_id,
                        usage,
                        thinking,
                    });
                }
                let json: serde_json::Value =
                    serde_json::from_str(payload).map_err(|_| LlmError::BadResponse {
                        provider: provider.clone(),
                        detail: format!("event is not JSON: {payload:.80}"),
                    })?;
                if json.get("type").and_then(serde_json::Value::as_str) == Some("error") {
                    let detail = json
                        .get("error")
                        .and_then(|error| error.get("message"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("the provider sent an error event");
                    return Err(LlmError::BadResponse { provider, detail: detail.to_owned() });
                }
                if json.get("error").is_some() && json.get("choices").is_none() {
                    let detail = json
                        .get("error")
                        .and_then(|error| error.get("message"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("the provider sent an error object");
                    return Err(LlmError::BadResponse { provider, detail: detail.to_owned() });
                }
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
                if let Some(reported_model) = json.get("model").and_then(serde_json::Value::as_str) {
                    reported_model.clone_into(&mut model);
                }
                if let Some(reported_id) = json.get("id").and_then(serde_json::Value::as_str) {
                    request_id = Some(reported_id.to_owned());
                }
                if let Some(reason) =
                    json.get("stop_reason").and_then(serde_json::Value::as_str).or_else(|| {
                        json.get("choices")
                            .and_then(|choices| choices.get(0))
                            .and_then(|choice| choice.get("finish_reason"))
                            .and_then(serde_json::Value::as_str)
                    })
                {
                    stop_reason = Some(parse_stop_reason(reason));
                }
                // Usage arrives on the final event (Anthropic) or as usage (OpenAI).
                if let Some(reported) = parse_usage(&json) {
                    usage = Some(reported);
                }
            }
        }
    }
    let Some(stop_reason) = stop_reason else {
        let detail = if text.is_empty() {
            "the stream ended with no events".to_owned()
        } else {
            "the stream ended before a terminal marker".to_owned()
        };
        return Err(LlmError::BadResponse { provider, detail });
    };
    Ok(Completion {
        content: vec![ContentBlock::text(text)],
        stop_reason,
        stop_sequence: None,
        model,
        request_id,
        usage,
        thinking,
    })
}

fn parse_stop_reason(reason: &str) -> StopReason {
    match reason {
        "end_turn" | "stop" => StopReason::EndTurn,
        "tool_use" | "tool_calls" | "function_call" => StopReason::ToolUse,
        "max_tokens" | "length" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        "pause_turn" => StopReason::PauseTurn,
        "refusal" => StopReason::Refusal,
        "model_context_window_exceeded" => StopReason::ModelContextWindowExceeded,
        "content_filter" | "safety" => StopReason::ContentFilter,
        other => StopReason::Other(other.to_owned()),
    }
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
            messages: vec![Message::text(Role::User, "hi")],
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
            messages: vec![Message::text(Role::User, "cache me")],
            tools: vec![
                crate::canonicalize(r#"{"input_schema":{"type":"object"},"name":"lookup"}"#).expect("tool"),
            ],
            thinking: Thinking { budget_tokens: 0 },
            breakpoints: Breakpoint::ALL.to_vec(),
        };
        let body = anthropic.render_body(&request).expect("renders");
        assert!(body.as_str().contains("cache_control"), "{}", body.as_str());

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
        assert!(!body.as_str().contains("cache_control"), "{}", body.as_str());
    }

    #[test]
    fn each_provider_renders_its_own_wire_shape() {
        let messages = vec![Message::text(Role::User, "hi")];

        let anthropic = test_client(ProviderKind::Anthropic);
        let request = Request {
            provider: ProviderKind::Anthropic,
            model: "m".to_owned(),
            messages: messages.clone(),
            tools: Vec::new(),
            thinking: Thinking { budget_tokens: 0 },
            breakpoints: Vec::new(),
        };
        let body = anthropic.render_body(&request).expect("anthropic renders");
        assert!(body.as_str().contains("\"max_tokens\""), "{}", body.as_str());
        assert!(
            body.as_str().contains(r#""messages":[{"content":[{"text":"hi","type":"text"}],"role":"user"}]"#),
            "{}",
            body.as_str()
        );

        let openai = test_client(ProviderKind::OpenAI);
        let request = Request { provider: ProviderKind::OpenAI, model: "m".to_owned(), ..request.clone() };
        let body = openai.render_body(&request).expect("openai renders");
        assert!(
            body.as_str().contains(r#""messages":[{"content":[{"text":"hi","type":"text"}],"role":"user"}]"#),
            "{}",
            body.as_str()
        );
        assert!(!body.as_str().contains("max_tokens"), "{}", body.as_str());

        let google = test_client(ProviderKind::Google);
        let request = Request { provider: ProviderKind::Google, model: "m".to_owned(), ..request.clone() };
        let body = google.render_body(&request).expect("google renders");
        assert!(
            body.as_str().contains(r#""contents":[{"parts":[{"text":"hi"}],"role":"user"}]"#),
            "{}",
            body.as_str()
        );
        assert!(!body.as_str().contains("\"messages\""), "{}", body.as_str());
    }

    #[test]
    fn anthropic_replay_bytes_keep_order_and_signatures() {
        let client = test_client(ProviderKind::Anthropic);
        let request = Request {
            provider: ProviderKind::Anthropic,
            model: "m".to_owned(),
            messages: vec![Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking { thinking: "trace".to_owned(), signature: "sig".to_owned() },
                    ContentBlock::RedactedThinking { data: "opaque".to_owned() },
                    ContentBlock::ToolUse {
                        id: "tool-1".to_owned(),
                        name: "lookup".to_owned(),
                        input: serde_json::json!({"query": "x"}),
                    },
                    ContentBlock::ToolResult {
                        tool_use_id: "tool-1".to_owned(),
                        content: serde_json::Value::String("found".to_owned()),
                        is_error: true,
                    },
                    ContentBlock::text("done"),
                ],
            }],
            tools: Vec::new(),
            thinking: Thinking { budget_tokens: 0 },
            breakpoints: Vec::new(),
        };

        let body = client.render_body(&request).expect("replay renders");
        assert_eq!(
            body.as_str(),
            r#"{"max_tokens":32000,"messages":[{"content":[{"signature":"sig","thinking":"trace","type":"thinking"},{"data":"opaque","type":"redacted_thinking"},{"id":"tool-1","input":{"query":"x"},"name":"lookup","type":"tool_use"},{"content":"found","is_error":true,"tool_use_id":"tool-1","type":"tool_result"},{"text":"done","type":"text"}],"role":"assistant"}],"model":"test-model","stream":true}"#
        );
    }

    #[test]
    fn anthropic_auth_is_x_api_key_openai_is_bearer() {
        use supra_secrets::SecretString;

        let credential = SecretString::new("key-material".to_owned());
        let anthropic = auth_headers(ProviderKind::Anthropic, &credential);
        assert!(anthropic.contains_key("x-api-key"), "{anthropic:?}");
        assert_eq!(
            anthropic.get("anthropic-version").and_then(|value| value.to_str().ok()),
            Some("2023-06-01")
        );
        assert!(!anthropic.contains_key(reqwest::header::AUTHORIZATION));

        let openai = auth_headers(ProviderKind::OpenAI, &credential);
        assert_eq!(
            openai.get(reqwest::header::AUTHORIZATION).and_then(|value| value.to_str().ok()),
            Some("Bearer key-material")
        );
        assert!(!openai.contains_key("x-api-key"));
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
