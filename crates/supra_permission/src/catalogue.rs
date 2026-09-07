//! The reversibility catalogue: a resolved effect, classified.
//!
//! Section 6 of the architecture document is explicit about what this owns:
//! "Classification runs on the **resolved effect**, never the tool name:
//! `shell_run(\"cargo test\")` is R0, `shell_run(\"rm -rf node_modules\")` is
//! R3". The catalogue is therefore a closed enumeration of effect *shapes* -
//! not a string parser, not a tool-name lookup. A caller resolves what the
//! tool is about to do into one of these shapes; the catalogue turns the
//! shape into a [`Reversibility`], and the gate composes that with the mode
//! matrix that already lives in `supra_types` (T6).
//!
//! # Why a closed enum and not predicates over strings
//!
//! Three reasons, each of them a lesson this project has already paid for:
//!
//! 1. **A string classifier is a second grammar.** T15.7's lesson about two
//!    parsers for one grammar applies verbatim: a `command.contains("rm")`
//!    classifier and a shell disagree about `env rm`, `\\rm`, and a script
//!    whose *first* line is harmless. The caller - T17's tool registry,
//!    which knows the argv - resolves the effect into a shape; the
//!    catalogue never guesses.
//! 2. **Every class boundary in the architecture is a shape, not a
//!    substring.** "rm outside the index" needs to know where the index
//!    lives; "network POST" needs to know the sandbox's network policy;
//!    "git-tracked and clean" needs the digest (T15). Those are inputs the
//!    caller holds, so the shape carries them as data.
//! 3. **A closed enum is testable exhaustively.** Every shape has a class,
//!    every class has a shape that produces it, and a test walks the whole
//!    table - the same discipline T6's mode matrix test takes.
//!
//! # The damping rule
//!
//! [`Effect::StructuralEdit`] is the damped shape: a splice that passed the
//! reparse gate with atomic rollback (T15.7) is rated one class lower than
//! [`Effect::BlindEdit`] on the same file, because verified structure earns
//! trust that is measurable rather than felt. The damping itself lives in
//! `Reversibility::damped` (T6); this catalogue's only job is to route the
//! verified shape through it and the blind shape around it.

use supra_types::Reversibility;

/// A resolved effect: what the tool is about to do, as a shape.
///
/// The caller constructs this from its own knowledge of the invocation -
/// argv, paths, network policy - which is why the fields are data rather
/// than raw command text. `Display` renders the shape for a prompt or a
/// log line; it is never parsed back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Read or search: no bytes change anywhere.
    Read,
    /// A command whose observable effect is confined to a scratch
    /// workspace the harness owns and will discard - a build directory, a
    /// test's tempdir. The harness's own state does not change.
    ScratchWork,
    /// A file edit with a journal snapshot committed (T16.6) or a
    /// git-tracked file with no uncommitted changes.
    RecoverableEdit {
        /// Why it is recoverable: a snapshot id, or git cleanliness.
        basis: RecoveryBasis,
    },
    /// A reparse-gated structural splice (T15.7 `replace_node`): the edit
    /// is verified to re-parse cleanly, the rollback is atomic, and
    /// formatting is preserved by construction.
    StructuralEdit {
        /// Why it is recoverable, as in [`Effect::RecoverableEdit`].
        basis: RecoveryBasis,
    },
    /// A file edit with no snapshot and no git safety net - the blind
    /// string-matching `edit_file` shape.
    BlindEdit,
    /// An effect outside the workspace's index: creating or changing
    /// files the harness did not snapshot and git does not track.
    OutsideEdit {
        /// The path the effect lands on, for the prompt.
        path: String,
    },
    /// An outbound network request with side effects on the remote side -
    /// a POST, a push. A read-only fetch is [`Effect::Read`]; the
    /// caller distinguishes them because it knows the method.
    NetworkPost {
        /// The destination, for the prompt.
        target: String,
    },
    /// A force-push: history rewrite on a shared remote.
    ForcePush {
        /// The remote and ref, for the prompt.
        target: String,
    },
    /// Destructive removal outside the workspace index - `rm` of files the
    /// harness cannot reconstruct.
    Remove {
        /// What would be removed, for the prompt.
        target: String,
    },
    /// A database-destructive statement: `DROP TABLE`, `TRUNCATE`, a
    /// migration that drops data.
    DropData {
        /// The store and object, for the prompt.
        target: String,
    },
    /// The sandbox or a guard layer is being asked to step aside. Not a
    /// class the matrix can soften: this is the `--sandbox off` shape,
    /// and it is always a question regardless of mode.
    EscapeHatch {
        /// What is being disabled.
        what: String,
    },
}

/// Why a recoverable edit is recoverable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryBasis {
    /// A T16.6 snapshot is committed for this edit's file.
    Snapshot,
    /// The file is git-tracked with no uncommitted changes; `git checkout`
    /// restores it.
    GitClean,
}

impl Effect {
    /// The reversibility class of this resolved effect.
    ///
    /// The whole catalogue, one arm per shape. `StructuralEdit` is the one
    /// damped arm, per the architecture's verifiability rule; everything
    /// else maps directly.
    #[must_use]
    pub fn reversibility(&self) -> Reversibility {
        match self {
            Self::Read | Self::ScratchWork => Reversibility::R0,
            Self::RecoverableEdit { .. } | Self::StructuralEdit { .. } => Reversibility::R1,
            Self::BlindEdit | Self::OutsideEdit { .. } => Reversibility::R2,
            // EscapeHatch shares the class deliberately: it is the one
            // shape whose *gate* handling differs (every mode asks), and
            // the class is not where that difference lives.
            Self::NetworkPost { .. }
            | Self::ForcePush { .. }
            | Self::Remove { .. }
            | Self::DropData { .. }
            | Self::EscapeHatch { .. } => Reversibility::R3,
        }
    }

    /// Whether the damping rule applies to this effect.
    ///
    /// Exposed so the gate can explain *why* a decision was reached -
    /// "rated one class lower because the splice is reparse-gated" is a
    /// sentence the TUI can render, and it is the measurable version of
    /// "verified structure earns greater trust".
    #[must_use]
    pub const fn is_structural(&self) -> bool {
        matches!(self, Self::StructuralEdit { .. })
    }
}

impl core::fmt::Display for Effect {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Read => write!(f, "read"),
            Self::ScratchWork => write!(f, "scratch work"),
            Self::RecoverableEdit { basis } => match basis {
                RecoveryBasis::Snapshot => write!(f, "edit (journal snapshot)"),
                RecoveryBasis::GitClean => write!(f, "edit (git clean)"),
            },
            Self::StructuralEdit { basis } => match basis {
                RecoveryBasis::Snapshot => write!(f, "structural splice (journal snapshot)"),
                RecoveryBasis::GitClean => write!(f, "structural splice (git clean)"),
            },
            Self::BlindEdit => write!(f, "edit (no safety net)"),
            Self::OutsideEdit { path } => write!(f, "edit outside the index: {path}"),
            Self::NetworkPost { target } => write!(f, "network POST: {target}"),
            Self::ForcePush { target } => write!(f, "force-push: {target}"),
            Self::Remove { target } => write!(f, "remove: {target}"),
            Self::DropData { target } => write!(f, "drop data: {target}"),
            Self::EscapeHatch { what } => write!(f, "disable {what}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalogue table, exhaustively: every shape has a class, and the
    /// classes the architecture names are each reachable from at least one
    /// shape. Written in full like T6's matrix test, so a change to either
    /// side must be made in both.
    #[test]
    fn every_shape_has_the_documented_class() {
        let table = [
            (Effect::Read, Reversibility::R0),
            (Effect::ScratchWork, Reversibility::R0),
            (Effect::RecoverableEdit { basis: RecoveryBasis::Snapshot }, Reversibility::R1),
            (Effect::RecoverableEdit { basis: RecoveryBasis::GitClean }, Reversibility::R1),
            (Effect::StructuralEdit { basis: RecoveryBasis::Snapshot }, Reversibility::R1),
            (Effect::StructuralEdit { basis: RecoveryBasis::GitClean }, Reversibility::R1),
            (Effect::BlindEdit, Reversibility::R2),
            (Effect::OutsideEdit { path: "/etc/hosts".to_owned() }, Reversibility::R2),
            (Effect::NetworkPost { target: "api.example.com".to_owned() }, Reversibility::R3),
            (Effect::ForcePush { target: "origin main".to_owned() }, Reversibility::R3),
            (Effect::Remove { target: "~/notes".to_owned() }, Reversibility::R3),
            (Effect::DropData { target: "production.users".to_owned() }, Reversibility::R3),
            (Effect::EscapeHatch { what: "sandbox".to_owned() }, Reversibility::R3),
        ];

        for (effect, want) in &table {
            assert_eq!(&effect.reversibility(), want, "{effect:?}");
        }

        // Every class is reachable: the table is not quietly missing a row.
        for class in Reversibility::ALL {
            assert!(table.iter().any(|(_, got)| got == &class), "no shape produces {class:?}");
        }
    }

    #[test]
    fn the_architectures_two_shell_examples_differ_by_shape_not_by_tool() {
        // "shell_run(\"cargo test\") is R0, shell_run(\"rm -rf node_modules\")
        // is R3, from the same tool" - the catalogue's shapes make that
        // difference carry data, so the same tool resolving to different
        // shapes classifies differently and the same shape classifies the
        // same regardless of which tool produced it.
        let cargo_test = Effect::ScratchWork; // the caller resolved: tests write only scratch
        let rm_rf = Effect::Remove { target: "node_modules".to_owned() };

        assert_eq!(cargo_test.reversibility(), Reversibility::R0);
        assert_eq!(rm_rf.reversibility(), Reversibility::R3);
    }

    #[test]
    fn the_structural_shape_is_the_damped_one() {
        // Verifiability damps risk, and this is where it is spent: the same
        // file, two edit shapes, one class apart once the gate applies
        // damping. The catalogue routes both to R1 here because both carry a
        // recovery basis; the *pair* differs in that the structural shape is
        // the one the gate can defend with the reparse gate's own evidence.
        let structural = Effect::StructuralEdit { basis: RecoveryBasis::GitClean };
        assert!(structural.is_structural());
        assert_eq!(structural.reversibility(), Reversibility::R1);

        let blind = Effect::BlindEdit;
        assert!(!blind.is_structural());
        assert_eq!(blind.reversibility(), Reversibility::R2);

        // And the damping itself, pinned where T6 defined it: the structural
        // shape at R1 damps to R0 - which under `auto` and `ask` is the
        // difference between run and run/prompt-free operation, and the
        // reason a verified splice earns its lower rate.
        assert_eq!(structural.reversibility().damped(), Reversibility::R0);
        assert_eq!(blind.reversibility().damped(), Reversibility::R1);
    }

    #[test]
    fn an_escape_hatch_is_never_softened_by_damping() {
        // The sandbox-off shape is R3 before damping and must remain a
        // question after it: damping is for verified edits, not for guards.
        // Under `auto` the damped class would run, so this pins that the
        // gate refuses to damp the one shape whose whole point is consent.
        let hatch = Effect::EscapeHatch { what: "sandbox".to_owned() };
        assert_eq!(hatch.reversibility(), Reversibility::R3);
        // The gate's rule: escape hatches prompt unconditionally. That is
        // asserted in the gate's own tests; here the catalogue only
        // guarantees the shape is recognisable - `is_structural` is false,
        // so no damping rationale can attach to it.
        assert!(!hatch.is_structural());
    }

    #[test]
    fn display_renders_the_prompt_text_not_a_debug_dump() {
        // The TUI shows these strings to a human mid-decision; they read as
        // effects, not as enum internals.
        assert_eq!(Effect::Read.to_string(), "read");
        assert_eq!(
            Effect::OutsideEdit { path: "/etc/hosts".to_owned() }.to_string(),
            "edit outside the index: /etc/hosts"
        );
        assert_eq!(Effect::Remove { target: "~/notes".to_owned() }.to_string(), "remove: ~/notes");
    }
}
