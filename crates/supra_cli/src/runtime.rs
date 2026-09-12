#![deny(missing_docs)]

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use supra_config::Config;
use supra_llm::{Completion, ContentBlock, LlmError, Message, Request, Role, StopReason, Thinking};

const MAX_PROVIDER_ATTEMPTS: usize = 3;
const PROVIDER_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_RATE_LIMIT_DELAY: Duration = Duration::from_secs(30);
const TRANSPORT_RETRY_BASE: Duration = Duration::from_millis(250);

type ProviderFuture<'a> = Pin<Box<dyn Future<Output = Result<Completion, LlmError>> + Send + 'a>>;

trait ProviderBackend: Send + Sync + 'static {
    fn provider(&self) -> supra_llm::ProviderKind;
    fn model(&self) -> &str;
    fn thinking(&self) -> Thinking;
    fn send(&self, request: Request) -> ProviderFuture<'_>;
}

struct LiveProvider {
    client: supra_llm::Client,
    credential: supra_secrets::SecretString,
}

impl ProviderBackend for LiveProvider {
    fn provider(&self) -> supra_llm::ProviderKind {
        self.client.provider()
    }

    fn model(&self) -> &str {
        self.client.model()
    }

    fn thinking(&self) -> Thinking {
        self.client.thinking()
    }

    fn send(&self, request: Request) -> ProviderFuture<'_> {
        Box::pin(async move { self.client.send(&request, &self.credential).await })
    }
}

/// The durable result of one executable CLI turn.
pub(crate) struct TurnResult {
    /// The persisted session that owns the accepted turn.
    pub(crate) session: supra_types::SessionId,
    /// The provider answer accepted by the turn state machine.
    pub(crate) answer: String,
}

/// Execute one provider-backed turn and persist its accepted answer.
///
/// The proposer and validators are independent requests. Validators receive
/// the task and candidate and must return a bounded structured verdict. E0's
/// single peer performs a second blind-validation call before the answer can
/// be sealed. Tool use is rejected because the CLI has no executable tool
/// dispatch loop yet; returning it as terminal text would certify an
/// unexecuted action.
pub(crate) async fn execute_turn(
    config: &Config,
    secrets: &supra_secrets::SecretManager,
    session_dir: &Path,
    requested_provider: Option<&str>,
    task: &str,
) -> anyhow::Result<TurnResult> {
    let task = task.trim();
    if task.is_empty() {
        anyhow::bail!("run requires a non-empty task");
    }
    let provider = select_provider(config, requested_provider)?;
    let client = supra_llm::Client::from_config(config, provider)?;
    let credential = config.provider_secret(provider, secrets)?;
    execute_turn_with_backend(config, session_dir, task, Arc::new(LiveProvider { client, credential })).await
}

async fn execute_turn_with_backend<B: ProviderBackend>(
    config: &Config,
    session_dir: &Path,
    task: &str,
    backend: Arc<B>,
) -> anyhow::Result<TurnResult> {
    let (_, cohort_size) = plan_turn(config, estimate_tier(task))
        .admitted
        .ok_or_else(|| anyhow::anyhow!("cohort limit leaves no admissible turn"))?;
    let agents: Vec<supra_types::AgentId> =
        (0..cohort_size).map(|_| supra_types::AgentId::generate()).collect();

    let store = Arc::new(supra_store::Store::open_in_memory()?);
    let board = supra_blackboard::Blackboard::open(store)?;
    let bus = Arc::new(supra_eventbus::Bus::new());
    let mut turn = supra_core::Turn::start_shared(board, bus, task, agents.clone())?;

    let proposal = terminal_text(send_with_retry(backend.as_ref(), request(backend.as_ref(), task)).await?)?;
    turn.record(supra_core::PeerAnswer::proposal(agents[0], proposal.clone()))?;

    let validators: &[supra_types::AgentId] = if cohort_size == 1 { &agents[..1] } else { &agents[1..] };
    let prompt = validation_prompt(task, &proposal);
    let mut tasks = tokio::task::JoinSet::new();
    for agent in validators {
        let backend = Arc::clone(&backend);
        let request = request(backend.as_ref(), &prompt);
        let agent = *agent;
        tasks.spawn(async move {
            let result = async {
                let completion = send_with_retry(backend.as_ref(), request).await?;
                let text = terminal_text(completion)?;
                parse_verdict(&text)
            }
            .await;
            (agent, result)
        });
    }

    let mut reached = false;
    let mut first_failure: Option<anyhow::Error> = None;
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((agent, Ok(verdict))) => {
                match turn.record(supra_core::PeerAnswer::validation(agent, verdict))? {
                    supra_core::Step::Reached => {
                        abort_and_drain(&mut tasks).await;
                        reached = true;
                        break;
                    }
                    supra_core::Step::Escalate => {
                        abort_and_drain(&mut tasks).await;
                        anyhow::bail!("peer quorum rejected the provider answer");
                    }
                    supra_core::Step::Collecting => {}
                }
            }
            Ok((_, Err(error))) => {
                if first_failure.is_none() {
                    first_failure = Some(error);
                }
            }
            Err(error) => {
                if first_failure.is_none() {
                    first_failure = Some(anyhow::anyhow!("validator task failed: {error}"));
                }
            }
        }
    }
    if !reached {
        if let Some(error) = first_failure {
            return Err(anyhow::anyhow!("peer validation ended before quorum: {error}"));
        }
        anyhow::bail!("peer validators completed without reaching quorum");
    }

    let turn_id = turn.id();
    let mut ledger = supra_prompt::PromptLedger::new();
    turn.finish(&mut ledger)?;
    let mut session = supra_session::Session::new();
    session.record_turn_with_body(turn_id, &proposal);
    session.save(session_dir)?;
    Ok(TurnResult { session: session.id(), answer: proposal })
}

async fn abort_and_drain<T: 'static>(tasks: &mut tokio::task::JoinSet<T>) {
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
}

fn select_provider<'a>(config: &'a Config, requested: Option<&'a str>) -> anyhow::Result<&'a str> {
    if let Some(name) = requested {
        if config.providers().contains_key(name) {
            supra_llm::ProviderKind::parse(name)
                .ok_or_else(|| anyhow::anyhow!("configured provider {name:?} is not supported"))?;
            return Ok(name);
        }
        anyhow::bail!("provider {name:?} is not configured");
    }
    for preferred in ["anthropic", "openai", "google"] {
        if config.providers().contains_key(preferred) {
            return Ok(preferred);
        }
    }
    if let Some(name) = config.providers().keys().next() {
        anyhow::bail!("no supported provider is configured (found {name:?})");
    }
    anyhow::bail!("no provider is configured")
}

async fn send_with_retry<B: ProviderBackend + ?Sized>(
    backend: &B,
    request: Request,
) -> Result<Completion, LlmError> {
    for attempt in 1..=MAX_PROVIDER_ATTEMPTS {
        let result = match tokio::time::timeout(PROVIDER_ATTEMPT_TIMEOUT, backend.send(request.clone())).await
        {
            Ok(result) => result,
            Err(_) => Err(LlmError::Transport {
                provider: backend.provider().name().to_owned(),
                detail: format!("request attempt {attempt} exceeded {PROVIDER_ATTEMPT_TIMEOUT:?}"),
            }),
        };
        match result {
            Ok(completion) => return Ok(completion),
            Err(error) if attempt < MAX_PROVIDER_ATTEMPTS => {
                let Some(delay) = retry_delay(&error, attempt) else {
                    return Err(error);
                };
                tokio::time::sleep(delay).await;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("the non-empty bounded attempt loop always returns")
}

fn retry_delay(error: &LlmError, attempt: usize) -> Option<Duration> {
    match error {
        LlmError::RateLimited { retry_after_ms, .. } => {
            let delay = Duration::from_millis(*retry_after_ms);
            (delay <= MAX_RATE_LIMIT_DELAY).then_some(delay)
        }
        LlmError::Transport { .. } => {
            let exponent = u32::try_from(attempt.saturating_sub(1)).unwrap_or(u32::MAX).min(8);
            Some(TRANSPORT_RETRY_BASE.saturating_mul(2_u32.saturating_pow(exponent)))
        }
        _ => None,
    }
}

fn request<B: ProviderBackend + ?Sized>(backend: &B, prompt: &str) -> Request {
    Request {
        provider: backend.provider(),
        model: backend.model().to_owned(),
        messages: vec![Message::text(Role::User, prompt)],
        tools: Vec::new(),
        thinking: backend.thinking(),
        breakpoints: Vec::new(),
    }
}

fn validation_prompt(task: &str, candidate: &str) -> String {
    format!(
        "Independently validate the candidate against the task. Reply with JSON only: \
         {{\"vote\":\"yes|no|abstain\",\"confidence\":\"low|medium|high\",\"reason\":\"at most 140 bytes\"}}.\nTask:\n{task}\nCandidate:\n{candidate}"
    )
}

fn terminal_text(completion: Completion) -> anyhow::Result<String> {
    if completion.content.iter().any(|block| matches!(block, ContentBlock::ToolUse { .. }))
        || completion.stop_reason == StopReason::ToolUse
    {
        anyhow::bail!("provider requested tool use, but the CLI tool loop is not available");
    }
    if completion.stop_reason != StopReason::EndTurn && completion.stop_reason != StopReason::StopSequence {
        anyhow::bail!("provider did not complete the turn: {:?}", completion.stop_reason);
    }
    let text = completion.text();
    if text.trim().is_empty() {
        anyhow::bail!("provider completed the turn without text");
    }
    Ok(text)
}

fn parse_verdict(text: &str) -> anyhow::Result<supra_types::Verdict> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct WireVerdict {
        vote: String,
        confidence: String,
        reason: String,
    }
    let trimmed = text.trim();
    let json = if let Some(fenced) = trimmed.strip_prefix("```json") {
        fenced
            .strip_suffix("```")
            .ok_or_else(|| anyhow::anyhow!("validator returned an unterminated JSON fence"))?
    } else if let Some(fenced) = trimmed.strip_prefix("```") {
        fenced
            .strip_suffix("```")
            .ok_or_else(|| anyhow::anyhow!("validator returned an unterminated JSON fence"))?
    } else {
        trimmed
    }
    .trim();
    let wire: WireVerdict = serde_json::from_str(json)
        .map_err(|error| anyhow::anyhow!("validator returned malformed verdict JSON: {error}"))?;
    let vote = match wire.vote.as_str() {
        "yes" => supra_types::Vote::Yes,
        "no" => supra_types::Vote::No,
        "abstain" => supra_types::Vote::Abstain,
        other => anyhow::bail!("validator returned unknown vote {other:?}"),
    };
    let confidence = match wire.confidence.as_str() {
        "low" => supra_types::Confidence::Low,
        "medium" => supra_types::Confidence::Medium,
        "high" => supra_types::Confidence::High,
        other => anyhow::bail!("validator returned unknown confidence {other:?}"),
    };
    Ok(supra_types::Verdict::new(vote, confidence, wire.reason, None)?)
}

pub(crate) struct Plan {
    pub(crate) admitted: Option<(supra_types::Tier, usize)>,
}

pub(crate) fn plan_turn(config: &Config, requested: supra_types::Tier) -> Plan {
    Plan { admitted: supra_types::admit(requested, config.cohort_limit()) }
}

pub(crate) fn estimate_tier(task: &str) -> supra_types::Tier {
    let paths: Vec<&str> = task
        .split_whitespace()
        .filter(|word| word.contains('/') || word.contains('\\'))
        .map(|word| {
            word.trim_matches(|character: char| {
                character.is_ascii_punctuation() && character != '/' && character != '\\'
            })
        })
        .filter(|word| !word.is_empty())
        .collect();
    let mut signals = supra_cohort::Signals::minimal();
    signals.anchors = supra_cohort::AnchorBand::of(paths.len().min(10));
    if paths.len() > 1 {
        signals.blast = supra_cohort::BlastBand::Contained;
    }
    if task_is_mutating(task) {
        signals.reversibility = supra_types::Reversibility::R1;
    }
    let areas = supra_cohort::AreaFlags::of_paths(&paths);
    supra_cohort::estimate(&signals, &areas)
}

fn task_is_mutating(task: &str) -> bool {
    const MUTATING_WORDS: &[&str] = &[
        "add",
        "apply",
        "build",
        "change",
        "commit",
        "create",
        "delete",
        "edit",
        "fix",
        "implement",
        "install",
        "migrate",
        "modify",
        "move",
        "publish",
        "refactor",
        "release",
        "remove",
        "rename",
        "replace",
        "run",
        "save",
        "update",
        "write",
    ];
    task.split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .any(|word| MUTATING_WORDS.contains(&word.to_ascii_lowercase().as_str()))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, MutexGuard};

    use super::*;
    use supra_config::ConfigLayer;

    #[derive(Clone)]
    struct ScriptedReply {
        delay: Duration,
        result: Result<Completion, LlmError>,
    }

    struct MockProvider {
        replies: Mutex<VecDeque<ScriptedReply>>,
        calls: AtomicUsize,
        completed: AtomicUsize,
    }

    impl MockProvider {
        fn new(replies: impl IntoIterator<Item = ScriptedReply>) -> Self {
            Self {
                replies: Mutex::new(replies.into_iter().collect()),
                calls: AtomicUsize::new(0),
                completed: AtomicUsize::new(0),
            }
        }

        fn replies(&self) -> MutexGuard<'_, VecDeque<ScriptedReply>> {
            self.replies.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn completed(&self) -> usize {
            self.completed.load(Ordering::SeqCst)
        }
    }

    impl ProviderBackend for MockProvider {
        fn provider(&self) -> supra_llm::ProviderKind {
            supra_llm::ProviderKind::OpenAI
        }

        fn model(&self) -> &'static str {
            "mock-model"
        }

        fn thinking(&self) -> Thinking {
            Thinking { budget_tokens: 0 }
        }

        fn send(&self, _request: Request) -> ProviderFuture<'_> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let reply = self.replies().pop_front();
            Box::pin(async move {
                let reply = reply.ok_or_else(|| LlmError::BadResponse {
                    provider: "openai".to_owned(),
                    detail: "mock reply queue exhausted".to_owned(),
                })?;
                tokio::time::sleep(reply.delay).await;
                self.completed.fetch_add(1, Ordering::SeqCst);
                reply.result
            })
        }
    }

    fn text_reply(text: &str, delay: Duration) -> ScriptedReply {
        ScriptedReply { delay, result: Ok(Completion::end_turn(text, None, Thinking { budget_tokens: 0 })) }
    }

    fn yes_reply(delay: Duration) -> ScriptedReply {
        text_reply(r#"{"vote":"yes","confidence":"high","reason":"checked"}"#, delay)
    }

    fn transport_failure() -> ScriptedReply {
        ScriptedReply {
            delay: Duration::ZERO,
            result: Err(LlmError::Transport {
                provider: "openai".to_owned(),
                detail: "temporary".to_owned(),
            }),
        }
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "supra-cli-runtime-{name}-{}-{}",
            std::process::id(),
            supra_types::SessionId::generate()
        ));
        std::fs::create_dir_all(&path).expect("scratch directory");
        path
    }

    fn config_with_limit(limit: usize) -> Config {
        let text = format!("[cohort]\nlimit = {limit}\n");
        let layer = ConfigLayer::parse(&text, supra_config::ConfigSource::User, "test".into())
            .expect("valid fixture");
        layer.validate(supra_config::ConfigSource::User).expect("valid fixture");
        supra_config::resolve(&[(supra_config::ConfigSource::User, layer)])
    }

    fn config_with_provider(name: &str) -> Config {
        let text = format!("[providers.{name}]\napi_key_env = \"SUPRA_TEST_KEY\"\n");
        let layer = ConfigLayer::parse(&text, supra_config::ConfigSource::User, "test".into())
            .expect("valid fixture");
        layer.validate(supra_config::ConfigSource::User).expect("valid fixture");
        supra_config::resolve(&[(supra_config::ConfigSource::User, layer)])
    }

    #[test]
    fn task_evidence_selects_nonminimal_tiers() {
        assert_eq!(estimate_tier("explain"), supra_types::Tier::E0);
        assert_eq!(estimate_tier("fix bug"), supra_types::Tier::E1);
        assert_eq!(estimate_tier("review src/lib.rs"), supra_types::Tier::E1);
        assert_eq!(estimate_tier("compare src/lib.rs tests/lib.rs"), supra_types::Tier::E2);
        assert_eq!(estimate_tier("fix src/auth/session.rs"), supra_types::Tier::E4);
    }

    #[test]
    fn admission_never_leaves_a_tier_gap() {
        for requested in [
            supra_types::Tier::E0,
            supra_types::Tier::E1,
            supra_types::Tier::E2,
            supra_types::Tier::E3,
            supra_types::Tier::E4,
            supra_types::Tier::E5,
        ] {
            for limit in 1..=80usize {
                let plan = plan_turn(&config_with_limit(limit), requested);
                if let Some((tier, k)) = plan.admitted {
                    assert_eq!(supra_types::Tier::containing(k), Some(tier));
                }
            }
        }
    }

    #[test]
    fn admission_reduces_to_a_complete_tier() {
        let plan = plan_turn(&config_with_limit(6), supra_types::Tier::E3);
        assert_eq!(plan.admitted, Some((supra_types::Tier::E2, 5)));
    }

    #[test]
    fn verdict_json_is_bounded_and_structured() {
        let verdict = parse_verdict(
            r#"```json
{"vote":"yes","confidence":"high","reason":"checked independently"}
```"#,
        )
        .expect("verdict");
        assert_eq!(verdict.vote(), supra_types::Vote::Yes);
        assert_eq!(verdict.confidence(), supra_types::Confidence::High);
    }

    #[test]
    fn an_unterminated_verdict_fence_is_rejected() {
        let error = parse_verdict(
            r#"```json
{"vote":"yes","confidence":"high","reason":"checked independently"}"#,
        )
        .expect_err("an incomplete fence must not be repaired implicitly");
        assert!(error.to_string().contains("unterminated"), "{error}");
    }

    #[test]
    fn provider_selection_refuses_unsupported_config_entries() {
        let config = config_with_provider("custom");
        let error = select_provider(&config, None).expect_err("custom has no transport");
        assert!(error.to_string().contains("no supported provider"), "{error}");
        let error = select_provider(&config, Some("custom")).expect_err("custom has no transport");
        assert!(error.to_string().contains("not supported"), "{error}");
    }

    #[test]
    fn provider_selection_uses_supported_transports_only() {
        for name in ["anthropic", "openai", "google"] {
            let config = config_with_provider(name);
            assert_eq!(select_provider(&config, None).expect("supported"), name);
            assert_eq!(select_provider(&config, Some(name)).expect("requested"), name);
        }
    }

    #[test]
    fn request_uses_the_clients_frozen_provider_model_and_budget() {
        let client = supra_llm::Client::new(
            "openai",
            "https://example.invalid".to_owned(),
            "pinned-model".to_owned(),
            17,
        )
        .expect("client");
        let credential = supra_secrets::SecretString::new("test".to_owned());
        let backend = LiveProvider { client, credential };
        let request = request(&backend, "task");
        assert_eq!(request.provider, supra_llm::ProviderKind::OpenAI);
        assert_eq!(request.model, "pinned-model");
        assert_eq!(request.thinking.budget_tokens, 17);
        assert_eq!(request.messages, vec![Message::text(Role::User, "task")]);
    }

    #[test]
    fn tool_use_is_never_returned_as_a_finished_answer() {
        let completion = Completion {
            content: vec![ContentBlock::ToolUse {
                id: "call".to_owned(),
                name: "read".to_owned(),
                input: serde_json::json!({}),
            }],
            stop_reason: StopReason::ToolUse,
            stop_sequence: None,
            model: "m".to_owned(),
            request_id: None,
            usage: None,
            thinking: supra_llm::Thinking { budget_tokens: 0 },
        };
        let error = terminal_text(completion).expect_err("tool use must not pass");
        assert!(error.to_string().contains("tool loop"), "{error}");
    }

    #[tokio::test(start_paused = true)]
    async fn transport_failures_retry_but_bad_responses_do_not() {
        let retried = MockProvider::new([transport_failure(), text_reply("ok", Duration::ZERO)]);
        let completion =
            send_with_retry(&retried, request(&retried, "task")).await.expect("transport retry succeeds");
        assert_eq!(completion.text(), "ok");
        assert_eq!(retried.calls(), 2);

        let terminal = MockProvider::new([ScriptedReply {
            delay: Duration::ZERO,
            result: Err(LlmError::BadResponse {
                provider: "openai".to_owned(),
                detail: "shape changed".to_owned(),
            }),
        }]);
        assert!(send_with_retry(&terminal, request(&terminal, "task")).await.is_err());
        assert_eq!(terminal.calls(), 1, "non-retryable failures stop immediately");
    }

    #[tokio::test(start_paused = true)]
    async fn provider_rate_limit_delays_are_bounded() {
        let too_long = MockProvider::new([ScriptedReply {
            delay: Duration::ZERO,
            result: Err(LlmError::RateLimited { provider: "openai".to_owned(), retry_after_ms: 60_000 }),
        }]);
        assert!(send_with_retry(&too_long, request(&too_long, "task")).await.is_err());
        assert_eq!(too_long.calls(), 1, "a turn does not sleep beyond its retry budget");
    }

    #[tokio::test(start_paused = true)]
    async fn validators_complete_out_of_order_and_terminal_quorum_cancels_the_rest() {
        let backend = Arc::new(MockProvider::new([
            text_reply("accepted answer", Duration::ZERO),
            yes_reply(Duration::from_millis(30)),
            yes_reply(Duration::from_secs(60)),
            yes_reply(Duration::from_millis(10)),
            yes_reply(Duration::from_millis(20)),
        ]));
        let session_dir = scratch("quorum-cancel");
        let result = execute_turn_with_backend(
            &config_with_limit(5),
            &session_dir,
            "compare src/a.rs tests/a.rs",
            Arc::clone(&backend),
        )
        .await
        .expect("fast validators reach quorum");

        assert_eq!(result.answer, "accepted answer");
        assert_eq!(backend.calls(), 5, "all validator requests fan out together");
        assert_eq!(backend.completed(), 4, "the slow request is cancelled at quorum");
        let resumed = supra_session::Session::resume(&session_dir, result.session)
            .expect("resume")
            .expect("saved session");
        assert_eq!(resumed.bodies(), &["accepted answer"]);
        let _ = std::fs::remove_dir_all(session_dir);
    }
}
