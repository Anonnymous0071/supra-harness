use std::collections::BTreeMap;
use std::process::Command;

use crate::error::HookError;
use crate::point::{HookContext, HookOutcome, HookPoint};

/// One registered hook: a point plus the command to run there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hook {
    /// When the hook runs.
    pub point: HookPoint,
    /// The command line to execute. Split on spaces; no shell.
    pub command: String,
}

/// The hook registry. Register-time is the only place prefix safety is
/// enforced: a point inside the prefix refuses here, not at fire time.
#[derive(Debug, Default)]
pub struct Registry {
    hooks: BTreeMap<HookPoint, Vec<Hook>>,
}

impl Registry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a hook. Refuses points inside the prefix.
    ///
    /// # Errors
    ///
    /// [`HookError::NotPrefixSafe`] when the point is not one of the
    /// four boundaries.
    pub fn register(&mut self, hook: Hook) -> Result<(), HookError> {
        if !hook.point.is_prefix_safe() {
            return Err(HookError::NotPrefixSafe { point: hook.point });
        }
        self.hooks.entry(hook.point).or_default().push(hook);
        Ok(())
    }

    /// Register from a configuration pair: point name plus command.
    ///
    /// # Errors
    ///
    /// [`HookError::UnknownPoint`] for an unknown name, then whatever
    /// [`Registry::register`] refuses.
    pub fn register_named(&mut self, point: &str, command: &str) -> Result<(), HookError> {
        let point = HookPoint::parse(point)?;
        self.register(Hook { point, command: command.to_owned() })
    }

    /// The hooks registered at one point.
    #[must_use]
    pub fn at(&self, point: HookPoint) -> &[Hook] {
        self.hooks.get(&point).map_or(&[], Vec::as_slice)
    }

    /// The total number of registered hooks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.hooks.values().map(Vec::len).sum()
    }

    /// Whether no hook is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Fire the hooks at one point. Each command runs to completion;
    /// a non-zero exit or a spawn failure is reported as
    /// [`HookError::Command`] but does not stop the other hooks - a
    /// hook is an observer, not a gate. The outcome is the first `Stop`
    /// any hook's exit code requests (exit 42), else `Continue`.
    ///
    /// The event travels as JSON on the hook's stdin: the hook sees
    /// what fired it, and nothing it could use to mutate the prefix.
    ///
    /// # Errors
    ///
    /// [`HookError::Command`] when a command cannot start or exits
    /// non-zero; the remaining hooks still run, and the error names
    /// the offender.
    pub fn fire(&self, point: HookPoint, context: &HookContext) -> Result<HookOutcome, HookError> {
        let mut outcome = HookOutcome::Continue;
        let mut failure: Option<HookError> = None;
        for hook in self.at(point) {
            let words = shell_words(&hook.command);
            let Some(program) = words.first().cloned() else {
                continue;
            };
            let rest = words[1..].to_vec();
            let payload = serde_json::to_string(&context.event).unwrap_or_else(|_| "{}".to_owned());
            let run = Command::new(program)
                .args(&rest)
                .env("SUPRA_HOOK_POINT", point.name())
                .env("SUPRA_TURN_COUNT", context.turn_count.to_string())
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .and_then(|mut child| {
                    use std::io::Write as _;
                    if let Some(mut stdin) = child.stdin.take() {
                        let _ = stdin.write_all(payload.as_bytes());
                    }
                    child.wait()
                });
            match run {
                Ok(status) if status.success() => {}
                Ok(status) if status.code() == Some(42) => outcome = HookOutcome::Stop,
                Ok(status) => {
                    if failure.is_none() {
                        failure = Some(HookError::Command(format!(
                            "{} exited {}",
                            hook.command,
                            status.code().unwrap_or(-1)
                        )));
                    }
                }
                Err(error) => {
                    if failure.is_none() {
                        failure = Some(HookError::Command(format!("{}: {error}", hook.command)));
                    }
                }
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(outcome),
        }
    }
}

/// Split a command line into words the way a POSIX shell would for the
/// subset hooks use: single- and double-quoted segments, backslash
/// escapes. No expansion, no redirection - a hook is an observer with
/// argv, not a shell pipeline.
fn shell_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for character in line.chars() {
        let single = quote == Some('\'');
        if escaped {
            current.push(character);
            escaped = false;
        } else if character == '\\' && quote != Some('\'') {
            escaped = true;
        } else if Some(character) == quote {
            quote = None;
        } else if character == '\'' || (character == '"' && quote.is_none()) {
            quote = Some(character);
        } else if character.is_whitespace() && quote.is_none() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
        } else {
            current.push(character);
        }
        let _ = single;
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use supra_types::Event;
    use supra_types::{SessionId, TurnId};

    fn context() -> HookContext {
        HookContext { event: Event::TurnStarted { turn: TurnId::generate() }, turn_count: 3 }
    }

    #[test]
    fn boundary_points_register_and_inside_points_refuse() {
        let mut registry = Registry::new();
        for point in
            [HookPoint::SessionStart, HookPoint::SessionEnd, HookPoint::TurnStart, HookPoint::TurnEnd]
        {
            registry.register(Hook { point, command: "true".to_owned() }).expect("boundary accepts");
        }
        for point in
            [HookPoint::BeforeTool, HookPoint::AfterTool, HookPoint::BeforeEvict, HookPoint::AfterCacheBreak]
        {
            let error = registry
                .register(Hook { point, command: "true".to_owned() })
                .expect_err("inside point refuses");
            assert!(matches!(error, HookError::NotPrefixSafe { .. }), "{error}");
        }
        assert_eq!(registry.len(), 4);
    }

    #[test]
    fn named_registration_parses_then_enforces() {
        let mut registry = Registry::new();
        registry.register_named("turn-start", "true").expect("boundary");
        let error = registry.register_named("before-tool", "true").expect_err("inside");
        assert!(matches!(error, HookError::NotPrefixSafe { .. }));
        let error = registry.register_named("not-a-point", "true").expect_err("unknown");
        assert!(matches!(error, HookError::UnknownPoint { .. }));
    }

    #[test]
    fn a_running_hook_sees_the_event_and_the_point() {
        let mut registry = Registry::new();
        registry
            .register_named(
                "turn-start",
                "python3 -c 'import os,sys,json; d=json.load(sys.stdin); sys.exit(0 if os.environ[\"SUPRA_HOOK_POINT\"]==\"turn-start\" and \"TurnStarted\" in d else 9)'",
            )
            .expect("register");
        let outcome = registry.fire(HookPoint::TurnStart, &context()).expect("fire");
        assert_eq!(outcome, HookOutcome::Continue);
    }

    #[test]
    fn exit_42_stops_and_zero_continues() {
        let mut registry = Registry::new();
        registry.register_named("turn-start", "true").expect("register");
        let outcome = registry.fire(HookPoint::TurnStart, &context()).expect("fire");
        assert_eq!(outcome, HookOutcome::Continue);

        let mut registry = Registry::new();
        registry.register_named("turn-start", "sh -c 'exit 42'").expect("register");
        let outcome = registry.fire(HookPoint::TurnStart, &context()).expect("fire");
        assert_eq!(outcome, HookOutcome::Stop);
    }

    #[test]
    fn a_failing_hook_reports_but_does_not_stop_the_others() {
        let mut registry = Registry::new();
        registry.register_named("turn-start", "sh -c 'exit 1'").expect("register");
        registry.register_named("turn-start", "true").expect("register 2");
        let error = registry.fire(HookPoint::TurnStart, &context()).expect_err("reports");
        assert!(matches!(error, HookError::Command(_)), "{error}");
    }

    #[test]
    fn a_command_that_cannot_start_is_reported_not_silent() {
        let mut registry = Registry::new();
        registry.register_named("turn-start", "definitely-not-a-program-271828").expect("register");
        let error = registry.fire(HookPoint::TurnStart, &context()).expect_err("reports");
        assert!(matches!(error, HookError::Command(_)), "{error}");
    }

    #[test]
    fn firing_an_empty_point_continues() {
        let registry = Registry::new();
        let outcome = registry.fire(HookPoint::TurnEnd, &context()).expect("fire");
        assert_eq!(outcome, HookOutcome::Continue);
    }

    #[test]
    fn the_payload_is_real_event_json() {
        let mut registry = Registry::new();
        registry
            .register_named(
                "session-start",
                "python3 -c 'import sys,json; d=json.load(sys.stdin); sys.exit(0 if \"SessionStarted\" in d else 9)'",
            )
            .expect("register");
        let session = SessionId::generate();
        let context = HookContext { event: Event::SessionStarted { session }, turn_count: 0 };
        registry.fire(HookPoint::SessionStart, &context).expect("fire");
    }
}
