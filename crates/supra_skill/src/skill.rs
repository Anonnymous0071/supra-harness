//! SKILL.md: one file, one skill.
//!
//! The format is front matter plus body, the shape every skill system
//! the user studied converged on (Kimchi's skills, the SKILL.md files in
//! the 0xPony tree): YAML-ish key/value pairs between `---` fences, then
//! the skill's instructions as Markdown. The front matter is parsed by
//! hand - a `key: value` line grammar is small enough to own, and pulling
//! a YAML dependency for six keys would carry its version pins and its
//! grammar ambiguities into a file the user edits by hand (the T15.7
//! two-parsers lesson, applied before the second parser could exist).
//!
//! Two fields are required: `name` (identity - what dependencies and the
//! model refer to) and `description` (what the model reads to decide
//! whether to open the body; the token-efficiency contract). One more is
//! optional: `requires`, a comma-separated list of skill names this one
//! builds on. Everything else in the front matter is preserved verbatim
//! under `extra` - unknown keys are the author's business, not the
//! loader's to refuse.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::SkillError;

/// One parsed skill.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    /// The skill's name, from the front matter. Directory names and file
    /// names are not it: the front matter is the only authority, so a
    /// renamed file keeps its identity and a moved file keeps its
    /// dependencies.
    pub name: String,
    /// One or two sentences, from the front matter: what the skill is
    /// for. This is the only part that rides in the prompt by default -
    /// the body is content, opened on demand (I2: the body never mutates
    /// `system`).
    pub description: String,
    /// The names this skill requires, from the optional `requires` field.
    /// Order is the author's; the loader sorts into topological order for
    /// execution and refuses cycles.
    pub requires: Vec<String>,
    /// The body: everything after the closing `---` fence, verbatim.
    /// Verbatim is load-bearing - the model sees the author's formatting,
    /// and a reflowed body is a different skill.
    pub body: String,
    /// Front-matter keys the loader does not interpret, preserved in
    /// file order. A skill can carry its own metadata (a version, an
    /// author, a category) without the loader pretending to understand it.
    pub extra: BTreeMap<String, String>,
}

impl Skill {
    /// Parse one SKILL.md file.
    ///
    /// # Errors
    ///
    /// [`SkillError::Io`] when the file cannot be read;
    /// [`SkillError::Parse`] when the fences or the `key: value` grammar
    /// are violated; [`SkillError::MissingField`] when `name` or
    /// `description` is absent or blank. Each parse error quotes the
    /// offending line.
    pub fn parse(path: impl AsRef<Path>) -> Result<Self, SkillError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)?;
        Self::parse_str(path.to_string_lossy().into_owned(), &text)
    }

    /// Parse skill text that has already been read. The path is for the
    /// error messages, which name the file the author must fix.
    ///
    /// # Errors
    ///
    /// As [`Skill::parse`], minus the I/O.
    pub fn parse_str(path: String, text: &str) -> Result<Self, SkillError> {
        let (front, body) = split_fences(&path, text)?;

        let mut name: Option<String> = None;
        let mut description: Option<String> = None;
        let mut requires: Vec<String> = Vec::new();
        let mut extra = BTreeMap::new();

        for line in front.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once(':') else {
                return Err(SkillError::Parse {
                    path,
                    detail: format!("expected `key: value`, found {line:?}"),
                });
            };
            let key = key.trim();
            let value = value.trim();
            match key {
                "name" => name = Some(nonblank(path.clone(), "name", value)?),
                "description" => description = Some(nonblank(path.clone(), "description", value)?),
                "requires" => {
                    requires = value
                        .split(',')
                        .map(str::trim)
                        .filter(|entry| !entry.is_empty())
                        .map(str::to_owned)
                        .collect::<Vec<_>>();
                }
                _ => {
                    extra.insert(key.to_owned(), value.to_owned());
                }
            }
        }

        let Some(name) = name else {
            return Err(SkillError::MissingField { path, field: "name" });
        };
        let Some(description) = description else {
            return Err(SkillError::MissingField { path, field: "description" });
        };

        Ok(Self { name, description, requires, body: body.to_owned(), extra })
    }
}

fn nonblank(path: String, field: &'static str, value: &str) -> Result<String, SkillError> {
    if value.is_empty() {
        return Err(SkillError::MissingField { path, field });
    }
    Ok(value.to_owned())
}

/// Split the front matter from the body at the `---` fences.
///
/// The grammar: optional leading blank lines, one `---`, the front
/// matter, one closing `---`, then the body. A file with no fences is a
/// parse error rather than an empty front matter - a body the author
/// forgot to fence is a bug the error names, not a skill with no name.
fn split_fences<'a>(path: &str, text: &'a str) -> Result<(&'a str, &'a str), SkillError> {
    let trimmed_start = text.trim_start_matches('\n');
    let Some(after_open) =
        trimmed_start.strip_prefix("---\n").or_else(|| trimmed_start.strip_prefix("---\r\n"))
    else {
        return Err(SkillError::Parse {
            path: path.to_owned(),
            detail: "the file must open with a `---` fence before the front matter".to_owned(),
        });
    };
    // The closing fence may be `---` at line start (the common form) or
    // the last line without a trailing newline. Anything else on the fence
    // line (`---oops`, `----`) is not a fence: the suffix would otherwise
    // become body content.
    let Some(offset) = after_open.find("\n---") else {
        return Err(SkillError::Parse {
            path: path.to_owned(),
            detail: "the front matter is never closed; add a `---` fence after it".to_owned(),
        });
    };
    let fence_end = offset + "\n---".len();
    let rest_of_line = &after_open[fence_end..];
    let line_end = rest_of_line.find('\n').unwrap_or(rest_of_line.len());
    let trailer = &rest_of_line[..line_end];
    if !trailer.is_empty() && trailer != "\r" {
        return Err(SkillError::Parse {
            path: path.to_owned(),
            detail: "the closing fence must be exactly `---` on its own line".to_owned(),
        });
    }
    let front = &after_open[..offset];
    let after_close = &after_open[fence_end + line_end..];
    // Drop the rest of the closing fence line: the newline before the body
    // is the body's.
    let body = after_close.strip_prefix('\n').unwrap_or(after_close);
    Ok((front, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = "---\nname: test\ndescription: A test skill.\n---\nBody text.\n";

    #[test]
    fn a_minimal_skill_parses() {
        let skill = Skill::parse_str("SKILL.md".to_owned(), MINIMAL).expect("parse");
        assert_eq!(skill.name, "test");
        assert_eq!(skill.description, "A test skill.");
        assert!(skill.requires.is_empty(), "no requires field, no requires");
        assert_eq!(skill.body, "Body text.\n");
    }

    #[test]
    fn a_suffixed_closing_fence_is_refused() {
        for text in [
            "---\nname: test\ndescription: d.\n---oops\nBody.\n",
            "---\nname: test\ndescription: d.\n----\nBody.\n",
            "---\nname: test\ndescription: d.\n--- trailing\nBody.\n",
        ] {
            let error = Skill::parse_str("SKILL.md".to_owned(), text).expect_err("not a fence");
            assert!(error.to_string().contains("exactly `---`"), "{error}");
        }
    }

    #[test]
    fn a_crlf_closing_fence_parses() {
        let text = "---\r\nname: test\r\ndescription: d.\r\n---\r\nBody.\r\n";
        let skill = Skill::parse_str("SKILL.md".to_owned(), text).expect("crlf parses");
        assert_eq!(skill.name, "test");
        assert_eq!(skill.body, "Body.\r\n");
    }

    #[test]
    fn requires_parses_as_a_comma_separated_list() {
        let text = "---\nname: t\ndescription: d\nrequires: one, two ,three\n---\n";
        let skill = Skill::parse_str("SKILL.md".to_owned(), text).expect("parse");
        assert_eq!(skill.requires, vec!["one", "two", "three"], "trimmed, in order");
    }

    #[test]
    fn unknown_front_matter_keys_are_preserved_not_refused() {
        let text = "---\nname: t\ndescription: d\nversion: 1.2\nauthor: someone\n---\n";
        let skill = Skill::parse_str("SKILL.md".to_owned(), text).expect("parse");
        assert_eq!(skill.extra.get("version").map(String::as_str), Some("1.2"));
        assert_eq!(skill.extra.get("author").map(String::as_str), Some("someone"));
    }

    #[test]
    fn missing_fields_are_named() {
        for (text, field) in [
            ("---\ndescription: d\n---\n", "name"),
            ("---\nname: t\n---\n", "description"),
            ("---\nname: \ndescription: d\n---\n", "name"),
        ] {
            match Skill::parse_str("SKILL.md".to_owned(), text) {
                Err(SkillError::MissingField { field: got, .. }) => assert_eq!(got, field),
                other => panic!("{text:?} must refuse with MissingField {field}; got {other:?}"),
            }
        }
    }

    #[test]
    fn a_file_without_fences_is_a_parse_error_not_an_empty_skill() {
        let error = Skill::parse_str("SKILL.md".to_owned(), "just a body").expect_err("no fences");
        assert!(matches!(error, SkillError::Parse { .. }), "{error:?}");
    }

    #[test]
    fn an_unclosed_front_matter_is_named() {
        let text = "---\nname: t\ndescription: d\n";
        let error = Skill::parse_str("SKILL.md".to_owned(), text).expect_err("unclosed");
        assert!(error.to_string().contains("never closed"), "{error}");
    }

    #[test]
    fn a_malformed_line_is_quoted_in_the_error() {
        let text = "---\nname: t\ndescription: d\nnot a pair\n---\n";
        let error = Skill::parse_str("SKILL.md".to_owned(), text).expect_err("malformed");
        let SkillError::Parse { detail, .. } = error else { panic!("shape") };
        assert!(detail.contains("not a pair"), "{detail}");
    }

    #[test]
    fn the_body_is_verbatim_including_blank_lines() {
        let body = "line one\n\n  indented stays indented\n\ttab too\n";
        let text = format!("---\nname: t\ndescription: d\n---\n{body}");
        let skill = Skill::parse_str("SKILL.md".to_owned(), &text).expect("parse");
        assert_eq!(skill.body, body, "no reflow, no trim, byte for byte");
    }
}
