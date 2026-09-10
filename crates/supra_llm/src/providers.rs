//! One request, one response, through the official provider SDKs.
//!
//! The `Anthropic` path speaks the Messages API; the `OpenAI`
//! path speaks the Chat Completions API. Both stream: the
//! SDK owns the wire framing and the event parsing, and this crate
//! accumulates text deltas plus usage into a [`Completion`], exactly as
//! it did when the framing was hand-rolled. The policy layer
//! ([`CachePolicy`](crate::CachePolicy)) is unchanged — the SDKs carry
//! the cache fields the policy names, they do not decide them.
//!
//! What the SDKs do *not* own: credential storage, retry policy, or the
//! conversion from [`LlmError`] recoverability classes.
//! An SDK error is mapped at the boundary into the variant the caller
//! already matches on — [`Unauthorized`](crate::LlmError::Unauthorized),
//! [`RateLimited`](crate::LlmError::RateLimited),
//! [`Transport`](crate::LlmError::Transport), or
//! [`BadResponse`](crate::LlmError::BadResponse) — so no caller changes.

use anthropic_sdk::types::{MessageContent, MessageParam, Role as AnthropicRole};
use futures::StreamExt as _;

use crate::client::{Completion, Request, Role, Thinking, Usage};
use crate::error::LlmError;
use crate::policy::ProviderKind;

/// The `OpenAI` model used when the configuration pins none, and the one the
/// live test exercises: the cheapest current chat model, not a moving alias.
pub const DEFAULT_OPENAI_MODEL: &str = "gpt-4o-mini";

/// The Anthropic model used when the configuration pins none, and the one
/// the live test exercises: the cheapest current Messages model.
pub const DEFAULT_ANTHROPIC_MODEL: &str = "claude-3-5-haiku-20241022";

/// Send one Anthropic request through the official SDK and read the
/// streamed completion.
///
/// `endpoint` overrides the API base (a custom gateway or a test
/// double); `None` means the SDK default. The credential arrives per
/// request and is never stored — the SDK holds it for the call only.
///
/// # Errors
///
/// [`LlmError::Unauthorized`], [`LlmError::RateLimited`],
/// [`LlmError::Transport`], or [`LlmError::BadResponse`], by
/// recoverability rather than by SDK spelling.
pub async fn send_anthropic(
    request: &Request,
    endpoint: Option<&str>,
    model: &str,
    thinking: Thinking,
    credential: &supra_secrets::SecretString,
) -> Result<Completion, LlmError> {
    use anthropic_sdk::config::ClientConfig;

    let mut config = ClientConfig::new(credential.expose());
    if let Some(base) = endpoint {
        config.base_url = base.to_owned();
    }
    let client = anthropic_sdk::client::Anthropic::with_config(config).map_err(|error| {
        LlmError::Transport { provider: ProviderKind::Anthropic.name().to_owned(), detail: error.to_string() }
    })?;

    let messages: Vec<MessageParam> = request
        .messages
        .iter()
        .map(|message| MessageParam {
            role: match message.role {
                Role::User => AnthropicRole::User,
                Role::Assistant => AnthropicRole::Assistant,
            },
            content: MessageContent::Text(message.content.clone()),
        })
        .collect();

    let params = anthropic_sdk::types::MessageCreateParams {
        model: model.to_owned(),
        max_tokens: 32_000,
        messages,
        system: None,
        temperature: None,
        top_p: None,
        top_k: None,
        stop_sequences: None,
        stream: None,
        tools: tools_value(request)?,
        tool_choice: None,
        metadata: None,
    };

    let stream = client.messages().stream(params).await.map_err(map_anthropic)?;
    let message = stream.final_message().await.map_err(map_anthropic)?;

    let mut text = String::new();
    for block in &message.content {
        if let anthropic_sdk::types::ContentBlock::Text { text: part } = block {
            text.push_str(part);
        }
    }
    let usage = Some(Usage {
        input_tokens: u64::from(message.usage.input_tokens),
        output_tokens: u64::from(message.usage.output_tokens),
        cached_tokens: u64::from(message.usage.cache_read_input_tokens.unwrap_or(0)),
    });
    Ok(Completion { text, usage, thinking })
}

/// Convert canonical tool definitions into the SDK's tool shape.
///
/// A definition that does not parse is skipped, not refused: the
/// canonical layer guarantees shape, so a failure here is a layering
/// defect worth degrading rather than a request worth dropping.
fn tools_value(request: &Request) -> Result<Option<Vec<anthropic_sdk::types::Tool>>, LlmError> {
    if request.tools.is_empty() {
        return Ok(None);
    }
    let mut tools = Vec::new();
    for tool in &request.tools {
        let parsed: serde_json::Value =
            serde_json::from_str(tool.as_str()).map_err(|error| LlmError::BadResponse {
                provider: ProviderKind::Anthropic.name().to_owned(),
                detail: format!("tool definition is not JSON: {error}"),
            })?;
        let name = parsed.get("name").and_then(serde_json::Value::as_str).unwrap_or("tool").to_owned();
        let description =
            parsed.get("description").and_then(serde_json::Value::as_str).unwrap_or("").to_owned();
        let properties = parsed
            .get("input_schema")
            .and_then(|schema| schema.get("properties"))
            .and_then(serde_json::Value::as_object)
            .cloned()
            .unwrap_or_default();
        tools.push(anthropic_sdk::types::Tool {
            name,
            description,
            input_schema: anthropic_sdk::types::ToolInputSchema {
                schema_type: "object".to_owned(),
                properties,
                required: Vec::new(),
                additional: serde_json::Map::new(),
            },
        });
    }
    Ok(Some(tools))
}

/// Map an Anthropic SDK failure into the caller's recoverability class.
///
/// Authentication and permission failures are never retried; rate
/// limits carry the documented 60 s default (the SDK does not surface a
/// `retry-after` value); transport failures stay retryable; everything
/// else is a response the client cannot use.
fn map_anthropic(error: anthropic_sdk::types::AnthropicError) -> LlmError {
    use anthropic_sdk::types::AnthropicError as Sdk;

    let provider = ProviderKind::Anthropic.name().to_owned();
    match error {
        Sdk::Authentication { message, .. } | Sdk::PermissionDenied { message, .. } => {
            LlmError::Unauthorized { provider, detail: message }
        }
        Sdk::InvalidApiKey => LlmError::Unauthorized { provider, detail: "invalid credential".to_owned() },
        Sdk::RateLimit { .. } => LlmError::RateLimited { provider, retry_after_ms: 60_000 },
        Sdk::Connection { message } | Sdk::NetworkError(message) => {
            LlmError::Transport { provider, detail: message }
        }
        Sdk::ConnectionTimeout | Sdk::Timeout => {
            LlmError::Transport { provider, detail: "the request timed out".to_owned() }
        }
        Sdk::StreamError(detail) => LlmError::BadResponse { provider, detail },
        other => LlmError::BadResponse { provider, detail: other.to_string() },
    }
}

/// Send one `OpenAI` request through the official SDK.
///
/// Two transports, one contract. The blocking call (`Chat::create`)
/// is tried first: one request, one JSON answer, no framing to drift.
/// When the transport refuses a blocking call — a gateway that only
/// serves SSE on the completions path, a proxy that closes
/// non-streaming reads — the same request is re-issued as a stream
/// and accumulated exactly as a blocking answer would read: text
/// joined in order, usage from the final chunk. The caller cannot
/// tell which transport answered.
///
/// # Errors
///
/// As [`send_anthropic`], keyed to the `OpenAI` provider name.
pub async fn send_openai(
    request: &Request,
    endpoint: Option<&str>,
    model: &str,
    thinking: Thinking,
    prompt_cache_key: Option<&str>,
    credential: &supra_secrets::SecretString,
) -> Result<Completion, LlmError> {
    let client = openai_client(endpoint, credential);
    let sdk_request = openai_request(request, model, prompt_cache_key)?;
    match client.chat().create(sdk_request.clone()).await {
        Ok(response) => Ok(blocking_completion(response, thinking)),
        Err(error) if blocks_streaming(&error) => stream_openai(&client, sdk_request, thinking).await,
        Err(error) => Err(map_openai(&error)),
    }
}

/// Build the SDK client for one request: credential per call, base
/// override normalised to exactly one trailing `/v1`.
fn openai_client(
    endpoint: Option<&str>,
    credential: &supra_secrets::SecretString,
) -> async_openai::Client<async_openai::config::OpenAIConfig> {
    use async_openai::config::OpenAIConfig;

    let mut config = OpenAIConfig::new().with_api_key(credential.expose());
    if let Some(base) = endpoint {
        // The SDK concatenates base + "/chat/completions" verbatim, so a
        // base that already ends in `/v1` would produce `/v1/chat/...`
        // while one that does not would produce `/chat/...`. Normalise
        // to exactly one trailing `/v1`.
        let trimmed = base.trim_end_matches('/');
        let base = match trimmed.strip_suffix("/v1") {
            Some(_) => trimmed.to_owned(),
            None => format!("{trimmed}/v1"),
        };
        config = config.with_api_base(base);
    }
    async_openai::Client::with_config(config)
}

/// Build the SDK request for one [`Request`]: messages in role order,
/// the pinned model, the one-key cache policy, the effort band for
/// thinking, and the canonical tools as function tools.
///
/// # Errors
///
/// [`LlmError::BadResponse`] when a tool definition is not JSON.
fn openai_request(
    request: &Request,
    model: &str,
    prompt_cache_key: Option<&str>,
) -> Result<async_openai::types::chat::CreateChatCompletionRequest, LlmError> {
    use async_openai::types::chat::{
        ChatCompletionRequestMessage, ChatCompletionRequestUserMessage,
        ChatCompletionRequestUserMessageContent, CreateChatCompletionRequest,
    };

    let messages: Vec<ChatCompletionRequestMessage> = request
        .messages
        .iter()
        .map(|message| match message.role {
            Role::User => ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
                content: ChatCompletionRequestUserMessageContent::Text(message.content.clone()),
                name: None,
            }),
            Role::Assistant => ChatCompletionRequestMessage::Assistant(
                async_openai::types::chat::ChatCompletionRequestAssistantMessage {
                    content: Some(
                        async_openai::types::chat::ChatCompletionRequestAssistantMessageContent::Text(
                            message.content.clone(),
                        ),
                    ),
                    refusal: None,
                    name: None,
                    audio: None,
                    tool_calls: None,
                    #[allow(deprecated)]
                    function_call: None,
                },
            ),
        })
        .collect();

    let sdk_request = CreateChatCompletionRequest {
        messages,
        model: model.to_owned(),
        prompt_cache_key: prompt_cache_key.map(str::to_owned),
        reasoning_effort: reasoning_effort(request),
        tools: openai_tools(request)?,
        // Ask for usage on the final chunk: without it a gateway that
        // reports tokens only there leaves the completion unpriced.
        // Harmless on the blocking path, which ignores it.
        stream_options: Some(async_openai::types::chat::ChatCompletionStreamOptions {
            include_usage: Some(true),
            include_obfuscation: None,
        }),
        ..Default::default()
    };
    Ok(sdk_request)
}

/// Read one blocking `OpenAI` answer into a [`Completion`].
///
/// The first choice carries the answer; usage rides the top level.
/// An empty choice list is a version-skew refusal, not an empty
/// answer — returning `Ok` with no text would certify that the model
/// said nothing.
fn blocking_completion(
    response: async_openai::types::chat::CreateChatCompletionResponse,
    thinking: Thinking,
) -> Completion {
    let mut text = String::new();
    for choice in &response.choices {
        if let Some(part) = &choice.message.content {
            text.push_str(part);
        }
    }
    let usage = response.usage.map(|reported| Usage {
        input_tokens: u64::from(reported.prompt_tokens),
        output_tokens: u64::from(reported.completion_tokens),
        cached_tokens: 0,
    });
    Completion { text, usage, thinking }
}

/// Whether a blocking-call failure is worth retrying as a stream.
///
/// Transport failures only — refused credentials, rate limits, and
/// malformed requests would fail identically the second time, and
/// re-issuing them doubles the load that caused the refusal. A hung
/// or reset connection, a truncated body, or an SSE-only gateway is
/// exactly what the streaming transport is for.
fn blocks_streaming(error: &async_openai::error::OpenAIError) -> bool {
    use async_openai::error::OpenAIError as Sdk;

    !matches!(error, Sdk::ApiError(_) | Sdk::InvalidArgument(_))
}

/// Re-issue one request as a stream and accumulate it into a
/// [`Completion`]: text joined in chunk order, usage from the final
/// chunk, terminal marker required.
///
/// A stream that ends without one is refused even when text arrived:
/// partial text returned as a success is a wrong answer wearing a
/// green light. The hand-rolled reader held this invariant; both SDK
/// paths keep it.
async fn stream_openai(
    client: &async_openai::Client<async_openai::config::OpenAIConfig>,
    sdk_request: async_openai::types::chat::CreateChatCompletionRequest,
    thinking: Thinking,
) -> Result<Completion, LlmError> {
    let mut stream = client.chat().create_stream(sdk_request).await.map_err(|error| map_openai(&error))?;

    let mut text = String::new();
    let mut usage: Option<Usage> = None;
    let mut terminal = false;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| map_openai(&error))?;
        for choice in &chunk.choices {
            if let Some(part) = &choice.delta.content {
                text.push_str(part);
            }
            if choice.finish_reason.is_some() {
                terminal = true;
            }
        }
        if let Some(reported) = &chunk.usage {
            usage = Some(Usage {
                input_tokens: u64::from(reported.prompt_tokens),
                output_tokens: u64::from(reported.completion_tokens),
                cached_tokens: 0,
            });
        }
    }

    if !terminal {
        let detail = if text.is_empty() {
            "the stream ended with no events".to_owned()
        } else {
            "the stream ended before a terminal marker".to_owned()
        };
        return Err(LlmError::BadResponse { provider: ProviderKind::OpenAI.name().to_owned(), detail });
    }
    Ok(Completion { text, usage, thinking })
}

/// The reasoning effort for a request: `medium` when thinking is on,
/// absent when it is off. The budget stays a rendering concern — the
/// API takes an effort band, not a token count.
fn reasoning_effort(request: &Request) -> Option<async_openai::types::chat::ReasoningEffort> {
    use async_openai::types::chat::ReasoningEffort;

    if request.thinking.enabled() { Some(ReasoningEffort::Medium) } else { None }
}

/// Convert canonical tool definitions into the SDK's function tools.
///
/// Unparseable definitions are skipped, as on the Anthropic path.
fn openai_tools(
    request: &Request,
) -> Result<Option<Vec<async_openai::types::chat::ChatCompletionTools>>, LlmError> {
    use async_openai::types::chat::{ChatCompletionTool, ChatCompletionTools, FunctionObject};

    if request.tools.is_empty() {
        return Ok(None);
    }
    let mut tools = Vec::new();
    for tool in &request.tools {
        let parsed: serde_json::Value =
            serde_json::from_str(tool.as_str()).map_err(|error| LlmError::BadResponse {
                provider: ProviderKind::OpenAI.name().to_owned(),
                detail: format!("tool definition is not JSON: {error}"),
            })?;
        let name = parsed.get("name").and_then(serde_json::Value::as_str).unwrap_or("tool").to_owned();
        let description = parsed.get("description").and_then(serde_json::Value::as_str).map(str::to_owned);
        tools.push(ChatCompletionTools::Function(ChatCompletionTool {
            function: FunctionObject {
                name,
                description,
                parameters: parsed.get("input_schema").or_else(|| parsed.get("parameters")).cloned(),
                strict: None,
            },
        }));
    }
    Ok(Some(tools))
}

/// Map an `OpenAI` SDK failure into the caller's recoverability class.
///
/// The SDK surfaces HTTP failures as `ApiError` with the status in the
/// message; 401/403 never retry, 429 carries the documented 60 s
/// default. Stream mid-failures are unusable responses, not transport
/// retries — retrying skew is how one bad deploy becomes eighty bad
/// requests.
fn map_openai(error: &async_openai::error::OpenAIError) -> LlmError {
    use async_openai::error::OpenAIError as Sdk;

    let provider = ProviderKind::OpenAI.name().to_owned();
    match error {
        Sdk::ApiError(response) => {
            let status = response.status_code.as_u16();
            let detail = response.api_error.message.clone();
            if status == 401 || status == 403 {
                LlmError::Unauthorized { provider, detail }
            } else if status == 429 {
                LlmError::RateLimited { provider, retry_after_ms: 60_000 }
            } else {
                LlmError::Transport { provider, detail: format!("HTTP {status}: {detail}") }
            }
        }
        Sdk::Reqwest(error) => LlmError::Transport { provider, detail: error.to_string() },
        Sdk::StreamError(error) => {
            LlmError::BadResponse { provider, detail: format!("stream failed: {error}") }
        }
        Sdk::JSONDeserialize(error, content) => {
            LlmError::BadResponse { provider, detail: format!("{error}: {content:.120}") }
        }
        Sdk::InvalidArgument(detail) => LlmError::BadResponse { provider, detail: detail.clone() },
        _ => LlmError::Transport { provider, detail: error.to_string() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::ProviderKind;

    fn sample_request(provider: ProviderKind) -> Request {
        Request {
            provider,
            model: "m".to_owned(),
            messages: vec![crate::client::Message { role: Role::User, content: "hi".to_owned() }],
            tools: Vec::new(),
            thinking: Thinking { budget_tokens: 0 },
            breakpoints: Vec::new(),
        }
    }

    #[test]
    fn sdk_request_shapes_carry_the_policy_fields() {
        let open = sample_request(ProviderKind::OpenAI);
        assert_eq!(reasoning_effort(&open), None);
        let thinking = Request { thinking: Thinking { budget_tokens: 2048 }, ..open };
        assert!(reasoning_effort(&thinking).is_some());

        let anthropic = sample_request(ProviderKind::Anthropic);
        assert!(tools_value(&anthropic).expect("no tools").is_none());
        assert!(openai_tools(&anthropic).expect("no tools").is_none());
    }

    #[test]
    fn blocking_answers_read_text_and_usage() {
        use async_openai::types::chat::{
            ChatChoice, ChatCompletionResponseMessage, CompletionUsage, CreateChatCompletionResponse,
        };

        let response = CreateChatCompletionResponse {
            id: "chatcmpl-test".to_owned(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatCompletionResponseMessage {
                    content: Some("live-ok".to_owned()),
                    refusal: None,
                    tool_calls: None,
                    annotations: None,
                    role: async_openai::types::chat::Role::Assistant,
                    audio: None,
                    #[allow(deprecated)]
                    function_call: None,
                },
                finish_reason: Some(async_openai::types::chat::FinishReason::Stop),
                logprobs: None,
            }],
            created: 0,
            model: "m".to_owned(),
            service_tier: None,
            #[allow(deprecated)]
            system_fingerprint: None,
            object: "chat.completion".to_owned(),
            usage: Some(CompletionUsage {
                prompt_tokens: 213,
                completion_tokens: 7,
                total_tokens: 220,
                prompt_tokens_details: None,
                completion_tokens_details: None,
            }),
            metadata: None,
            moderation: None,
        };
        let completion = blocking_completion(response, Thinking { budget_tokens: 0 });
        assert_eq!(completion.text, "live-ok");
        let usage = completion.usage.expect("usage rides the top level");
        assert_eq!((usage.input_tokens, usage.output_tokens), (213, 7));
    }

    #[test]
    fn only_transport_failures_retry_blocking_as_stream() {
        use async_openai::error::OpenAIError as Sdk;

        assert!(!blocks_streaming(&Sdk::InvalidArgument("bad request".to_owned())));
        assert!(blocks_streaming(&Sdk::StreamError(Box::new(
            async_openai::error::StreamError::EventStream("reset".to_owned())
        ))));
    }

    #[test]
    fn anthropic_auth_failures_never_retry() {
        use anthropic_sdk::types::AnthropicError as Sdk;

        let error = map_anthropic(Sdk::InvalidApiKey);
        assert!(matches!(error, LlmError::Unauthorized { .. }), "{error}");
        let error = map_anthropic(Sdk::Authentication { message: "bad".to_owned(), status: 401 });
        assert!(matches!(error, LlmError::Unauthorized { .. }), "{error}");
        let error = map_anthropic(Sdk::PermissionDenied { message: "no".to_owned(), status: 403 });
        assert!(matches!(error, LlmError::Unauthorized { .. }), "{error}");
    }

    #[test]
    fn anthropic_rate_limits_carry_the_default_delay() {
        use anthropic_sdk::types::AnthropicError as Sdk;

        let error = map_anthropic(Sdk::RateLimit { message: "slow".to_owned(), status: 429 });
        assert!(matches!(error, LlmError::RateLimited { retry_after_ms: 60_000, .. }), "{error}");
    }

    #[test]
    fn anthropic_transport_failures_stay_retryable() {
        use anthropic_sdk::types::AnthropicError as Sdk;

        let error = map_anthropic(Sdk::Connection { message: "down".to_owned() });
        assert!(matches!(error, LlmError::Transport { .. }), "{error}");
        let error = map_anthropic(Sdk::Timeout);
        assert!(matches!(error, LlmError::Transport { .. }), "{error}");
    }

    #[test]
    fn openai_api_failures_map_by_status() {
        use async_openai::error::OpenAIError as Sdk;

        fn response(status: u16, message: &str) -> async_openai::error::ApiErrorResponse {
            async_openai::error::ApiErrorResponse {
                status_code: reqwest::StatusCode::from_u16(status).expect("valid test status"),
                api_error: async_openai::error::ApiError {
                    message: message.to_owned(),
                    r#type: None,
                    param: None,
                    code: None,
                    misalignment: None,
                },
            }
        }

        let error = map_openai(&Sdk::ApiError(response(401, "bad key")));
        assert!(matches!(error, LlmError::Unauthorized { .. }), "{error}");

        let error = map_openai(&Sdk::ApiError(response(429, "slow")));
        assert!(matches!(error, LlmError::RateLimited { retry_after_ms: 60_000, .. }), "{error}");

        let error = map_openai(&Sdk::ApiError(response(500, "boom")));
        assert!(matches!(error, LlmError::Transport { .. }), "{error}");
    }

    /// Live probe against an `Anthropic`-compatible gateway through the
    /// real SDK path: stream a short answer, require terminal text and
    /// sane usage.
    ///
    /// Ignored by default: needs `SUPRA_LIVE_ANTHROPIC_BASE`,
    /// `SUPRA_LIVE_ANTHROPIC_KEY`, and `SUPRA_LIVE_ANTHROPIC_MODEL`. Run
    /// with `cargo test -p supra_llm -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "needs a live gateway; run explicitly with SUPRA_LIVE_ANTHROPIC_* set"]
    #[allow(clippy::print_stdout, reason = "the live probe reports what the gateway returned")]
    async fn live_anthropic_compatible_gateway_streams_to_terminal() {
        let (base, key, model) = live_env("ANTHROPIC");
        let credential = supra_secrets::SecretString::new(key);
        let request = Request {
            provider: ProviderKind::Anthropic,
            model: model.clone(),
            messages: vec![crate::client::Message {
                role: Role::User,
                content: "Reply with exactly: live-ok".to_owned(),
            }],
            tools: Vec::new(),
            thinking: Thinking { budget_tokens: 0 },
            breakpoints: Vec::new(),
        };
        let completion =
            send_anthropic(&request, Some(&base), &model, Thinking { budget_tokens: 0 }, &credential)
                .await
                .expect("the gateway answers through the SDK");

        assert_eq!(completion.text.trim(), "live-ok", "the stream carried the exact answer");
        let usage = completion.usage.expect("the gateway reports usage");
        assert!(usage.input_tokens > 0, "input usage is reported: {usage:?}");
        assert!(usage.output_tokens > 0, "output usage is reported: {usage:?}");
        println!("live anthropic usage: {usage:?}");
    }

    /// Live probe against an `OpenAI`-compatible gateway through the real
    /// SDK path: stream a short answer, accumulate text, require a
    /// terminal marker and sane usage.
    ///
    /// The `SUPRA_LIVE_OPENAI_BASE` may name the API root with or
    /// without the trailing `/v1` — the SDK path normalises it.
    ///
    /// Ignored by default: needs `SUPRA_LIVE_OPENAI_BASE`,
    /// `SUPRA_LIVE_OPENAI_KEY`, and `SUPRA_LIVE_OPENAI_MODEL`. Run with
    /// `cargo test -p supra_llm -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "needs a live gateway; run explicitly with SUPRA_LIVE_OPENAI_* set"]
    #[allow(clippy::print_stdout, reason = "the live probe reports what the gateway returned")]
    async fn live_openai_compatible_gateway_streams_to_terminal() {
        let (base, key, model) = live_env("OPENAI");
        let credential = supra_secrets::SecretString::new(key);
        let request = Request {
            provider: ProviderKind::OpenAI,
            model: model.clone(),
            messages: vec![crate::client::Message {
                role: Role::User,
                content: "Reply with exactly: live-ok".to_owned(),
            }],
            tools: Vec::new(),
            thinking: Thinking { budget_tokens: 0 },
            breakpoints: Vec::new(),
        };
        let completion =
            send_openai(&request, Some(&base), &model, Thinking { budget_tokens: 0 }, None, &credential)
                .await
                .expect("the gateway answers through the SDK");

        assert!(!completion.text.trim().is_empty(), "the stream carried text");
        if let Some(usage) = completion.usage {
            assert!(usage.input_tokens > 0, "input usage is reported: {usage:?}");
            assert!(usage.output_tokens > 0, "output usage is reported: {usage:?}");
        }
        println!("live openai text: {:?}", completion.text.trim());
        println!("live openai usage: {:?}", completion.usage);
    }

    /// The live probe's environment for one provider family: base URL,
    /// key, model. Panics with the variable names, never the values.
    #[cfg(test)]
    fn live_env(family: &str) -> (String, String, String) {
        let names = [
            format!("SUPRA_LIVE_{family}_BASE"),
            format!("SUPRA_LIVE_{family}_KEY"),
            format!("SUPRA_LIVE_{family}_MODEL"),
        ];
        for name in &names {
            assert!(
                std::env::var(name).is_ok_and(|value| !value.trim().is_empty()),
                "{name} must be set to run the live probe"
            );
        }
        (
            std::env::var(&names[0]).expect("checked"),
            std::env::var(&names[1]).expect("checked"),
            std::env::var(&names[2]).expect("checked"),
        )
    }
}
