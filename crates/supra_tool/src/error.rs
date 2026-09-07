//! What an invocation refused, and what the model should do about it.
//!
//! The robust-invocation contract is the user's standing mandate: a weak
//! tool-calling model must never *fail* a tool call. Every refusal below is
//! a structured, machine-readable answer the model can act on - the
//! alternative, an error string the model cannot parse, is how tool loops
//! derail. Each variant names the fix in its message, because the message
//! is the model's only channel.
//!
//! | Variant | Shape | The model's next move |
//! | ------- | ----- | ---------------------- |
//! | `UnknownTool` | the name is not in the registry | read the manifest list, retry with a registered name |
//! | `BadArguments` | the arguments are not a JSON object | re-emit as an object |
//! | `MissingField` / `WrongFieldType` | the schema refuses | add or fix the named field |
//! | `UnknownField` | strict schemas refuse extras | drop the named field |
//! | `PreconditionFailed` | the instruction-carrying precondition | perform the named required step first |
//! | `Disabled` | the tool is frozen out for this session | choose another tool; do not retry |

use thiserror::Error;

impl From<supra_llm::CanonicalError> for ToolError {
    fn from(error: supra_llm::CanonicalError) -> Self {
        // The canonicaliser's refusal (invalid JSON, duplicate keys) is a
        // BadArguments shape: the arguments as sent cannot become a JSON
        // object, and the message names what the parser rejected - which
        // is exactly what the retry needs.
        Self::BadArguments {
            tool: String::new(),
            got: format!("arguments are not valid canonical JSON: {error}"),
        }
    }
}

/// A tool invocation refusal. Structured by contract: `PreconditionFailed`
/// and `BadArguments` carry the fields a retry needs, not a prose blob.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ToolError {
    /// The named tool is not in the registry.
    #[error("no tool named {name:?}; the registered tools are: {known:?}")]
    UnknownTool {
        /// The name the model called.
        name: String,
        /// The registered names, for the model's next attempt.
        known: Vec<String>,
    },

    /// The arguments are not a JSON object. Providers send strings, arrays,
    /// and numbers when a weak model loses the schema; refusing with the
    /// shape it sent is how the retry gets to be correct.
    #[error("tool {tool:?} arguments must be a JSON object, got {got}")]
    BadArguments {
        /// Which tool.
        tool: String,
        /// What arrived instead: "string", "array", "number", ...
        got: String,
    },

    /// A required field is absent.
    #[error("tool {tool:?} is missing required field {field:?}")]
    MissingField {
        /// Which tool.
        tool: String,
        /// The field the schema requires.
        field: String,
    },

    /// A field's value has the wrong JSON type.
    #[error("tool {tool:?} field {field:?} must be {expected}, got {got}")]
    WrongFieldType {
        /// Which tool.
        tool: String,
        /// The field.
        field: String,
        /// The JSON type the schema requires.
        expected: &'static str,
        /// The JSON type that arrived.
        got: String,
    },

    /// A strict schema refuses a field it does not know. Extra fields are
    /// the weak-model failure mode that corrupts silently when tolerated:
    /// a typo'd `pathh` beside a valid `path` would run against the wrong
    /// default rather than fail.
    #[error("tool {tool:?} does not take field {field:?}")]
    UnknownField {
        /// Which tool.
        tool: String,
        /// The field the schema does not know.
        field: String,
    },

    /// An instruction-carrying precondition refused the call.
    ///
    /// The message names the step (`step`) and what it unlocks (`needs`):
    /// ``read_file src/main.rs before editing it`` is an instruction the
    /// model can follow, which is the whole §5 design - the workflow is
    /// not described in prose, it is the only path the tool permits.
    #[error("precondition failed: {step}")]
    PreconditionFailed {
        /// The step to perform first, phrased as an instruction.
        step: String,
        /// What the step unlocks.
        needs: String,
    },

    /// The tool is disabled for this session (I3: `allowed_tools` or
    /// `tool_choice`, never removal). Retrying is wrong; choosing another
    /// tool is right.
    #[error("tool {name:?} is disabled for this session")]
    Disabled {
        /// The disabled tool's name.
        name: String,
    },
}
