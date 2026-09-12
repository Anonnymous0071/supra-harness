//! One request, one response, through provider-native transports.
//!
//! The `Anthropic` path sends canonical Messages API bytes over rustls;
//! the `OpenAI` path uses its official SDK's Chat Completions API. Both
//! stream into the same lossless [`Completion`] contract. The policy layer
//! ([`CachePolicy`](crate::CachePolicy)) owns cache shape and thinking policy;
//! transports only send those decisions and classify failures.
//!
//! Credentials arrive per request and retry policy stays with the caller.
//! Boundary failures are classified as [`Unauthorized`](crate::LlmError::Unauthorized),
//! [`RateLimited`](crate::LlmError::RateLimited),
//! [`Transport`](crate::LlmError::Transport), or
//! [`BadResponse`](crate::LlmError::BadResponse).

use futures::StreamExt as _;

use crate::client::{Completion, ContentBlock, Request, Role, StopReason, Thinking, Usage};
use crate::error::LlmError;
use crate::policy::ProviderKind;

/// The `OpenAI` model used when the configuration pins none, and the one the
/// live test exercises: the cheapest current chat model, not a moving alias.
pub const DEFAULT_OPENAI_MODEL: &str = "gpt-4o-mini";

/// The Anthropic model used when the configuration pins none, and the one
/// the live test exercises: the cheapest current Messages model.
pub const DEFAULT_ANTHROPIC_MODEL: &str = "claude-haiku-4-5-20251001";

/// Send one Anthropic request as canonical Messages API bytes and read the
/// streamed completion.
///
/// `endpoint` overrides the API base (a custom gateway or test double);
/// `None` selects `https://api.anthropic.com`. The credential arrives per
/// request and is never stored.
///
/// # Errors
///
/// [`LlmError::Unauthorized`], [`LlmError::RateLimited`],
/// [`LlmError::Transport`], or [`LlmError::BadResponse`], by
/// recoverability rather than by wire spelling.
#[allow(clippy::too_many_lines, reason = "the lossless response conversion forms one boundary")]
pub async fn send_anthropic(
    http: reqwest::Client,
    request: &Request,
    endpoint: Option<&str>,
    model: &str,
    thinking: Thinking,
    credential: &supra_secrets::SecretString,
) -> Result<Completion, LlmError> {
    let endpoint = anthropic_messages_endpoint(endpoint);
    let body = anthropic_body(request, model)?;
    let response = http
        .post(endpoint)
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .header("x-api-key", credential.expose())
        .header("anthropic-version", "2023-06-01")
        .body(body.as_str().to_owned())
        .send()
        .await
        .map_err(|error| LlmError::Transport {
            provider: ProviderKind::Anthropic.name().to_owned(),
            detail: error.to_string(),
        })?;

    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(LlmError::Unauthorized {
            provider: ProviderKind::Anthropic.name().to_owned(),
            detail: format!("HTTP {status}"),
        });
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(LlmError::RateLimited {
            provider: ProviderKind::Anthropic.name().to_owned(),
            retry_after_ms: provider_retry_after_ms(response.headers()),
        });
    }
    if !status.is_success() {
        let detail = response.text().await.unwrap_or_default();
        return Err(LlmError::BadResponse {
            provider: ProviderKind::Anthropic.name().to_owned(),
            detail: format!("HTTP {status}: {detail}"),
        });
    }
    read_anthropic_sse(response, thinking).await
}

fn anthropic_messages_endpoint(base: Option<&str>) -> String {
    let base = base.unwrap_or("https://api.anthropic.com").trim_end_matches('/');
    if base.ends_with("/v1/messages") { base.to_owned() } else { format!("{base}/v1/messages") }
}

fn anthropic_body(request: &Request, model: &str) -> Result<supra_types::CanonicalJson, LlmError> {
    let renderer = crate::client::Client::new(
        ProviderKind::Anthropic.name(),
        String::new(),
        model.to_owned(),
        request.thinking.budget_tokens,
    )?;
    renderer.render_body(request).map_err(|error| LlmError::BadResponse {
        provider: ProviderKind::Anthropic.name().to_owned(),
        detail: format!("request body is not canonical: {error}"),
    })
}

const MAX_ANTHROPIC_STREAM_BYTES: usize = 16 * 1024 * 1024;
const MAX_ANTHROPIC_EVENT_BYTES: usize = 1024 * 1024;
const MAX_ANTHROPIC_BLOCKS: usize = 4096;

fn provider_retry_after_ms(headers: &reqwest::header::HeaderMap) -> u64 {
    if let Some(seconds) = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
    {
        return seconds.saturating_mul(1_000);
    }
    headers
        .get("retry-after-ms")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(60_000)
}

#[derive(Debug)]
enum AnthropicBlock {
    Text { text: String, citations: Vec<serde_json::Value> },
    Thinking { thinking: String, signature: String },
    RedactedThinking { data: String },
    ToolUse { id: String, name: String, input: serde_json::Value, partial_json: String },
    Unknown { raw: serde_json::Value, partial_json: String },
}

impl AnthropicBlock {
    fn start(raw: serde_json::Value) -> Self {
        let kind = raw.get("type").and_then(serde_json::Value::as_str).unwrap_or_default();
        let string =
            |name: &str| raw.get(name).and_then(serde_json::Value::as_str).unwrap_or_default().to_owned();
        match kind {
            "text" => Self::Text {
                text: string("text"),
                citations: raw
                    .get("citations")
                    .and_then(serde_json::Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            },
            "thinking" => Self::Thinking { thinking: string("thinking"), signature: string("signature") },
            "redacted_thinking" => Self::RedactedThinking { data: string("data") },
            "tool_use" => Self::ToolUse {
                id: string("id"),
                name: string("name"),
                input: raw.get("input").cloned().unwrap_or(serde_json::Value::Null),
                partial_json: String::new(),
            },
            _ => Self::Unknown { raw, partial_json: String::new() },
        }
    }

    fn apply_delta(&mut self, delta: &serde_json::Value) -> Result<(), String> {
        let kind = delta.get("type").and_then(serde_json::Value::as_str).unwrap_or_default();
        match (self, kind) {
            (Self::Text { text, .. }, "text_delta") => {
                text.push_str(delta.get("text").and_then(serde_json::Value::as_str).unwrap_or_default());
            }
            (Self::Text { citations, .. }, "citations_delta" | "citation_delta") => {
                if let Some(citation) = delta.get("citation") {
                    citations.push(citation.clone());
                }
            }
            (Self::Thinking { thinking, .. }, "thinking_delta") => {
                thinking
                    .push_str(delta.get("thinking").and_then(serde_json::Value::as_str).unwrap_or_default());
            }
            (Self::Thinking { signature, .. }, "signature_delta") => {
                signature
                    .push_str(delta.get("signature").and_then(serde_json::Value::as_str).unwrap_or_default());
            }
            (Self::ToolUse { partial_json, .. } | Self::Unknown { partial_json, .. }, "input_json_delta") => {
                partial_json.push_str(
                    delta.get("partial_json").and_then(serde_json::Value::as_str).unwrap_or_default(),
                );
            }
            (block, _) => {
                return Err(format!("{kind} delta does not match {} block", block.kind()));
            }
        }
        Ok(())
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Text { .. } => "text",
            Self::Thinking { .. } => "thinking",
            Self::RedactedThinking { .. } => "redacted_thinking",
            Self::ToolUse { .. } => "tool_use",
            Self::Unknown { .. } => "unknown",
        }
    }

    fn finish(self) -> Result<ContentBlock, String> {
        Ok(match self {
            Self::Text { text, citations } => ContentBlock::Text { text, citations },
            Self::Thinking { thinking, signature } => ContentBlock::Thinking { thinking, signature },
            Self::RedactedThinking { data } => ContentBlock::RedactedThinking { data },
            Self::ToolUse { id, name, input, partial_json } => {
                let input = if partial_json.is_empty() {
                    input
                } else {
                    serde_json::from_str(&partial_json)
                        .map_err(|error| format!("tool input JSON is invalid: {error}"))?
                };
                ContentBlock::ToolUse { id, name, input }
            }
            Self::Unknown { mut raw, partial_json } => {
                if !partial_json.is_empty() {
                    raw["input"] = serde_json::from_str(&partial_json)
                        .map_err(|error| format!("unknown block input JSON is invalid: {error}"))?;
                }
                ContentBlock::Unknown { raw }
            }
        })
    }
}

#[derive(Default)]
struct AnthropicStream {
    open: std::collections::BTreeMap<u32, AnthropicBlock>,
    complete: std::collections::BTreeMap<u32, ContentBlock>,
    stop_reason: Option<StopReason>,
    stop_sequence: Option<String>,
    model: String,
    request_id: Option<String>,
    usage: Option<Usage>,
    saw_message_stop: bool,
}

impl AnthropicStream {
    fn event(&mut self, event: &serde_json::Value) -> Result<(), String> {
        match event.get("type").and_then(serde_json::Value::as_str).unwrap_or_default() {
            "message_start" => {
                let message = event.get("message").ok_or("message_start has no message")?;
                message
                    .get("model")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .clone_into(&mut self.model);
                self.request_id = message.get("id").and_then(serde_json::Value::as_str).map(str::to_owned);
                self.merge_usage(message);
                if let Some(blocks) = message.get("content").and_then(serde_json::Value::as_array) {
                    for (index, block) in blocks.iter().cloned().enumerate() {
                        let index = u32::try_from(index).map_err(|error| error.to_string())?;
                        self.insert_complete(index, AnthropicBlock::start(block).finish()?)?;
                    }
                }
            }
            "content_block_start" => {
                let index = event_index(event)?;
                if self.open.contains_key(&index) || self.complete.contains_key(&index) {
                    return Err(format!("duplicate content block index {index}"));
                }
                self.ensure_block_capacity()?;
                let raw = event.get("content_block").cloned().ok_or("content_block_start has no block")?;
                self.open.insert(index, AnthropicBlock::start(raw));
            }
            "content_block_delta" => {
                let index = event_index(event)?;
                let delta = event.get("delta").ok_or("content_block_delta has no delta")?;
                self.open
                    .get_mut(&index)
                    .ok_or_else(|| format!("delta for unopened block {index}"))?
                    .apply_delta(delta)?;
            }
            "content_block_stop" => {
                let index = event_index(event)?;
                let block =
                    self.open.remove(&index).ok_or_else(|| format!("stop for unopened block {index}"))?;
                self.insert_complete(index, block.finish()?)?;
            }
            "message_delta" => {
                let delta = event.get("delta").ok_or("message_delta has no delta")?;
                if let Some(reason) = delta.get("stop_reason").and_then(serde_json::Value::as_str) {
                    self.stop_reason = Some(anthropic_stop_reason(reason));
                }
                self.stop_sequence =
                    delta.get("stop_sequence").and_then(serde_json::Value::as_str).map(str::to_owned);
                self.merge_usage(event);
            }
            "message_stop" => self.saw_message_stop = true,
            "ping" => {}
            "error" => {
                return Err(event
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("the provider sent an error event")
                    .to_owned());
            }
            kind => return Err(format!("unknown Anthropic stream event {kind:?}")),
        }
        Ok(())
    }

    fn ensure_block_capacity(&self) -> Result<(), String> {
        if self.open.len().saturating_add(self.complete.len()) >= MAX_ANTHROPIC_BLOCKS {
            Err(format!("stream exceeds {MAX_ANTHROPIC_BLOCKS} content blocks"))
        } else {
            Ok(())
        }
    }

    fn insert_complete(&mut self, index: u32, block: ContentBlock) -> Result<(), String> {
        self.ensure_block_capacity()?;
        if self.complete.insert(index, block).is_some() {
            return Err(format!("duplicate content block index {index}"));
        }
        Ok(())
    }

    fn merge_usage(&mut self, value: &serde_json::Value) {
        let Some(reported) = value.get("usage") else { return };
        let usage = self.usage.get_or_insert_with(Usage::default);
        if let Some(input) = reported.get("input_tokens").and_then(serde_json::Value::as_u64) {
            usage.input_tokens = input;
        }
        if let Some(output) = reported.get("output_tokens").and_then(serde_json::Value::as_u64) {
            usage.output_tokens = output;
        }
        if let Some(cached) = reported.get("cache_read_input_tokens").and_then(serde_json::Value::as_u64) {
            usage.cached_tokens = cached;
        }
    }
}

fn event_index(event: &serde_json::Value) -> Result<u32, String> {
    let index = event.get("index").and_then(serde_json::Value::as_u64).ok_or("stream event has no index")?;
    u32::try_from(index).map_err(|_| format!("content block index {index} exceeds u32"))
}

fn anthropic_stop_reason(reason: &str) -> StopReason {
    match reason {
        "end_turn" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        "pause_turn" => StopReason::PauseTurn,
        "refusal" => StopReason::Refusal,
        "model_context_window_exceeded" => StopReason::ModelContextWindowExceeded,
        other => StopReason::Other(other.to_owned()),
    }
}

async fn read_anthropic_sse(response: reqwest::Response, thinking: Thinking) -> Result<Completion, LlmError> {
    let provider = ProviderKind::Anthropic.name().to_owned();
    let header_request_id = response
        .headers()
        .get("request-id")
        .or_else(|| response.headers().get("x-request-id"))
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut received = 0usize;
    let mut state = AnthropicStream::default();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|error| LlmError::Transport { provider: provider.clone(), detail: error.to_string() })?;
        received = received.saturating_add(chunk.len());
        if received > MAX_ANTHROPIC_STREAM_BYTES {
            return Err(LlmError::BadResponse {
                provider,
                detail: format!("stream exceeds {MAX_ANTHROPIC_STREAM_BYTES} bytes"),
            });
        }
        buffer.extend_from_slice(&chunk);
        while let Some(end) = find_sse_event_end(&buffer) {
            if end > MAX_ANTHROPIC_EVENT_BYTES {
                return Err(LlmError::BadResponse {
                    provider,
                    detail: format!("SSE event exceeds {MAX_ANTHROPIC_EVENT_BYTES} bytes"),
                });
            }
            let event: Vec<u8> = buffer.drain(..end).collect();
            if let Some(payload) = sse_payload(&event)
                .map_err(|detail| LlmError::BadResponse { provider: provider.clone(), detail })?
            {
                if payload == "[DONE]" {
                    state.saw_message_stop = true;
                } else {
                    let json: serde_json::Value =
                        serde_json::from_str(payload).map_err(|error| LlmError::BadResponse {
                            provider: provider.clone(),
                            detail: format!("event is not JSON: {error}"),
                        })?;
                    state
                        .event(&json)
                        .map_err(|detail| LlmError::BadResponse { provider: provider.clone(), detail })?;
                }
            }
        }
        if buffer.len() > MAX_ANTHROPIC_EVENT_BYTES {
            return Err(LlmError::BadResponse {
                provider,
                detail: format!("SSE event exceeds {MAX_ANTHROPIC_EVENT_BYTES} bytes"),
            });
        }
    }

    if !buffer.iter().all(u8::is_ascii_whitespace) {
        return Err(LlmError::BadResponse {
            provider,
            detail: "stream ended inside an SSE event".to_owned(),
        });
    }
    if !state.saw_message_stop {
        return Err(LlmError::BadResponse {
            provider,
            detail: "stream ended before message_stop".to_owned(),
        });
    }
    if !state.open.is_empty() {
        return Err(LlmError::BadResponse {
            provider,
            detail: "stream ended with an open content block".to_owned(),
        });
    }
    let stop_reason = state.stop_reason.ok_or_else(|| LlmError::BadResponse {
        provider: provider.clone(),
        detail: "message_stop arrived without a stop_reason".to_owned(),
    })?;
    let request_id = header_request_id.or(state.request_id);
    Ok(Completion {
        content: state.complete.into_values().collect(),
        stop_reason,
        stop_sequence: state.stop_sequence,
        model: state.model,
        request_id,
        usage: state.usage,
        thinking,
    })
}

fn find_sse_event_end(buffer: &[u8]) -> Option<usize> {
    let unix = buffer.windows(2).position(|pair| pair == b"\n\n").map(|at| at + 2);
    let windows = buffer.windows(4).position(|part| part == b"\r\n\r\n").map(|at| at + 4);
    match (unix, windows) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, right) => left.or(right),
    }
}

fn sse_payload(event: &[u8]) -> Result<Option<&str>, String> {
    let text = std::str::from_utf8(event).map_err(|error| format!("SSE event is not UTF-8: {error}"))?;
    let mut data = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("data:") {
            if data.is_some() {
                return Err("SSE event has multiple data lines".to_owned());
            }
            data = Some(value.trim());
        }
    }
    Ok(data)
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
                content: ChatCompletionRequestUserMessageContent::Text(message.text_content()),
                name: None,
            }),
            Role::Assistant => ChatCompletionRequestMessage::Assistant(
                async_openai::types::chat::ChatCompletionRequestAssistantMessage {
                    content: Some(
                        async_openai::types::chat::ChatCompletionRequestAssistantMessageContent::Text(
                            message.text_content(),
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
    let mut content = Vec::new();
    let mut stop_reason = StopReason::EndTurn;
    for choice in &response.choices {
        if let Some(part) = &choice.message.content {
            content.push(ContentBlock::text(part.clone()));
        }
        if let Some(tool_calls) = &choice.message.tool_calls {
            content.extend(tool_calls.iter().map(openai_tool_call));
        }
        if let Some(reason) = choice.finish_reason {
            stop_reason = openai_stop_reason(reason);
        }
    }
    let usage = response.usage.map(|reported| Usage {
        input_tokens: u64::from(reported.prompt_tokens),
        output_tokens: u64::from(reported.completion_tokens),
        cached_tokens: 0,
    });
    Completion {
        content,
        stop_reason,
        stop_sequence: None,
        model: response.model,
        request_id: Some(response.id),
        usage,
        thinking,
    }
}

fn openai_tool_call(call: &async_openai::types::chat::ChatCompletionMessageToolCalls) -> ContentBlock {
    match call {
        async_openai::types::chat::ChatCompletionMessageToolCalls::Function(call) => {
            let input = serde_json::from_str(&call.function.arguments)
                .unwrap_or_else(|_| serde_json::Value::String(call.function.arguments.clone()));
            ContentBlock::ToolUse { id: call.id.clone(), name: call.function.name.clone(), input }
        }
        async_openai::types::chat::ChatCompletionMessageToolCalls::Custom(call) => ContentBlock::ToolUse {
            id: call.id.clone(),
            name: call.custom_tool.name.clone(),
            input: serde_json::Value::String(call.custom_tool.input.clone()),
        },
    }
}

fn openai_stop_reason(reason: async_openai::types::chat::FinishReason) -> StopReason {
    match reason {
        async_openai::types::chat::FinishReason::Stop => StopReason::EndTurn,
        async_openai::types::chat::FinishReason::Length => StopReason::MaxTokens,
        async_openai::types::chat::FinishReason::ToolCalls
        | async_openai::types::chat::FinishReason::FunctionCall => StopReason::ToolUse,
        async_openai::types::chat::FinishReason::ContentFilter => StopReason::ContentFilter,
    }
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

    let mut content = Vec::new();
    let mut tool_calls = std::collections::BTreeMap::<u32, OpenAiToolCall>::new();
    let mut usage: Option<Usage> = None;
    let mut stop_reason = None;
    let mut model = String::new();
    let mut request_id = None;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| map_openai(&error))?;
        model.clone_from(&chunk.model);
        request_id = Some(chunk.id.clone());
        for choice in &chunk.choices {
            if let Some(part) = &choice.delta.content {
                content.push(ContentBlock::text(part.clone()));
            }
            if let Some(deltas) = &choice.delta.tool_calls {
                for delta in deltas {
                    let call = tool_calls.entry(delta.index).or_default();
                    if let Some(id) = &delta.id {
                        call.id.clone_from(id);
                    }
                    if let Some(function) = &delta.function {
                        if let Some(name) = &function.name {
                            call.name.push_str(name);
                        }
                        if let Some(arguments) = &function.arguments {
                            call.arguments.push_str(arguments);
                        }
                    }
                }
            }
            if let Some(reason) = choice.finish_reason {
                stop_reason = Some(openai_stop_reason(reason));
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

    let Some(stop_reason) = stop_reason else {
        let detail = if content.is_empty() {
            "the stream ended with no events".to_owned()
        } else {
            "the stream ended before a terminal marker".to_owned()
        };
        return Err(LlmError::BadResponse { provider: ProviderKind::OpenAI.name().to_owned(), detail });
    };
    content.extend(tool_calls.into_values().map(|call| {
        let input =
            serde_json::from_str(&call.arguments).unwrap_or(serde_json::Value::String(call.arguments));
        ContentBlock::ToolUse { id: call.id, name: call.name, input }
    }));
    Ok(Completion { content, stop_reason, stop_sequence: None, model, request_id, usage, thinking })
}

#[derive(Default)]
struct OpenAiToolCall {
    id: String,
    name: String,
    arguments: String,
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
            messages: vec![crate::client::Message::text(Role::User, "hi")],
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
        assert!(openai_tools(&anthropic).expect("no tools").is_none());
        let body = anthropic_body(&anthropic, DEFAULT_ANTHROPIC_MODEL).expect("body renders");
        assert!(!body.as_str().contains("\"tools\""), "{}", body.as_str());
    }

    #[test]
    fn anthropic_preserves_complete_tool_schemas() {
        let request = Request {
            tools: vec![crate::canonicalize(
                r#"{"description":"lookup","input_schema":{"additionalProperties":false,"properties":{"query":{"minLength":1,"type":"string"}},"required":["query"],"type":"object"},"name":"lookup"}"#,
            )
            .expect("canonical tool")],
            ..sample_request(ProviderKind::Anthropic)
        };

        let body = anthropic_body(&request, DEFAULT_ANTHROPIC_MODEL).expect("body renders");
        let parsed: serde_json::Value = serde_json::from_str(body.as_str()).expect("body is JSON");
        let schema = &parsed["tools"][0]["input_schema"];
        assert_eq!(schema["required"], serde_json::json!(["query"]));
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["properties"]["query"]["minLength"], 1);
    }

    #[test]
    fn provider_stop_reasons_remain_distinct() {
        assert_eq!(anthropic_stop_reason("end_turn"), StopReason::EndTurn);
        assert_eq!(anthropic_stop_reason("tool_use"), StopReason::ToolUse);
        assert_eq!(anthropic_stop_reason("max_tokens"), StopReason::MaxTokens);
        assert_eq!(anthropic_stop_reason("pause_turn"), StopReason::PauseTurn);
        assert_eq!(anthropic_stop_reason("refusal"), StopReason::Refusal);
        assert_eq!(
            openai_stop_reason(async_openai::types::chat::FinishReason::Length),
            StopReason::MaxTokens
        );
        assert_eq!(
            openai_stop_reason(async_openai::types::chat::FinishReason::ToolCalls),
            StopReason::ToolUse
        );
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
        assert_eq!(completion.text(), "live-ok");
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
    fn anthropic_retry_after_obeys_headers() {
        use reqwest::header::{HeaderMap, HeaderValue};

        let mut headers = HeaderMap::new();
        assert_eq!(provider_retry_after_ms(&headers), 60_000);
        headers.insert("retry-after-ms", HeaderValue::from_static("1250"));
        assert_eq!(provider_retry_after_ms(&headers), 1_250);
        headers.insert(reqwest::header::RETRY_AFTER, HeaderValue::from_static("4"));
        assert_eq!(provider_retry_after_ms(&headers), 4_000);
    }

    #[test]
    fn anthropic_stream_state_preserves_ordered_protocol_blocks() {
        let mut stream = AnthropicStream::default();
        for event in [
            serde_json::json!({"type":"message_start","message":{"id":"msg_1","model":"claude-test","content":[],"usage":{"input_tokens":7}}}),
            serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
            serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"reason"}}),
            serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}),
            serde_json::json!({"type":"content_block_stop","index":0}),
            serde_json::json!({"type":"content_block_start","index":1,"content_block":{"type":"redacted_thinking","data":"opaque"}}),
            serde_json::json!({"type":"content_block_stop","index":1}),
            serde_json::json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"tool_1","name":"lookup","input":{}}}),
            serde_json::json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"query\":\"x\"}"}}),
            serde_json::json!({"type":"content_block_stop","index":2}),
            serde_json::json!({"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":11}}),
            serde_json::json!({"type":"message_stop"}),
        ] {
            stream.event(&event).expect("valid event");
        }

        assert_eq!(stream.stop_reason, Some(StopReason::ToolUse));
        assert_eq!(stream.model, "claude-test");
        assert_eq!(stream.request_id.as_deref(), Some("msg_1"));
        assert_eq!(stream.usage, Some(Usage { input_tokens: 7, output_tokens: 11, cached_tokens: 0 }));
        assert_eq!(
            stream.complete.into_values().collect::<Vec<_>>(),
            vec![
                ContentBlock::Thinking { thinking: "reason".to_owned(), signature: "sig".to_owned() },
                ContentBlock::RedactedThinking { data: "opaque".to_owned() },
                ContentBlock::ToolUse {
                    id: "tool_1".to_owned(),
                    name: "lookup".to_owned(),
                    input: serde_json::json!({"query":"x"}),
                },
            ]
        );
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
            messages: vec![crate::client::Message::text(Role::User, "Reply with exactly: live-ok")],
            tools: Vec::new(),
            thinking: Thinking { budget_tokens: 0 },
            breakpoints: Vec::new(),
        };
        let completion = send_anthropic(
            reqwest::Client::new(),
            &request,
            Some(&base),
            &model,
            Thinking { budget_tokens: 0 },
            &credential,
        )
        .await
        .expect("the gateway answers through the Messages transport");

        assert_eq!(completion.text().trim(), "live-ok", "the stream carried the exact answer");
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
            messages: vec![crate::client::Message::text(Role::User, "Reply with exactly: live-ok")],
            tools: Vec::new(),
            thinking: Thinking { budget_tokens: 0 },
            breakpoints: Vec::new(),
        };
        let completion =
            send_openai(&request, Some(&base), &model, Thinking { budget_tokens: 0 }, None, &credential)
                .await
                .expect("the gateway answers through the SDK");

        assert!(!completion.text().trim().is_empty(), "the stream carried text");
        if let Some(usage) = completion.usage {
            assert!(usage.input_tokens > 0, "input usage is reported: {usage:?}");
            assert!(usage.output_tokens > 0, "output usage is reported: {usage:?}");
        }
        println!("live openai text: {:?}", completion.text().trim());
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
