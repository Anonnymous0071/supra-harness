//! The tool registry: frozen manifests, robust invocation, preconditions.

use std::collections::BTreeMap;

use supra_llm::canonicalize;
use supra_permission::Effect;
use supra_types::{CanonicalJson, ToolClass};

use crate::error::ToolError;
use crate::schema::{Field, Schema};

/// One registered tool.
///
/// Everything the registry needs to describe, validate, and classify an
/// invocation of this tool - and nothing else. Execution is not the
/// registry's business: T23's turn loop dispatches the validated call to
/// the executor, through the permission gate (T16.7) and the sandbox
/// (T16).
#[derive(Clone, Debug)]
pub struct Tool {
    name: String,
    class: ToolClass,
    schema: Schema,
    /// Resolves an invocation's arguments into the permission catalogue's
    /// effect shape. T16.7's rule: classification runs on the resolved
    /// effect, never the tool name - and the registry is the layer that
    /// knows the arguments, so the registry is the layer that resolves.
    resolver: fn(&serde_json::Map<String, serde_json::Value>) -> Effect,
    /// The precondition, if this tool carries one. See [`Precondition`].
    precondition: Option<Precondition>,
}

/// An instruction-carrying precondition: the §5 design.
///
/// The architecture's table is explicit about the shape this replaces:
/// "always read a file before editing" as a prose instruction (~10 tokens,
/// unreliable) versus `edit_file` **failing** with a structured error if
/// the file was not read this session (0 prompt tokens, reliable). The
/// workflow is not described; it is the only path the tool surface
/// permits.
///
/// The check is a function over a session state the caller supplies:
/// `read_files` is the set of files read this session, and the
/// precondition decides membership. The registry does not own session
/// state (T23 does); it owns the rule.
#[derive(Clone)]
pub struct Precondition {
    /// The fields whose values the rule needs - typically `path`.
    fields: Vec<String>,
    /// The rule itself. Returns `Err` with the structured refusal the
    /// model reads; the message is an instruction ("read {file} before
    /// editing it"), which is what makes the precondition
    /// instruction-carrying rather than merely a barrier.
    check: fn(&serde_json::Map<String, serde_json::Value>, &SessionFacts) -> Result<(), ToolError>,
}

impl core::fmt::Debug for Precondition {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Precondition").field("fields", &self.fields).finish_non_exhaustive()
    }
}

impl Precondition {
    /// The read-before-edit rule, the architecture's own example.
    ///
    /// `path` must have been read this session. A session fact of "not
    /// read" refuses with the instruction that satisfies the rule - the
    /// model's next call is `read_file`, not a retry.
    #[must_use]
    pub fn read_before_edit() -> Self {
        Self {
            fields: vec!["path".to_owned()],
            check: |arguments, facts| {
                let path = arguments.get("path").and_then(serde_json::Value::as_str).unwrap_or_default();
                if facts.files_read.contains(path) {
                    Ok(())
                } else {
                    Err(ToolError::PreconditionFailed {
                        step: format!("read_file {path:?} before editing it"),
                        needs: "edit_file requires the file to have been read this session".to_owned(),
                    })
                }
            },
        }
    }

    /// The rule's input fields.
    #[must_use]
    pub fn fields(&self) -> &[String] {
        &self.fields
    }
}

/// What the session knows, for precondition checks.
///
/// Owned by the turn loop (T23); passed here by reference so the
/// registry's checks stay pure and the state stays where it belongs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionFacts {
    /// The files read this session, by the path as the tool received it.
    pub files_read: std::collections::BTreeSet<String>,
}

impl Tool {
    /// Register a tool with all its surfaces.
    ///
    /// # Errors
    ///
    /// [`ToolError::BadArguments`] when the schema refuses to build - the
    /// build-time invariants (duplicates, empty names, empty
    /// descriptions) surface here so a bad registration fails at
    /// registration, not at the first call.
    pub fn register(
        name: &str,
        class: ToolClass,
        fields: Vec<Field>,
        resolver: fn(&serde_json::Map<String, serde_json::Value>) -> Effect,
    ) -> Result<Self, ToolError> {
        Ok(Self {
            name: name.to_owned(),
            class,
            schema: Schema::build(name, fields)?,
            resolver,
            precondition: None,
        })
    }

    /// The tool's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The authority class (T12.5): who may invoke.
    #[must_use]
    pub const fn class(&self) -> ToolClass {
        self.class
    }

    /// The argument schema.
    #[must_use]
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// Attach a precondition. Builder-style, used at registration.
    #[must_use]
    pub fn with_precondition(mut self, precondition: Precondition) -> Self {
        self.precondition = Some(precondition);
        self
    }

    /// The provider-facing manifest fragment: name, description, and
    /// parameter schema, exactly as the wire wants it. Rendered from the
    /// same fields that validate, so the manifest and the validation
    /// cannot disagree.
    #[must_use]
    pub fn to_manifest(&self, description: &str) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "description": description,
            "input_schema": self.schema.to_provider_schema(),
        })
    }

    /// Validate one invocation's arguments against the schema.
    ///
    /// # Errors
    ///
    /// The schema's structured refusals - see [`Schema::validate`].
    pub fn validate(&self, arguments: &serde_json::Map<String, serde_json::Value>) -> Result<(), ToolError> {
        self.schema.validate(arguments)
    }

    /// Resolve this invocation's effect shape, for the permission gate.
    #[must_use]
    pub fn resolve_effect(&self, arguments: &serde_json::Map<String, serde_json::Value>) -> Effect {
        (self.resolver)(arguments)
    }

    /// Check this invocation against the precondition, if any.
    ///
    /// # Errors
    ///
    /// [`ToolError::PreconditionFailed`] when the rule refuses - with the
    /// instruction that satisfies it.
    pub fn check_precondition(
        &self,
        arguments: &serde_json::Map<String, serde_json::Value>,
        facts: &SessionFacts,
    ) -> Result<(), ToolError> {
        match &self.precondition {
            Some(precondition) => (precondition.check)(arguments, facts),
            None => Ok(()),
        }
    }
}

/// The frozen registry: built once at startup, never mutated after.
///
/// I3 is the binding constraint: every tool is registered at startup;
/// disabling uses `allowed_tools` or `tool_choice`, never removal; dynamic
/// discovery is append-only. The type makes the first part structural -
/// there is no `remove`, and the interior map is private - and
/// [`Registry::disable`] is the session-set of names the dispatcher skips,
/// not a mutation of the registry itself.
pub struct Registry {
    tools: BTreeMap<String, Tool>,
    disabled: std::collections::BTreeSet<String>,
}

impl Registry {
    /// An empty registry. Tools arrive with [`Registry::register`] before
    /// the session starts; the frozen-at-startup rule means nothing
    /// arrives after.
    #[must_use]
    pub fn new() -> Self {
        Self { tools: BTreeMap::new(), disabled: std::collections::BTreeSet::new() }
    }

    /// Register one tool.
    ///
    /// # Errors
    ///
    /// [`ToolError::UnknownTool`]-shaped duplicate refusal when the name
    /// is already registered: a second registration of one name is a bug
    /// in the startup order, and silently replacing the first would leave
    /// the manifest and the executor disagreeing about which tool runs.
    pub fn register(&mut self, tool: Tool) -> Result<(), ToolError> {
        if self.tools.contains_key(tool.name()) {
            return Err(ToolError::UnknownTool {
                name: tool.name().to_owned(),
                known: vec![format!("{} is already registered", tool.name())],
            });
        }
        self.tools.insert(tool.name().to_owned(), tool);
        Ok(())
    }

    /// Mark a tool disabled for this session (I3: never removal).
    ///
    /// # Errors
    ///
    /// [`ToolError::UnknownTool`] when the name is not registered - a
    /// disabled unknown is a configuration bug worth naming.
    pub fn disable(&mut self, name: &str) -> Result<(), ToolError> {
        if !self.tools.contains_key(name) {
            return Err(ToolError::UnknownTool { name: name.to_owned(), known: self.names() });
        }
        self.disabled.insert(name.to_owned());
        Ok(())
    }

    /// The registered names, sorted.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// The manifest list, as the provider-facing `tools` array. Every
    /// registered tool appears, disabled or not - I3: disabling uses
    /// `allowed_tools`, and the manifest the model saw at BP1 stays
    /// byte-stable.
    #[must_use]
    pub fn manifest(&self, descriptions: &BTreeMap<String, String>) -> Vec<serde_json::Value> {
        self.tools
            .values()
            .map(|tool| {
                let description = descriptions.get(tool.name()).map(String::as_str).unwrap_or_default();
                tool.to_manifest(description)
            })
            .collect()
    }

    /// Validate and dispatch one invocation.
    ///
    /// The robust-invocation contract, in order:
    ///
    /// 1. The name resolves, or the refusal lists the registered names.
    /// 2. The tool is not disabled, or the refusal says so (a retry is
    ///    wrong; another tool is right).
    /// 3. The arguments are a JSON object, or the refusal names the shape
    ///    that arrived - the weak-model failure mode this exists for.
    /// 4. The arguments canonicalise through T13's serialiser - the same
    ///    producer every `CanonicalJson` must come from, so argument
    ///    bytes are stable from the model's pen to the ledger.
    /// 5. The schema validates, with structured, field-naming refusals.
    /// 6. The precondition checks, with the instruction that satisfies it.
    ///
    /// On success the invocation is validated but not executed: the
    /// caller (T23) runs the permission gate on the resolved effect, then
    /// dispatches.
    ///
    /// # Errors
    ///
    /// The refusal shapes above, each carrying what the retry needs.
    pub fn invoke(&self, name: &str, arguments: &str, facts: &SessionFacts) -> Result<Invocation, ToolError> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| ToolError::UnknownTool { name: name.to_owned(), known: self.names() })?;

        if self.disabled.contains(name) {
            return Err(ToolError::Disabled { name: name.to_owned() });
        }

        // Canonicalisation first, validation on the parsed value: one
        // parse (T13's strict parser, duplicate-refusing), no
        // re-serialisation, and the argument bytes that reach the ledger
        // are the bytes the model produced in canonical form. The same
        // strict parser reads the map back - never a second serialiser
        // whose output could disagree with the first.
        let canonical = canonicalize(arguments)?;
        let parsed = supra_llm::parse_strict(arguments).map_err(ToolError::from)?;
        let Value::Object(map) = &parsed else {
            return Err(ToolError::BadArguments { tool: name.to_owned(), got: "not an object".to_owned() });
        };

        tool.validate(map)?;
        tool.check_precondition(map, facts)?;

        Ok(Invocation { tool: name.to_owned(), arguments: canonical, effect: tool.resolve_effect(map) })
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

/// The validated invocation the turn loop receives.
///
/// `arguments` is the canonical text - the same string the ledger's
/// `ToolUse` block will hold, produced by the same serialiser. `effect`
/// is what the permission gate classifies.
#[derive(Clone, Debug)]
pub struct Invocation {
    tool: String,
    arguments: CanonicalJson,
    effect: Effect,
}

impl Invocation {
    /// The tool's name.
    #[must_use]
    pub fn tool(&self) -> &str {
        &self.tool
    }

    /// The canonical argument text, as the ledger will hold it.
    #[must_use]
    pub fn arguments(&self) -> &CanonicalJson {
        &self.arguments
    }

    /// The resolved effect, for the permission gate.
    #[must_use]
    pub fn effect(&self) -> &Effect {
        &self.effect
    }
}

use serde_json::Value;

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::schema::FieldType;
    use supra_permission::Effect as EffectShape;

    fn edit_file_tool() -> Tool {
        Tool::register(
            "edit_file",
            ToolClass::Agent,
            vec![
                Field {
                    name: "path".into(),
                    field_type: FieldType::Text,
                    required: true,
                    description: "The file to edit.".into(),
                },
                Field {
                    name: "old".into(),
                    field_type: FieldType::Text,
                    required: true,
                    description: "Text to replace.".into(),
                },
                Field {
                    name: "new".into(),
                    field_type: FieldType::Text,
                    required: true,
                    description: "Replacement text.".into(),
                },
            ],
            // The blind-edit resolver: a real registry inspects git status
            // and the journal; this one resolves the architecture's R2
            // shape so the tests assert the plumbing, not the policy.
            |_| EffectShape::BlindEdit,
        )
        .expect("register")
        .with_precondition(Precondition::read_before_edit())
    }

    fn registry() -> Registry {
        let mut registry = Registry::new();
        registry.register(edit_file_tool()).expect("register");
        registry
    }

    #[test]
    fn an_unknown_tool_lists_the_registered_names() {
        let registry = registry();
        let error = registry.invoke("edit_fiile", "{}", &SessionFacts::default()).expect_err("unknown");
        let ToolError::UnknownTool { name, known } = error else { panic!("shape") };
        assert_eq!(name, "edit_fiile");
        assert_eq!(known, vec!["edit_file".to_owned()]);
    }

    #[test]
    fn non_object_arguments_name_the_arrived_shape() {
        let registry = registry();
        let facts = SessionFacts { files_read: ["a.rs".to_owned()].into_iter().collect() };
        // A weak model sends a string of JSON rather than an object; the
        // string here is *valid JSON* that canonicalises to a non-object,
        // which is the exact corner: canonicalisation succeeds, the
        // object check refuses, and the refusal names what arrived.
        let error = registry.invoke("edit_file", "\"just a string\"", &facts).expect_err("not an object");
        assert!(
            matches!(error, ToolError::BadArguments { tool, got } if tool == "edit_file" && got == "not an object")
        );
    }

    #[test]
    fn schema_refusals_carry_the_field() {
        let registry = registry();
        let facts = SessionFacts { files_read: ["a.rs".to_owned()].into_iter().collect() };
        let error =
            registry.invoke("edit_file", r#"{ "path": "a.rs" }"#, &facts).expect_err("missing fields");
        assert!(matches!(error, ToolError::MissingField { field, .. } if field == "old"));

        let error = registry
            .invoke("edit_file", r#"{ "path": 3, "old": "x", "new": "y" }"#, &facts)
            .expect_err("wrong type");
        assert!(
            matches!(error, ToolError::WrongFieldType { field, expected, .. } if field == "path" && expected == "string")
        );
    }

    #[test]
    fn the_precondition_refuses_with_an_instruction() {
        let registry = registry();
        let empty_facts = SessionFacts::default();
        let error = registry
            .invoke("edit_file", r#"{ "path": "a.rs", "old": "x", "new": "y" }"#, &empty_facts)
            .expect_err("not read");
        let ToolError::PreconditionFailed { step, needs } = error else { panic!("shape") };
        // The §5 contract, verbatim: the refusal is an instruction the
        // model can follow, not a barrier it can only bounce off.
        assert!(step.contains("read_file"), "{step}");
        assert!(step.contains("a.rs"), "{step}");
        assert!(needs.contains("read"), "{needs}");

        // And after the read, the same call passes the precondition.
        let facts = SessionFacts { files_read: ["a.rs".to_owned()].into_iter().collect() };
        let invocation = registry
            .invoke("edit_file", r#"{ "path": "a.rs", "old": "x", "new": "y" }"#, &facts)
            .expect("read satisfies");
        assert_eq!(invocation.tool(), "edit_file");
    }

    #[test]
    fn the_invocation_carries_canonical_arguments_and_the_effect() {
        // Key order is the weak-model variance; the canonicaliser sorts it,
        // and the ledger holds the same bytes the invocation produced.
        let registry = registry();
        let facts = SessionFacts { files_read: ["a.rs".to_owned()].into_iter().collect() };
        let invocation = registry
            .invoke("edit_file", r#"{ "new": "y", "path": "a.rs", "old": "x" }"#, &facts)
            .expect("invoke");
        assert_eq!(
            invocation.arguments().as_str(),
            r#"{"new":"y","old":"x","path":"a.rs"}"#,
            "canonical: sorted keys, no whitespace"
        );
        assert!(matches!(invocation.effect(), EffectShape::BlindEdit));
    }

    #[test]
    fn disabled_is_a_session_state_not_a_removal() {
        // I3: disabling never removes. The manifest still lists the tool
        // (byte-stable for the prefix), and the invocation is refused with
        // the shape that says "choose another tool".
        let mut registry = registry();
        registry.disable("edit_file").expect("disable");

        let manifest = registry.manifest(&BTreeMap::new());
        assert_eq!(manifest.len(), 1, "the disabled tool is still in the manifest");
        assert_eq!(manifest[0]["name"], json!("edit_file"));

        let facts = SessionFacts { files_read: ["a.rs".to_owned()].into_iter().collect() };
        let error = registry
            .invoke("edit_file", r#"{ "path": "a.rs", "old": "x", "new": "y" }"#, &facts)
            .expect_err("disabled");
        assert!(matches!(error, ToolError::Disabled { name } if name == "edit_file"));
    }

    #[test]
    fn a_duplicate_registration_refuses() {
        let mut registry = registry();
        let error = registry.register(edit_file_tool()).expect_err("duplicate");
        assert!(matches!(error, ToolError::UnknownTool { .. }));
    }

    #[test]
    fn the_resolver_receives_the_validated_arguments() {
        // The effect resolver sees the same map validation approved, so
        // classification runs on the resolved effect with the model's own
        // argument values - never on the tool name alone.
        let mut registry = Registry::new();
        registry
            .register(
                Tool::register(
                    "shell_run",
                    ToolClass::Host,
                    vec![Field {
                        name: "command".into(),
                        field_type: FieldType::Text,
                        required: true,
                        description: "The command line.".into(),
                    }],
                    |arguments| {
                        let command =
                            arguments.get("command").and_then(serde_json::Value::as_str).unwrap_or_default();
                        // The architecture's two examples, as one resolver: the
                        // same tool, effects that differ by resolved content.
                        if command.contains("rm -rf") {
                            EffectShape::Remove { target: command.to_owned() }
                        } else {
                            EffectShape::ScratchWork
                        }
                    },
                )
                .expect("register"),
            )
            .expect("register shell");

        let effect = registry
            .invoke("shell_run", r#"{ "command": "cargo test" }"#, &SessionFacts::default())
            .expect("invoke")
            .effect()
            .clone();
        assert_eq!(effect, EffectShape::ScratchWork, "cargo test is R0");

        let effect = registry
            .invoke("shell_run", r#"{ "command": "rm -rf node_modules" }"#, &SessionFacts::default())
            .expect("invoke")
            .effect()
            .clone();
        assert!(matches!(effect, EffectShape::Remove { .. }), "rm -rf is R3, same tool");
    }

    #[test]
    fn the_manifest_is_byte_stable_across_calls() {
        // I3's cache property, observed: the same registry renders the
        // same manifest bytes, and a disable does not change them.
        let mut registry = registry();
        let descriptions = BTreeMap::from([("edit_file".to_owned(), "Edit a file.".to_owned())]);

        let first = serde_json::to_string(&registry.manifest(&descriptions)).expect("serialise");
        let second = serde_json::to_string(&registry.manifest(&descriptions)).expect("serialise");
        assert_eq!(first, second);

        registry.disable("edit_file").expect("disable");
        let third = serde_json::to_string(&registry.manifest(&descriptions)).expect("serialise");
        assert_eq!(first, third, "disabling is not a manifest mutation");
    }
}
