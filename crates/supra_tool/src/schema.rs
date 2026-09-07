//! Tool argument schemas: a closed shape language, not JSON Schema.
//!
//! The provider-facing manifest is a JSON value (whatever the provider's
//! API expects), but validation inside the harness uses this small closed
//! language instead. Two reasons, both paid for already in this project:
//!
//! 1. **JSON Schema is a second grammar.** Validating with one schema
//!    dialect and describing with another gives two implementations that
//!    disagree on exactly the inputs a weak model produces (T15.7's
//!    two-parsers lesson, restated).
//! 2. **A closed language is exhaustively testable.** Every type checks
//!    every JSON value, a test walks the whole table, and there is no
//!    `oneOf`/`allOf` composition whose semantics differ between
//!    validators.
//!
//! The language is deliberately minimal: what the built-in tool surface
//! needs (T17) is objects with typed, required-or-optional fields, and
//! strictness about unknown fields. Nothing more, until a real tool needs
//! more - and then the need arrives with the tool, not ahead of it.

use serde_json::Value;

/// The JSON type one argument field must hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldType {
    /// A JSON string.
    Text,
    /// A JSON number.
    Number,
    /// A JSON boolean.
    Boolean,
}

impl FieldType {
    /// The type's name, for the refusal message and the provider manifest.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Text => "string",
            Self::Number => "number",
            Self::Boolean => "boolean",
        }
    }

    /// Whether a JSON value holds this type.
    #[must_use]
    pub fn matches(self, value: &Value) -> bool {
        matches!(
            (self, value),
            (Self::Text, Value::String(_))
                | (Self::Number, Value::Number(_))
                | (Self::Boolean, Value::Bool(_))
        )
    }
}

/// One field of an object schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    /// The field's name.
    pub name: String,
    /// The JSON type the value must hold.
    pub field_type: FieldType,
    /// Whether the field must be present. An optional-but-present field is
    /// still type-checked: absent is a choice, wrong-typed is an error.
    pub required: bool,
    /// One sentence, shown in the provider manifest, that says what the
    /// field is for. The description is part of the tool's token cost, so
    /// it earns its place by being the difference between a model that
    /// uses the field correctly and one that guesses.
    pub description: String,
}

/// An object schema: typed fields, strict about unknowns.
///
/// Built with [`Schema::build`], which enforces the invariants a schema
/// must not violate: no duplicate field names, no empty field names, no
/// empty descriptions (a description that says nothing is token cost
/// without instruction).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Schema {
    tool: String,
    fields: Vec<Field>,
}

impl Schema {
    /// Build a schema for `tool`, or refuse with the reason.
    ///
    /// # Errors
    ///
    /// `BadArguments`-shaped refusals with the invariant's detail when the
    /// field list violates one: duplicate names, empty names, empty
    /// descriptions. Refusing at build time is what keeps validation
    /// simple later - a schema that could not be built badly cannot
    /// validate badly.
    pub fn build(tool: &str, fields: Vec<Field>) -> Result<Self, crate::error::ToolError> {
        let mut seen = std::collections::BTreeSet::new();
        for field in &fields {
            if field.name.is_empty() {
                return Err(crate::error::ToolError::BadArguments {
                    tool: tool.to_owned(),
                    got: "a schema with an empty field name".to_owned(),
                });
            }
            if !seen.insert(field.name.as_str()) {
                return Err(crate::error::ToolError::BadArguments {
                    tool: tool.to_owned(),
                    got: format!("schema field {:?} declared twice", field.name),
                });
            }
            if field.description.trim().is_empty() {
                return Err(crate::error::ToolError::BadArguments {
                    tool: tool.to_owned(),
                    got: format!("schema field {:?} has no description", field.name),
                });
            }
        }
        Ok(Self { tool: tool.to_owned(), fields })
    }

    /// The tool this schema belongs to.
    #[must_use]
    pub fn tool(&self) -> &str {
        &self.tool
    }

    /// The fields, in declaration order.
    #[must_use]
    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    /// Validate one field map against this schema.
    ///
    /// # Errors
    ///
    /// [`crate::error::ToolError::MissingField`] for an absent required
    /// field, [`crate::error::ToolError::WrongFieldType`] for a
    /// wrong-typed value, [`crate::error::ToolError::UnknownField`] for a
    /// field the schema does not know. Each message names the field, which
    /// is the whole robust-invocation contract: the retry can be correct.
    pub fn validate(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> Result<(), crate::error::ToolError> {
        for field in &self.fields {
            match arguments.get(&field.name) {
                None => {
                    if field.required {
                        return Err(crate::error::ToolError::MissingField {
                            tool: self.tool.clone(),
                            field: field.name.clone(),
                        });
                    }
                }
                Some(value) => {
                    if !field.field_type.matches(value) {
                        return Err(crate::error::ToolError::WrongFieldType {
                            tool: self.tool.clone(),
                            field: field.name.clone(),
                            expected: field.field_type.name(),
                            got: type_name(value).to_owned(),
                        });
                    }
                }
            }
        }
        // Unknown fields are refused, not tolerated: an extra `pathh`
        // beside a valid `path` is a typo the tool would otherwise run
        // against its default, which is silent corruption.
        for name in arguments.keys() {
            if !self.fields.iter().any(|field| field.name == *name) {
                return Err(crate::error::ToolError::UnknownField {
                    tool: self.tool.clone(),
                    field: name.clone(),
                });
            }
        }
        Ok(())
    }

    /// The provider-facing parameter schema, as a JSON value.
    ///
    /// The shape is the common denominator the major providers accept: an
    /// object with typed properties, `required`, and strict. Built from
    /// the same [`Field`] list that validates, so the two cannot disagree
    /// - one source of truth, rendered two ways.
    #[must_use]
    pub fn to_provider_schema(&self) -> Value {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();
        for field in &self.fields {
            let mut property = serde_json::Map::new();
            property.insert("type".to_owned(), Value::String(field.field_type.name().to_owned()));
            property.insert("description".to_owned(), Value::String(field.description.clone()));
            properties.insert(field.name.clone(), Value::Object(property));
            if field.required {
                required.push(Value::String(field.name.clone()));
            }
        }
        let mut schema = serde_json::Map::new();
        schema.insert("type".to_owned(), Value::String("object".to_owned()));
        schema.insert("properties".to_owned(), Value::Object(properties));
        schema.insert("required".to_owned(), Value::Array(required));
        schema.insert("additionalProperties".to_owned(), Value::Bool(false));
        Value::Object(schema)
    }
}

/// The JSON type a value holds, by name - for the refusal message.
fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::error::ToolError;

    fn edit_schema() -> Schema {
        Schema::build(
            "edit_file",
            vec![
                Field {
                    name: "path".to_owned(),
                    field_type: FieldType::Text,
                    required: true,
                    description: "The file to edit.".to_owned(),
                },
                Field {
                    name: "old".to_owned(),
                    field_type: FieldType::Text,
                    required: true,
                    description: "The text to replace.".to_owned(),
                },
                Field {
                    name: "new".to_owned(),
                    field_type: FieldType::Text,
                    required: true,
                    description: "The replacement text.".to_owned(),
                },
                Field {
                    name: "dry_run".to_owned(),
                    field_type: FieldType::Boolean,
                    required: false,
                    description: "Validate only; write nothing.".to_owned(),
                },
            ],
        )
        .expect("valid schema")
    }

    #[test]
    fn a_valid_argument_map_passes() {
        let schema = edit_schema();
        let arguments = serde_json::from_value::<serde_json::Map<String, Value>>(json!({
            "path": "a.rs", "old": "x", "new": "y", "dry_run": true
        }))
        .expect("map");
        assert_eq!(schema.validate(&arguments), Ok(()));

        // Optional field absent: fine.
        let arguments = serde_json::from_value::<serde_json::Map<String, Value>>(json!({
            "path": "a.rs", "old": "x", "new": "y"
        }))
        .expect("map");
        assert_eq!(schema.validate(&arguments), Ok(()));
    }

    #[test]
    fn a_missing_required_field_names_the_field() {
        let schema = edit_schema();
        let arguments = serde_json::from_value::<serde_json::Map<String, Value>>(json!({
            "path": "a.rs", "old": "x"
        }))
        .expect("map");
        assert_eq!(
            schema.validate(&arguments),
            Err(ToolError::MissingField { tool: "edit_file".into(), field: "new".into() })
        );
    }

    #[test]
    fn a_wrong_typed_field_names_both_types() {
        let schema = edit_schema();
        let arguments = serde_json::from_value::<serde_json::Map<String, Value>>(json!({
            "path": 7, "old": "x", "new": "y"
        }))
        .expect("map");
        assert_eq!(
            schema.validate(&arguments),
            Err(ToolError::WrongFieldType {
                tool: "edit_file".into(),
                field: "path".into(),
                expected: "string",
                got: "number".into(),
            })
        );
    }

    #[test]
    fn an_unknown_field_is_refused_not_ignored() {
        // The typo case the strictness exists for: `pathh` beside `path`
        // would run against a default if tolerated, which is corruption
        // wearing a success.
        let schema = edit_schema();
        let arguments = serde_json::from_value::<serde_json::Map<String, Value>>(json!({
            "path": "a.rs", "pathh": "b.rs", "old": "x", "new": "y"
        }))
        .expect("map");
        assert_eq!(
            schema.validate(&arguments),
            Err(ToolError::UnknownField { tool: "edit_file".into(), field: "pathh".into() })
        );
    }

    #[test]
    fn an_optional_field_is_still_type_checked() {
        let schema = edit_schema();
        let arguments = serde_json::from_value::<serde_json::Map<String, Value>>(json!({
            "path": "a.rs", "old": "x", "new": "y", "dry_run": "yes please"
        }))
        .expect("map");
        assert!(matches!(
            schema.validate(&arguments),
            Err(ToolError::WrongFieldType { field, .. }) if field == "dry_run"
        ));
    }

    #[test]
    fn a_schema_cannot_be_built_badly() {
        // The build-time invariants: duplicates, empty names, empty
        // descriptions all refuse, so validation never sees a malformed
        // schema.
        let duplicate = Schema::build(
            "t",
            vec![
                Field {
                    name: "a".into(),
                    field_type: FieldType::Text,
                    required: true,
                    description: "x".into(),
                },
                Field {
                    name: "a".into(),
                    field_type: FieldType::Text,
                    required: true,
                    description: "y".into(),
                },
            ],
        );
        assert!(duplicate.is_err(), "duplicate names refuse");

        let empty_name = Schema::build(
            "t",
            vec![Field {
                name: String::new(),
                field_type: FieldType::Text,
                required: true,
                description: "x".into(),
            }],
        );
        assert!(empty_name.is_err(), "empty names refuse");

        let no_description = Schema::build(
            "t",
            vec![Field {
                name: "a".into(),
                field_type: FieldType::Text,
                required: true,
                description: "  ".into(),
            }],
        );
        assert!(no_description.is_err(), "empty descriptions refuse");
    }

    #[test]
    fn the_provider_schema_agrees_with_validation() {
        // One source of truth, rendered two ways: every required field the
        // provider schema advertises is one validation demands, and the
        // strictness the provider schema declares is the strictness
        // validation enforces.
        let schema = edit_schema();
        let provider = schema.to_provider_schema();

        let advertised: Vec<String> = provider["required"]
            .as_array()
            .expect("required array")
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let enforced: Vec<String> =
            schema.fields().iter().filter(|field| field.required).map(|field| field.name.clone()).collect();
        assert_eq!(advertised, enforced);

        assert_eq!(provider["additionalProperties"], json!(false), "the manifest declares strict");
        assert_eq!(provider["type"], json!("object"));

        // And each property's declared type is the one validation checks.
        for field in schema.fields() {
            let declared = &provider["properties"][&field.name]["type"];
            assert_eq!(declared, &json!(field.field_type.name()), "{}", field.name);
        }
    }
}
