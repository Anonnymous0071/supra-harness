//! The permission model: two axes that are routinely conflated.
//!
//! | Axis | Question | Enforced by | User-relaxable |
//! | ---- | -------- | ----------- | -------------- |
//! | [`ToolClass`] | *who* may invoke | absence of the WASM capability (T20) | **never** |
//! | [`Reversibility`] against [`Mode`] | does this need **user consent** | host-side gate (T16.7) | yes |
//!
//! Keeping them apart is what makes `yolo` safe to offer at all. `yolo` says "stop
//! asking me"; it does not say "let the model do things only the host may do".
//! [`decide`] composes both axes in that order, and a test walks every combination
//! of the two to show that no mode - `yolo` included - can turn a class refusal into
//! permission.
//!
//! # Reversibility, not danger
//!
//! "Danger level" cannot be computed. Reversibility can, and it is computed from the
//! **resolved effect** rather than the tool name: `shell_run("cargo test")` is
//! [`Reversibility::R0`] and `shell_run("rm -rf node_modules")` is
//! [`Reversibility::R3`], from the same tool. Classification itself is T16.7's job;
//! this module owns the classes, the mode matrix, and the damping rule.

use serde::{Deserialize, Serialize};

/// Who is asking to invoke a tool.
///
/// Ordered by authority, ascending, so `Ord` answers "does this caller have at
/// least that much authority".
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Invoker {
    /// A peer running inside a WASM component. Holds the least authority, and is
    /// the only invoker whose capabilities are enforced by *absence*: a tool it may
    /// not call has no import to call it through.
    Agent,
    /// The host process, acting on a peer's behalf after a gate decision.
    Host,
    /// The person at the terminal, acting directly.
    User,
}

/// The minimum authority a tool requires of its caller.
///
/// Deliberately the same ladder as [`Invoker`]: a tool classified `Host` cannot be
/// requested by an agent, but can be requested by the host or by the user.
///
/// # Never relaxable
///
/// No function in this module accepts both a `ToolClass` and a [`Mode`] in a way
/// that lets the mode widen the class. [`decide`] takes both but consults the class
/// first and returns [`Decision::Refuse`] before the mode is ever examined.
/// `scripts/check-invariants.sh` fails the build if a method on `ToolClass` grows a
/// `Mode` parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ToolClass {
    /// A peer may request it: reads, searches, edits within the workspace.
    Agent,
    /// Only the host may perform it: spawning processes, writing the journal,
    /// touching the keyring. T20 derives the agent's WASM import set from this, so
    /// a peer has no name to call.
    Host,
    /// Only the user may perform it: approving a finding, changing a mode,
    /// disabling the sandbox.
    User,
}

impl ToolClass {
    /// Whether `invoker` holds at least this much authority.
    #[must_use]
    pub const fn permits(self, invoker: Invoker) -> bool {
        self.rank() <= invoker.rank()
    }

    const fn rank(self) -> u8 {
        match self {
            Self::Agent => 0,
            Self::Host => 1,
            Self::User => 2,
        }
    }
}

impl Invoker {
    const fn rank(self) -> u8 {
        match self {
            Self::Agent => 0,
            Self::Host => 1,
            Self::User => 2,
        }
    }
}

/// How hard an effect is to undo.
///
/// Ascending severity, so `Ord` answers "is this at least as hard to undo as that".
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Reversibility {
    /// No side effect: a read, a search, a test run.
    R0,
    /// Automatically recoverable, because the file is git-tracked and clean or a
    /// T16.6 journal snapshot exists.
    R1,
    /// Recoverable with intervention.
    R2,
    /// Irreversible: `rm` outside the index, a force-push, a network POST, a
    /// `DROP TABLE`.
    R3,
}

impl Reversibility {
    /// Every class, ascending.
    pub const ALL: [Self; 4] = [Self::R0, Self::R1, Self::R2, Self::R3];

    /// One class less severe, saturating at [`Self::R0`].
    ///
    /// Verifiability damps risk, and this is where that is spent. `replace_node`
    /// (T15.7) passes a reparse gate with atomic rollback and preserves formatting,
    /// so it is *provably* safer than a blind string-matching `edit_file` on the
    /// same file - and the permission engine rates it one class lower. Verified
    /// structure earning greater trust is measurable rather than felt.
    #[must_use]
    pub const fn damped(self) -> Self {
        match self {
            Self::R0 | Self::R1 => Self::R0,
            Self::R2 => Self::R1,
            Self::R3 => Self::R2,
        }
    }
}

/// What the permission gate does with a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Decision {
    /// Proceed without asking.
    Run,
    /// Ask the user first.
    Prompt,
    /// Do not proceed, and do not ask.
    Refuse,
}

/// How much consent the user has pre-granted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Mode {
    /// Read-only. Anything with a side effect is refused rather than queued, so a
    /// plan is a plan and not a staging area.
    Plan,
    /// Ask about every side effect.
    Ask,
    /// Ask only about the irreversible.
    ///
    /// The default, and only defensible because T16.6 `supra_journal` exists.
    /// Without an undo stack this would be a hope rather than an engineering
    /// decision.
    #[default]
    Auto,
    /// Ask about nothing.
    ///
    /// Relaxes consent and nothing else: the guard layers, the sandbox, and the AST
    /// reparse gate are all still in force, and [`ToolClass`] is untouched.
    Yolo,
}

impl Mode {
    /// Every mode, from most to least restrictive.
    pub const ALL: [Self; 4] = [Self::Plan, Self::Ask, Self::Auto, Self::Yolo];

    /// Short name, as shown in the status line and accepted on the command line.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Ask => "ask",
            Self::Auto => "auto",
            Self::Yolo => "yolo",
        }
    }

    /// The consent decision for an effect of this reversibility.
    ///
    /// This is the matrix in section 6 of the architecture document, and a test in
    /// this module walks all sixteen cells against it.
    #[must_use]
    pub const fn decide(self, reversibility: Reversibility) -> Decision {
        // One arm per row of the table, rather than a flat match over pairs. The
        // shape is the documentation: each mode reads as a single rule plus its
        // exception, which is how the table reads too.
        match self {
            // Read-only work runs; everything else is refused rather than queued,
            // so a plan is a plan and not a staging area.
            Self::Plan => match reversibility {
                Reversibility::R0 => Decision::Run,
                Reversibility::R1 | Reversibility::R2 | Reversibility::R3 => Decision::Refuse,
            },
            // Every side effect is a question.
            Self::Ask => match reversibility {
                Reversibility::R0 => Decision::Run,
                Reversibility::R1 | Reversibility::R2 | Reversibility::R3 => Decision::Prompt,
            },
            // Only the irreversible is a question, which is defensible because
            // T16.6 can undo the rest.
            Self::Auto => match reversibility {
                Reversibility::R3 => Decision::Prompt,
                Reversibility::R0 | Reversibility::R1 | Reversibility::R2 => Decision::Run,
            },
            // Nothing is a question. Consent only: authority is untouched.
            Self::Yolo => Decision::Run,
        }
    }
}

/// The complete gate: authority first, then consent.
///
/// Order matters and is the whole point. A [`ToolClass`] refusal is returned before
/// `mode` is examined, so no mode can widen it.
#[must_use]
pub const fn decide(
    class: ToolClass,
    invoker: Invoker,
    mode: Mode,
    reversibility: Reversibility,
) -> Decision {
    if !class.permits(invoker) {
        return Decision::Refuse;
    }
    mode.decide(reversibility)
}

/// Where a permission rule came from.
///
/// Ordered by precedence, ascending, so `Ord` picks the winner among competing
/// allows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum RuleSource {
    /// Shipped with the binary.
    Builtin,
    /// The user's global configuration.
    User,
    /// The project's checked-in configuration.
    Project,
    /// A command-line flag for this invocation.
    Cli,
    /// Set during this session, for instance by a slash command.
    Session,
}

impl core::fmt::Display for RuleSource {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Lowercase words, the shape a refusal message or a status line
        // shows: "a rule from builtin denies this request" reads as English
        // without a lookup table in every renderer.
        f.write_str(match self {
            Self::Builtin => "builtin",
            Self::User => "user",
            Self::Project => "project",
            Self::Cli => "cli",
            Self::Session => "session",
        })
    }
}

/// What a rule says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RuleEffect {
    /// Permitted.
    Allow,
    /// Forbidden.
    Deny,
}

/// One resolved permission rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rule {
    /// Where it came from.
    pub source: RuleSource,
    /// What it says.
    pub effect: RuleEffect,
}

/// Combine competing rules.
///
/// Deny wins outright, then the highest-precedence allow, then `None` for "no rule
/// applies" - which leaves the reversibility gate to decide.
///
/// # "Deny always winning" is literal
///
/// **Any** deny beats **every** allow, whatever their sources, so precedence orders
/// allows only. Three things settle this reading rather than leaving it open:
///
/// 1. **The project already has this shape elsewhere.** Guard layers L1 to L7 have
///    no off switch, and disabling the sandbox is not a permission rule at all - it
///    is a separate `--sandbox off` flag with its own confirmation and a persistent
///    status-line warning. Escape hatches here are explicit and ceremonial, never a
///    higher-precedence allow quietly winning.
/// 2. **The alternative inverts the trust boundary.** If deny only won among rules
///    of equal precedence, a `Session` allow - something a slash command can set
///    mid-conversation - would override a `Builtin` prohibition. A prohibition that
///    a session can lift is not a prohibition.
/// 3. **It is what security policy engines do.** An explicit deny winning
///    unconditionally is the standard evaluation rule, and matching it means an
///    operator's intuition transfers.
///
/// # What this obliges of rule authors
///
/// Because a deny is absolute, `Deny` is the wrong tool for "usually not". The
/// correct expression of a default is **no rule at all**, which falls through to
/// [`Mode::decide`] and asks the user. `RuleSource::Builtin` must therefore emit
/// `Deny` only for effects that must never be permitted under any mode by any user;
/// everything else it has an opinion about belongs in the reversibility
/// classification, not here. T16.7 owns that catalogue.
#[must_use]
pub fn resolve(rules: &[Rule]) -> Option<RuleEffect> {
    if rules.iter().any(|rule| rule.effect == RuleEffect::Deny) {
        return Some(RuleEffect::Deny);
    }
    rules
        .iter()
        .filter(|rule| rule.effect == RuleEffect::Allow)
        .max_by_key(|rule| rule.source)
        .map(|rule| rule.effect)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mode_matrix_matches_the_architecture_document() {
        use Decision::{Prompt, Refuse, Run};
        use Reversibility::{R0, R1, R2, R3};

        // docs/ARCHITECTURE.md section 6, one row per mode. Written out in full
        // rather than generated, so a change to either has to be made in both.
        let table = [
            (Mode::Plan, [Run, Refuse, Refuse, Refuse]),
            (Mode::Ask, [Run, Prompt, Prompt, Prompt]),
            (Mode::Auto, [Run, Run, Run, Prompt]),
            (Mode::Yolo, [Run, Run, Run, Run]),
        ];

        for (mode, expected) in table {
            for (class, want) in [R0, R1, R2, R3].into_iter().zip(expected) {
                assert_eq!(mode.decide(class), want, "{mode:?} / {class:?}");
            }
        }
    }

    #[test]
    fn auto_is_the_default() {
        // Because T16.6 exists. If the journal were ever dropped, this default
        // would have to change with it.
        assert_eq!(Mode::default(), Mode::Auto);
    }

    #[test]
    fn read_only_work_never_prompts_in_any_mode() {
        for mode in Mode::ALL {
            assert_eq!(mode.decide(Reversibility::R0), Decision::Run, "{mode:?}");
        }
    }

    #[test]
    fn plan_refuses_rather_than_queueing() {
        for class in [Reversibility::R1, Reversibility::R2, Reversibility::R3] {
            assert_eq!(Mode::Plan.decide(class), Decision::Refuse);
        }
    }

    #[test]
    fn yolo_relaxes_consent_and_nothing_else() {
        // The property that makes yolo offerable: it collapses the consent axis
        // completely, and has no effect at all on the authority axis.
        for reversibility in Reversibility::ALL {
            assert_eq!(Mode::Yolo.decide(reversibility), Decision::Run);
        }

        for class in [ToolClass::Host, ToolClass::User] {
            assert_eq!(
                decide(class, Invoker::Agent, Mode::Yolo, Reversibility::R0),
                Decision::Refuse,
                "yolo must not let an agent reach a {class:?} tool"
            );
        }
    }

    #[test]
    fn no_mode_can_widen_the_authority_axis() {
        // Exhaustive over both axes. This is the test that keeps the two from being
        // conflated again later: whenever the class refuses the invoker, the answer
        // is Refuse regardless of mode or reversibility.
        for class in [ToolClass::Agent, ToolClass::Host, ToolClass::User] {
            for invoker in [Invoker::Agent, Invoker::Host, Invoker::User] {
                for mode in Mode::ALL {
                    for reversibility in Reversibility::ALL {
                        let decision = decide(class, invoker, mode, reversibility);
                        if class.permits(invoker) {
                            assert_eq!(decision, mode.decide(reversibility));
                        } else {
                            assert_eq!(
                                decision,
                                Decision::Refuse,
                                "{class:?} / {invoker:?} / {mode:?} / {reversibility:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_authority_ladder_is_the_documented_one() {
        assert!(ToolClass::Agent.permits(Invoker::Agent));
        assert!(ToolClass::Agent.permits(Invoker::Host));
        assert!(ToolClass::Agent.permits(Invoker::User));

        assert!(!ToolClass::Host.permits(Invoker::Agent));
        assert!(ToolClass::Host.permits(Invoker::Host));
        assert!(ToolClass::Host.permits(Invoker::User));

        assert!(!ToolClass::User.permits(Invoker::Agent));
        assert!(!ToolClass::User.permits(Invoker::Host));
        assert!(ToolClass::User.permits(Invoker::User));
    }

    #[test]
    fn damping_moves_exactly_one_class_and_saturates() {
        assert_eq!(Reversibility::R3.damped(), Reversibility::R2);
        assert_eq!(Reversibility::R2.damped(), Reversibility::R1);
        assert_eq!(Reversibility::R1.damped(), Reversibility::R0);
        assert_eq!(Reversibility::R0.damped(), Reversibility::R0);

        // Damping may soften a decision but must never harden one.
        for mode in Mode::ALL {
            for reversibility in Reversibility::ALL {
                let plain = mode.decide(reversibility);
                let damped = mode.decide(reversibility.damped());
                let rank = |decision| match decision {
                    Decision::Run => 0_u8,
                    Decision::Prompt => 1,
                    Decision::Refuse => 2,
                };
                assert!(rank(damped) <= rank(plain), "{mode:?} / {reversibility:?}");
            }
        }
    }

    #[test]
    fn damping_is_what_makes_a_verified_edit_cheaper_to_authorise() {
        // The concrete case from the architecture document: the same file, two
        // tools. A blind edit is R2 and prompts under `auto` after damping is
        // withheld; a reparse-gated splice damps to R1 and runs.
        assert_eq!(Mode::Ask.decide(Reversibility::R2), Decision::Prompt);
        assert_eq!(Mode::Auto.decide(Reversibility::R3), Decision::Prompt);
        assert_eq!(Mode::Auto.decide(Reversibility::R3.damped()), Decision::Run);
    }

    #[test]
    fn severity_and_authority_both_order_ascending() {
        assert!(Reversibility::R0 < Reversibility::R3);
        assert!(Invoker::Agent < Invoker::User);
        assert!(RuleSource::Builtin < RuleSource::Session);
    }

    #[test]
    fn deny_beats_every_allow() {
        let allow_session = Rule { source: RuleSource::Session, effect: RuleEffect::Allow };
        let deny_builtin = Rule { source: RuleSource::Builtin, effect: RuleEffect::Deny };
        assert_eq!(resolve(&[allow_session, deny_builtin]), Some(RuleEffect::Deny));
        assert_eq!(resolve(&[deny_builtin, allow_session]), Some(RuleEffect::Deny));
    }

    #[test]
    fn a_deny_from_the_weakest_source_survives_every_stronger_allow() {
        // The decision spelled out at its most uncomfortable: a builtin deny beats
        // an allow from every other source at once, including a session one the user
        // set deliberately. That is the point - a prohibition a slash command can
        // lift is not a prohibition, and the escape hatches in this design are
        // separate confirmed flags rather than higher-precedence allows.
        let deny = Rule { source: RuleSource::Builtin, effect: RuleEffect::Deny };
        let allows = [
            Rule { source: RuleSource::User, effect: RuleEffect::Allow },
            Rule { source: RuleSource::Project, effect: RuleEffect::Allow },
            Rule { source: RuleSource::Cli, effect: RuleEffect::Allow },
            Rule { source: RuleSource::Session, effect: RuleEffect::Allow },
        ];

        let mut rules = allows.to_vec();
        rules.push(deny);
        assert_eq!(resolve(&rules), Some(RuleEffect::Deny));

        // And order of evaluation cannot change it, which is what "unconditionally"
        // has to mean if an operator is to reason about a rule set at all.
        rules.reverse();
        assert_eq!(resolve(&rules), Some(RuleEffect::Deny));
    }

    #[test]
    fn a_default_is_the_absence_of_a_rule_not_a_deny() {
        // The obligation the literal reading places on rule authors. "Usually not"
        // must be expressed as no rule, so the consent gate can still ask; writing
        // it as a deny would make it unaskable.
        assert_eq!(resolve(&[]), None);
        assert_eq!(Mode::Ask.decide(Reversibility::R2), Decision::Prompt);
        assert_eq!(Mode::Auto.decide(Reversibility::R3), Decision::Prompt);
    }

    #[test]
    fn precedence_orders_allows_only() {
        let builtin = Rule { source: RuleSource::Builtin, effect: RuleEffect::Allow };
        let session = Rule { source: RuleSource::Session, effect: RuleEffect::Allow };
        assert_eq!(resolve(&[builtin, session]), Some(RuleEffect::Allow));
        assert_eq!(resolve(&[]), None, "no rule means the reversibility gate decides");
    }

    #[test]
    fn mode_labels_are_the_command_line_spellings() {
        let labels: Vec<&str> = Mode::ALL.iter().map(|mode| mode.label()).collect();
        assert_eq!(labels, vec!["plan", "ask", "auto", "yolo"]);
    }
}
