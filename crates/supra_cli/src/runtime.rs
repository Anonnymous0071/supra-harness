#![deny(missing_docs)]

use std::path::Path;

use supra_config::Config;
use supra_llm::{Completion, ContentBlock, Message, Request, Role, StopReason};

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
    let (_, cohort_size) = plan_turn(config, estimate_tier())
        .admitted
        .ok_or_else(|| anyhow::anyhow!("cohort limit leaves no admissible turn"))?;
    let agents: Vec<supra_types::AgentId> =
        (0..cohort_size).map(|_| supra_types::AgentId::generate()).collect();

    let store = std::sync::Arc::new(supra_store::Store::open_in_memory()?);
    let board = supra_blackboard::Blackboard::open(store)?;
    let mut turn = supra_core::Turn::start(board, supra_eventbus::Bus::new(), task, agents.clone())?;

    let proposal = send_text(&client, &credential, task).await?;
    turn.record(supra_core::PeerAnswer::proposal(agents[0], proposal.clone()))?;

    let validators: &[supra_types::AgentId] = if cohort_size == 1 { &agents[..1] } else { &agents[1..] };
    for agent in validators {
        let verdict = validate_candidate(&client, &credential, task, &proposal).await?;
        match turn.record(supra_core::PeerAnswer::validation(*agent, verdict))? {
            supra_core::Step::Reached => break,
            supra_core::Step::Escalate => anyhow::bail!("peer quorum rejected the provider answer"),
            supra_core::Step::Collecting => {}
        }
    }

    let turn_id = turn.id();
    let mut ledger = supra_prompt::PromptLedger::new();
    turn.finish(&mut ledger)?;
    let mut session = supra_session::Session::new();
    session.record_turn_with_body(turn_id, &proposal);
    session.save(session_dir)?;
    Ok(TurnResult { session: session.id(), answer: proposal })
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

async fn send_text(
    client: &supra_llm::Client,
    credential: &supra_secrets::SecretString,
    task: &str,
) -> anyhow::Result<String> {
    let completion = client.send(&request(client, task), credential).await?;
    terminal_text(completion)
}

fn request(client: &supra_llm::Client, prompt: &str) -> Request {
    Request {
        provider: client.provider(),
        model: client.model().to_owned(),
        messages: vec![Message::text(Role::User, prompt)],
        tools: Vec::new(),
        thinking: client.thinking(),
        breakpoints: Vec::new(),
    }
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

async fn validate_candidate(
    client: &supra_llm::Client,
    credential: &supra_secrets::SecretString,
    task: &str,
    candidate: &str,
) -> anyhow::Result<supra_types::Verdict> {
    let prompt = format!(
        "Independently validate the candidate against the task. Reply with JSON only: \
         {{\"vote\":\"yes|no|abstain\",\"confidence\":\"low|medium|high\",\"reason\":\"at most 140 bytes\"}}.\nTask:\n{task}\nCandidate:\n{candidate}"
    );
    let text = send_text(client, credential, &prompt).await?;
    parse_verdict(&text)
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

pub(crate) fn estimate_tier() -> supra_types::Tier {
    let signals = supra_cohort::Signals::minimal();
    let areas = supra_cohort::AreaFlags::none();
    supra_cohort::estimate(&signals, &areas)
}

#[cfg(test)]
mod tests {
    use super::*;
    use supra_config::ConfigLayer;

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
    fn a_minimal_task_estimates_e0() {
        assert_eq!(estimate_tier(), supra_types::Tier::E0);
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
        let request = request(&client, "task");
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
}
