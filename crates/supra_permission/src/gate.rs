//! The host-side permission gate.
//!
//! The architecture's two-axis table says what this is: authority
//! (`ToolClass`, never relaxable) and consent ([`Mode`], user-relaxable)
//! composed in that order. T6 owns the axes and the matrix; T16.7 owns the
//! composition a tool call actually passes through:
//!
//! 1. **Rules first.** [`supra_types::resolve`] over the caller's rule set:
//!    any deny refuses outright (deny always wins, literally), an allow
//!    short-circuits consent, no rule falls through to the matrix.
//! 2. **The matrix.** [`supra_types::decide`] consults the class before the
//!    mode, so no mode - `yolo` included - can widen authority. The
//!    reversibility comes from the catalogue ([`crate::catalogue`]), with
//!    damping applied exactly when the effect is the verified structural
//!    shape.
//! 3. **The escape-hatch exception.** An effect that asks a guard to step
//!    aside is a question in *every* mode, including `yolo`. Consent is
//!    what `yolo` pre-grants; stepping aside from the sandbox is not a
//!    reversibility question at all but a ceremony the architecture
//!    assigns to `--sandbox off` - so the gate routes it to [`Outcome::Ask`]
//!    no matter what the matrix alone would say.
//!
//! # Batching
//!
//! The gate is evaluated per request, but questions are *rendered* in
//! batches: one prompt can carry many pending questions, and the answers
//! come back per item. [`Batch`] is that shape - collect the asks, ask
//! once, resolve each request by its item's answer. A batch with no
//! questions produces no prompt at all, and an empty batch is refused as
//! [`PermissionError::EmptyBatch`] rather than answered vacuously.

use supra_types::{Decision, Invoker, Mode, Rule, ToolClass, decide, resolve};

use crate::catalogue::Effect;
use crate::error::PermissionError;

/// One permission request: everything the gate needs, and nothing else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The effect the tool is about to have, resolved by the caller into a
    /// catalogue shape.
    pub effect: Effect,
    /// The minimum authority the tool requires.
    pub class: ToolClass,
    /// Who is invoking.
    pub invoker: Invoker,
    /// A short, human-readable summary for the prompt - typically the tool
    /// name and its primary argument.
    pub summary: String,
}

/// What the gate says about one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Proceed; no question needed.
    Run,
    /// Ask the user; the request proceeds only on a per-item yes.
    Ask {
        /// The request, as the prompt should show it.
        request: Request,
        /// Why the gate is asking rather than running - rendered by the TUI
        /// as the reason line under the question.
        reason: AskReason,
    },
    /// Do not proceed, and do not ask.
    Refuse {
        /// Why, for the log line the refusal becomes.
        reason: RefuseReason,
    },
}

/// Why the gate is asking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AskReason {
    /// The mode matrix says this reversibility prompts in this mode.
    Matrix,
    /// The effect asks a guard to step aside; every mode asks.
    EscapeHatch,
}

/// Why the gate refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefuseReason {
    /// A rule denies the request. Deny is absolute.
    Rule {
        /// Where the denying rule came from.
        source: supra_types::RuleSource,
    },
    /// The authority axis refused: this invoker may not use this class of
    /// tool. No mode can widen this.
    Authority,
    /// The mode is `plan` and the effect has a side effect; a plan refuses
    /// rather than queues.
    Plan,
    /// The user did not answer this item, or answered no. Unanswered
    /// refuses rather than runs: a question the user did not answer is not
    /// consent.
    NotAnswered {
        /// The request's summary, for the refusal's log line.
        summary: String,
    },
}

/// The complete gate: rules, then authority, then consent.
///
/// `rules` are the caller's resolved rules for this invocation (T16.8/T30
/// own loading and precedence assembly; the gate only evaluates what it is
/// handed, so the same gate serves every source set).
#[must_use]
pub fn gate(request: &Request, mode: Mode, rules: &[Rule]) -> Outcome {
    // Rules first: deny always wins, an allow skips consent, no rule falls
    // through. T6's `resolve` implements the literal reading; the gate adds
    // only the outcome shapes.
    if let Some(effect) = resolve(rules) {
        return match effect {
            supra_types::RuleEffect::Deny => {
                let source = rules
                    .iter()
                    .find(|rule| rule.effect == supra_types::RuleEffect::Deny)
                    .map_or(supra_types::RuleSource::Builtin, |rule| rule.source);
                Outcome::Refuse { reason: RefuseReason::Rule { source } }
            }
            supra_types::RuleEffect::Allow => Outcome::Run,
        };
    }

    // Authority before consent, and no damping touches this axis: the class
    // is consulted first and a refusal returns before the mode is examined,
    // exactly as `decide` does.
    if !request.class.permits(request.invoker) {
        return Outcome::Refuse { reason: RefuseReason::Authority };
    }

    // The escape-hatch exception: consent is what a mode pre-grants, and
    // stepping aside from a guard is not consent's to grant. Every mode
    // asks, including `yolo`.
    if matches!(request.effect, Effect::EscapeHatch { .. }) {
        return Outcome::Ask { request: clone_request(request), reason: AskReason::EscapeHatch };
    }

    // The matrix, with damping applied exactly when the effect is the
    // verified structural shape. Blind edits and unverified shapes get
    // their plain class - verifiability is what earns the lower rate, and
    // nothing else does.
    let reversibility = if request.effect.is_structural() {
        request.effect.reversibility().damped()
    } else {
        request.effect.reversibility()
    };

    match decide(request.class, request.invoker, mode, reversibility) {
        Decision::Run => Outcome::Run,
        Decision::Prompt => Outcome::Ask { request: clone_request(request), reason: AskReason::Matrix },
        Decision::Refuse => Outcome::Refuse { reason: RefuseReason::Plan },
    }
}

fn clone_request(request: &Request) -> Request {
    Request {
        effect: request.effect.clone(),
        class: request.class,
        invoker: request.invoker,
        summary: request.summary.clone(),
    }
}

/// A batch of pending questions: one prompt, per-item answers.
///
/// Built from a set of requests evaluated against one mode and rule set.
/// `gate` each request first; the ones that come back [`Outcome::Ask`] go
/// into the batch, and [`Batch::resolve`] turns the user's per-item answers
/// into final outcomes. An answer of "yes" runs; anything else - "no", an
/// unanswered item, a dismissed prompt - refuses. Unanswered refuses
/// rather than runs: a question the user did not answer is not consent.
pub struct Batch {
    items: Vec<Request>,
}

impl Batch {
    /// Collect the asks from a set of gate outcomes.
    ///
    /// # Errors
    ///
    /// [`PermissionError::EmptyBatch`] when there is nothing to ask - an
    /// empty batch that answered would let a caller report "the user
    /// approved nothing" as a decision, when no question was ever asked.
    pub fn collect(outcomes: &[Outcome]) -> Result<Self, PermissionError> {
        let items: Vec<Request> = outcomes
            .iter()
            .filter_map(|outcome| match outcome {
                Outcome::Ask { request, .. } => Some(clone_request(request)),
                _ => None,
            })
            .collect();
        if items.is_empty() {
            return Err(PermissionError::EmptyBatch);
        }
        Ok(Self { items })
    }

    /// The items, in prompt order.
    #[must_use]
    pub fn items(&self) -> &[Request] {
        &self.items
    }

    /// Turn per-item answers into final outcomes.
    ///
    /// `answers` is indexed by item position; a missing answer or a `false`
    /// both refuse. The returned vector aligns with the batch's items, so
    /// the caller pairs them by index without a second lookup.
    #[must_use]
    pub fn resolve(&self, answers: &[bool]) -> Vec<Outcome> {
        self.items
            .iter()
            .zip(answers.iter().chain(std::iter::repeat(&false)))
            .map(|(request, answer)| {
                if *answer {
                    Outcome::Run
                } else {
                    Outcome::Refuse { reason: RefuseReason::NotAnswered { summary: request.summary.clone() } }
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use supra_types::{Rule, RuleEffect, RuleSource};

    fn request(effect: Effect) -> Request {
        Request { effect, class: ToolClass::Agent, invoker: Invoker::Host, summary: "the tool".to_owned() }
    }

    #[test]
    fn rules_come_first_and_deny_wins() {
        let read = request(Effect::Read);
        let deny = Rule { source: RuleSource::Builtin, effect: RuleEffect::Deny };

        // A deny refuses even in yolo, even for a read.
        let outcome = gate(&read, Mode::Yolo, &[deny]);
        assert!(matches!(
            outcome,
            Outcome::Refuse { reason: RefuseReason::Rule { source: RuleSource::Builtin } }
        ));

        // An allow skips consent: a network POST runs in plan mode, because
        // the rule said so and rules outrank modes.
        let allow = Rule { source: RuleSource::Session, effect: RuleEffect::Allow };
        let post = request(Effect::NetworkPost { target: "api".to_owned() });
        assert_eq!(gate(&post, Mode::Plan, &[allow]), Outcome::Run);

        // No rule: the matrix decides.
        assert_eq!(gate(&post, Mode::Plan, &[]), Outcome::Refuse { reason: RefuseReason::Plan });
    }

    #[test]
    fn authority_refuses_before_the_mode_is_examined() {
        // The two-axis property, at the gate level: a Host-class tool
        // requested by an Agent refuses in yolo with an authority reason,
        // not a mode reason.
        let request = Request {
            effect: Effect::Read,
            class: ToolClass::Host,
            invoker: Invoker::Agent,
            summary: "spawn".to_owned(),
        };
        for mode in Mode::ALL {
            assert_eq!(
                gate(&request, mode, &[]),
                Outcome::Refuse { reason: RefuseReason::Authority },
                "{mode:?}"
            );
        }
    }

    #[test]
    fn plan_refuses_rather_than_queues() {
        let edit = request(Effect::BlindEdit);
        assert_eq!(gate(&edit, Mode::Plan, &[]), Outcome::Refuse { reason: RefuseReason::Plan });

        let remove = request(Effect::Remove { target: "x".to_owned() });
        assert_eq!(gate(&remove, Mode::Plan, &[]), Outcome::Refuse { reason: RefuseReason::Plan });
    }

    #[test]
    fn the_mode_matrix_through_the_gate() {
        // Ask/auto on the classes the architecture names: under auto, R3
        // prompts and R2 runs; under ask, both prompt.
        let remove = request(Effect::Remove { target: "x".to_owned() });
        let blind = request(Effect::BlindEdit);

        assert!(matches!(gate(&remove, Mode::Auto, &[]), Outcome::Ask { reason: AskReason::Matrix, .. }));
        assert_eq!(gate(&blind, Mode::Auto, &[]), Outcome::Run);
        assert!(matches!(gate(&remove, Mode::Ask, &[]), Outcome::Ask { .. }));
        assert!(matches!(gate(&blind, Mode::Ask, &[]), Outcome::Ask { .. }));
    }

    #[test]
    fn yolo_still_asks_for_an_escape_hatch() {
        // The exception the architecture's shape demands: `yolo` pre-grants
        // consent, and stepping aside from a guard is not consent's to
        // grant. Every mode asks.
        let hatch = request(Effect::EscapeHatch { what: "sandbox".to_owned() });
        for mode in Mode::ALL {
            assert!(
                matches!(gate(&hatch, mode, &[]), Outcome::Ask { reason: AskReason::EscapeHatch, .. }),
                "{mode:?} must ask for an escape hatch"
            );
        }
    }

    #[test]
    fn a_rule_deny_beats_an_escape_hatch_allow() {
        // The uncomfortable corner, stated: a session allow cannot make a
        // sandbox-off effect run unasked, because the escape-hatch routing
        // is checked after rules only for asks - and a deny anywhere still
        // wins over everything.
        let hatch = request(Effect::EscapeHatch { what: "sandbox".to_owned() });
        let deny = Rule { source: RuleSource::Builtin, effect: RuleEffect::Deny };
        assert!(matches!(gate(&hatch, Mode::Yolo, &[deny]), Outcome::Refuse { .. }));

        // And an allow short-circuits before the escape-hatch check - which
        // is the rule set's prerogative, and the reason `builtin` is
        // obliged to deny exactly the never-permittable and nothing else.
        let allow = Rule { source: RuleSource::Session, effect: RuleEffect::Allow };
        assert_eq!(gate(&hatch, Mode::Yolo, &[allow]), Outcome::Run);
    }

    #[test]
    fn the_structural_shape_runs_where_the_blind_one_prompts() {
        // The measurable version of "verifiability damps risk": under ask,
        // a blind edit prompts and a reparse-gated splice on the same file
        // runs, because the catalogue routed the structural shape through
        // damping and the blind shape around it.
        let structural = request(Effect::StructuralEdit { basis: crate::catalogue::RecoveryBasis::GitClean });
        let blind = request(Effect::BlindEdit);

        assert_eq!(gate(&structural, Mode::Ask, &[]), Outcome::Run);
        assert!(matches!(gate(&blind, Mode::Ask, &[]), Outcome::Ask { .. }));
    }

    #[test]
    fn a_batch_collects_only_the_asks() {
        let outcomes = [
            Outcome::Run,
            Outcome::Ask { request: request(Effect::BlindEdit), reason: AskReason::Matrix },
            Outcome::Refuse { reason: RefuseReason::Plan },
            Outcome::Ask {
                request: request(Effect::Remove { target: "x".to_owned() }),
                reason: AskReason::Matrix,
            },
        ];
        let batch = Batch::collect(&outcomes).expect("batch");
        assert_eq!(batch.items().len(), 2, "only the asks are in the batch");
        assert_eq!(batch.items()[0].summary, "the tool");
    }

    #[test]
    fn an_empty_batch_is_refused_not_answered() {
        assert!(matches!(Batch::collect(&[]), Err(PermissionError::EmptyBatch)));
        assert!(matches!(
            Batch::collect(&[Outcome::Run, Outcome::Refuse { reason: RefuseReason::Plan }]),
            Err(PermissionError::EmptyBatch)
        ));
    }

    #[test]
    fn the_full_matrix_through_the_gate_sixteen_cells() {
        // T6's matrix test walks mode x reversibility; this is the same
        // walk through the gate, so the composition cannot drift from the
        // table it composes. Four representative shapes, one per class -
        // the catalogue test already pins every shape's class, so the
        // cells here are about the gate's routing, not re-derivation.
        use supra_types::Decision;

        let shapes = [
            (Effect::Read, supra_types::Reversibility::R0),
            (
                Effect::RecoverableEdit { basis: crate::catalogue::RecoveryBasis::GitClean },
                supra_types::Reversibility::R1,
            ),
            (Effect::BlindEdit, supra_types::Reversibility::R2),
            (Effect::Remove { target: "x".to_owned() }, supra_types::Reversibility::R3),
        ];

        for mode in Mode::ALL {
            for (effect, reversibility) in &shapes {
                let outcome = gate(&request(effect.clone()), mode, &[]);
                let want = mode.decide(*reversibility);
                match (outcome, want) {
                    (Outcome::Run, Decision::Run)
                    | (Outcome::Ask { .. }, Decision::Prompt)
                    | (Outcome::Refuse { .. }, Decision::Refuse) => {}
                    (got, expected) => {
                        panic!("{mode:?} / {effect:?}: gate said {got:?}, matrix says {expected:?}")
                    }
                }
            }
        }
    }

    #[test]
    fn batch_answers_align_by_index_and_missing_answers_refuse() {
        let outcomes = [
            Outcome::Ask { request: request(Effect::BlindEdit), reason: AskReason::Matrix },
            Outcome::Ask {
                request: request(Effect::Remove { target: "x".to_owned() }),
                reason: AskReason::Matrix,
            },
        ];
        let batch = Batch::collect(&outcomes).expect("batch");

        // Both answered yes: both run.
        assert_eq!(batch.resolve(&[true, true]), vec![Outcome::Run, Outcome::Run]);

        // One yes, one no: aligned by index.
        let resolved = batch.resolve(&[true, false]);
        assert_eq!(resolved[0], Outcome::Run);
        assert!(matches!(resolved[1], Outcome::Refuse { .. }));

        // No answers at all: unanswered refuses rather than runs - a
        // question the user did not answer is not consent.
        let resolved = batch.resolve(&[]);
        assert!(matches!(resolved[0], Outcome::Refuse { .. }));
        assert!(matches!(resolved[1], Outcome::Refuse { .. }));
    }
}
