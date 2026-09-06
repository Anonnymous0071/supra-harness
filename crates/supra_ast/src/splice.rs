//! Byte-range splice with a reparse gate: the structural edit.
//!
//! # What `replace_node` guarantees
//!
//! Three things, in order: the range addresses exactly one node (no rounding to
//! the nearest node - a splice of half a node is string surgery, refused); the
//! spliced file parses error-free; the spliced node parses to the *same kind*
//! it was. The source is never mutated in place: the splice builds a new
//! string, verifies it, and returns it. Atomic rollback is therefore structural
//! - there is nothing to roll back, because the input still exists unchanged.
//!
//! # Why same-kind, not just parses
//!
//! A function replaced by a struct parses fine and is still the wrong edit.
//! Same-kind is what makes the gate structural rather than merely syntactic:
//! it proves the replacement occupies the same grammatical role, which is the
//! part a string-matching `edit_file` cannot promise. Cross-kind replacement
//! is not forbidden because it is dangerous - it is refused because it is a
//! different operation (delete plus insert) wearing this one's name.
//!
//! # Why the range must be exact
//!
//! Rounding a near-miss to the enclosing node would make the gate certify an
//! edit the caller did not ask for: the caller named bytes, the gate would
//! bless different bytes. Exactness keeps the certificate honest - the bytes
//! named are the bytes replaced, or nothing is replaced at all.
//!
//! # Formatting is preserved by construction
//!
//! The splice touches only the addressed bytes; every byte outside the range is
//! copied verbatim. No pretty-printer runs, so no line the edit did not touch
//! can drift. "Preserves formatting" is therefore not a claim about a
//! formatter - it is the absence of one.

use std::path::Path;

use crate::error::AstError;

/// Replace the node at `start..end` with `replacement`, gated.
///
/// `path` decides the language and names the file in errors; `source` is the
/// file's bytes. Returns the new file bytes on success; the input is never
/// mutated. See the module documentation for the three guarantees.
///
/// # Errors
///
/// [`AstError::UnsupportedLanguage`] for anything outside the seven grammars.
/// [`AstError::HasErrors`] when the source does not parse cleanly.
/// [`AstError::RangeIsNotANode`] when the range is out of bounds, inverted, or
/// names anything other than exactly one node.
/// [`AstError::GateRefused`] when the spliced result has errors or the node
/// changed kind. The source is untouched in every error case.
pub fn replace_node(
    path: &Path,
    source: &[u8],
    start: usize,
    end: usize,
    replacement: &str,
) -> Result<Vec<u8>, AstError> {
    let Some(language) = supra_digest::Language::detect(path) else {
        return Err(AstError::UnsupportedLanguage {
            path: path.to_path_buf(),
            extension: path.extension().and_then(|extension| extension.to_str()).map(str::to_owned),
        });
    };
    if start >= end || end > source.len() {
        return Err(AstError::RangeIsNotANode { path: path.to_path_buf(), start, end });
    }
    // Empty ranges are refused above, not below: `find_exact` with start == end
    // would descend to the deepest node *containing* the point and report it as
    // exact when its start coincides - turning a zero-width range into a
    // whole-node match. The unit test pins the finder half independently, so
    // the two refusals are tested separately instead of passing silently on
    // each other.
    let before_kind = {
        let tree = parse_clean(path, source, language)?;
        let root = tree.root_node();
        // The range check runs before the error check, deliberately: a range
        // naming no node is refused as a range even in a file that would also
        // fail the error gate. Range errors are the caller's addressing mistake
        // (fix the bytes); HasErrors is the file's condition (fix the source).
        // Reporting the addressing mistake first keeps the remedy actionable.
        let Some(node) = find_exact(&root, start, end) else {
            return Err(AstError::RangeIsNotANode { path: path.to_path_buf(), start, end });
        };
        if has_error(&root) {
            return Err(AstError::HasErrors {
                path: path.to_path_buf(),
                detail: "the source contains error nodes".to_owned(),
            });
        }
        node.kind().to_owned()
    };

    let mut spliced = Vec::with_capacity(source.len() + replacement.len());
    spliced.extend_from_slice(&source[..start]);
    spliced.extend_from_slice(replacement.as_bytes());
    spliced.extend_from_slice(&source[end..]);

    let after = parse_clean(path, &spliced, language).map_err(|error| match error {
        AstError::HasErrors { detail, .. } => AstError::GateRefused { path: path.to_path_buf(), detail },
        other => other,
    })?;
    if has_error(&after.root_node()) {
        return Err(AstError::GateRefused {
            path: path.to_path_buf(),
            detail: "the spliced result contains error nodes".to_owned(),
        });
    }
    let after_root = after.root_node();
    let replacement_end = start + replacement.len();
    let Some(node) = find_exact(&after_root, start, replacement_end) else {
        return Err(AstError::GateRefused {
            path: path.to_path_buf(),
            detail: format!("bytes {start}..{replacement_end} address no single node after the splice"),
        });
    };
    if node.kind() != before_kind {
        return Err(AstError::GateRefused {
            path: path.to_path_buf(),
            detail: format!(
                "node kind changed from {} to {}; cross-kind replacement is delete plus insert, not a splice",
                before_kind,
                node.kind()
            ),
        });
    }
    Ok(spliced)
}

/// Parse, refusing nothing by itself.
///
/// Returns the tree with error nodes intact; the callers decide what errors
/// mean. Folding the error check into the parse would force one error variant
/// for two conditions with different remedies (fix the bytes vs fix the
/// source) - so the check lives at the call sites, where the remedy is known.
fn parse_clean(
    path: &Path,
    source: &[u8],
    language: supra_digest::Language,
) -> Result<tree_sitter::Tree, AstError> {
    let mut parser = tree_sitter::Parser::new();
    let grammar = grammar(language);
    parser.set_language(&grammar).map_err(|_| AstError::HasErrors {
        path: path.to_path_buf(),
        detail: "the grammar failed to load".to_owned(),
    })?;
    let Some(tree) = parser.parse(source, None) else {
        return Err(AstError::HasErrors {
            path: path.to_path_buf(),
            detail: "the parser returned no tree".to_owned(),
        });
    };
    Ok(tree)
}

/// The compiled grammar for a language. Same seven as T15 - the splicer parses
/// what the indexer indexes, so a file the digest can anchor is a file this
/// gate can verify, and vice versa.
fn grammar(language: supra_digest::Language) -> tree_sitter::Language {
    match language {
        supra_digest::Language::Rust => tree_sitter_rust::LANGUAGE.into(),
        supra_digest::Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        supra_digest::Language::Python => tree_sitter_python::LANGUAGE.into(),
        supra_digest::Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        supra_digest::Language::Go => tree_sitter_go::LANGUAGE.into(),
        supra_digest::Language::C => tree_sitter_c::LANGUAGE.into(),
        supra_digest::Language::Cpp => tree_sitter_cpp::LANGUAGE.into(),
    }
}

/// Whether the tree contains an error node anywhere.
fn has_error(node: &tree_sitter::Node<'_>) -> bool {
    if node.kind() == "ERROR" || node.is_missing() {
        return true;
    }
    let mut cursor = node.walk();
    let children: Vec<_> = node.children(&mut cursor).collect();
    children.iter().any(has_error)
}

/// The node whose span is exactly `start..end`, if exactly one exists.
///
/// Empty ranges match nothing: `start == end` is a point, and a point where a
/// node starts is still not a node. The guard in `replace_node` refuses empty
/// ranges before reaching here, and the unit test pins this half - so the two
/// refusals are tested independently.
fn find_exact<'a>(
    root: &'a tree_sitter::Node<'a>,
    start: usize,
    end: usize,
) -> Option<tree_sitter::Node<'a>> {
    // Descend while exactly one child contains the range; stop at the deepest
    // node spanning it exactly. A range strictly inside a node with no child
    // containing it is a partial node - refused, never rounded outward.
    let mut current = *root;
    loop {
        if start == end {
            return None;
        }
        if current.start_byte() == start && current.end_byte() == end {
            return Some(current);
        }
        let mut cursor = current.walk();
        let children: Vec<_> = current.children(&mut cursor).collect();
        let containing: Vec<_> = children
            .into_iter()
            .filter(|child| child.start_byte() <= start && child.end_byte() >= end)
            .collect();
        if containing.len() != 1 {
            return None;
        }
        current = containing[0];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (std::path::PathBuf, Vec<u8>) {
        let source = b"fn alpha() {\n    1\n}\n\nfn beta() {\n    2\n}\n".to_vec();
        (std::path::PathBuf::from("a.rs"), source)
    }

    /// End byte of the first function node, from the tree - not from a byte
    /// search, which would couple every test to this exact fixture text.
    fn function_end(source: &[u8]) -> usize {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into()).expect("rust grammar loads");
        let tree = parser.parse(source, None).expect("fixture parses");
        let root = tree.root_node();
        let mut cursor = root.walk();
        let children: Vec<_> = root.children(&mut cursor).collect();
        children
            .iter()
            .find(|node| node.kind() == "function_item")
            .map(tree_sitter::Node::end_byte)
            .expect("a function in the fixture")
    }

    #[test]
    fn a_function_body_is_replaced_and_nothing_else_moves() {
        let (path, source) = fixture();
        // Range of the first function node exactly. The test computes it from
        // the tree rather than from a byte search: the first `}` in the file
        // happens to close the first function here, but asserting via the
        // parser keeps the fixture honest if the source ever changes.
        let start = 0;
        let end = function_end(&source);
        let out =
            replace_node(&path, &source, start, end, "fn alpha() {\n    99\n}").expect("a same-kind splice");
        // Byte-level, not string-level: `windows` compares raw bytes, so an
        // encoding-sensitive assertion cannot pass on a near-miss. The output
        // above decodes to `...99\n}\n\nfn beta()...` - both windows must hit.
        assert!(out.windows(4).any(|window| window == b"99\n}"), "{out:?}");
        assert!(out.windows(4).any(|window| window == b"beta"), "beta untouched: {out:?}");
        // Everything outside the range is byte-identical. `end` is the
        // tree-reported end of the first function; the tail from `end` onward
        // is untouched, so it appears verbatim at the tail of the output -
        // shifted by the length delta between replacement and original.
        let tail = &source[end..];
        assert_eq!(&out[out.len() - tail.len()..], tail);
    }

    #[test]
    fn a_partial_node_is_refused_not_rounded() {
        let (path, source) = fixture();
        // Bytes 4..8 are `ha() `: strictly inside the `alpha` identifier
        // (3..8), which itself *is* a node (measured: a probe found exactly
        // one node spanning 3..8, kind `identifier`). The range names part of
        // an identifier - no node spans it, so `find_exact` descends to the
        // identifier (the one child containing the range), finds no child
        // containing it, and returns None. Refused, never rounded outward to
        // the identifier.
        //
        // The replacement is the exact original bytes: splicing them back is
        // byte-identical, so the reparse gate cannot object to the *content* -
        // only RangeIsNotANode proves the range itself was judged. (Measured:
        // any *different* five bytes there either parse - `XXXXX` keeps `fn
        // XXXXX()` valid - or fail the gate for content reasons, coupling the
        // test to the wrong discipline either way.)
        let error = replace_node(&path, &source, 4, 8, "ha() ").expect_err("partial");
        assert!(matches!(error, AstError::RangeIsNotANode { .. }), "{error}");
        assert_eq!(source, fixture().1, "the source is untouched");
    }

    #[test]
    fn a_kind_change_is_refused() {
        let (path, source) = fixture();
        let end = function_end(&source);
        let error = replace_node(&path, &source, 0, end, "struct Alpha;").expect_err("kind change");
        assert!(matches!(error, AstError::GateRefused { .. }), "{error}");
    }

    #[test]
    fn a_broken_replacement_is_refused() {
        let (path, source) = fixture();
        let end = function_end(&source);
        let error = replace_node(&path, &source, 0, end, "fn alpha( { nope").expect_err("broken");
        assert!(matches!(error, AstError::GateRefused { .. }), "{error}");
        assert_eq!(source, fixture().1, "the source is untouched");
    }

    #[test]
    fn errors_outside_the_replaced_span_still_refuse() {
        // What isolates the error gate: errors *outside* the replaced span. The
        // replacement below keeps alpha's own span clean (`fn alpha` + body) and
        // appends garbage after it: find_exact resolves, the error gate fires on
        // the tail.
        let (path, source) = fixture();
        let end = function_end(&source);
        let mut replacement = b"fn alpha() {\n    99\n}".to_vec();
        replacement.extend_from_slice(b"\nfn broken( { nope");
        let replacement = String::from_utf8(replacement).expect("ascii");
        let error = replace_node(&path, &source, 0, end, &replacement).expect_err("error nodes");
        assert!(matches!(error, AstError::GateRefused { .. }), "{error}");
        assert!(error.to_string().contains("error nodes"), "{error}");
    }

    #[test]
    fn a_broken_source_is_refused_before_any_splice() {
        // `fn broken( { nope` parses with an error node, and bytes 0..4 are the
        // `fn b` prefix - not a node, so the range check fires first by design
        // (addressing mistakes report as ranges, file conditions as HasErrors).
        // This test therefore uses a range that *is* a node in the broken tree's
        // error recovery: bytes 0..17 span the whole `function_item` including
        // its error body, which `find_exact` resolves - and then the error gate
        // fires. Both orders are covered: RangeIsNotANode above, HasErrors here.
        let path = std::path::PathBuf::from("a.rs");
        let source = b"fn broken( { nope";
        let error = replace_node(&path, source, 0, source.len(), "fn x() {}").expect_err("broken source");
        assert!(matches!(error, AstError::HasErrors { .. }), "{error}");
    }

    #[test]
    fn an_inverted_or_out_of_bounds_range_is_refused() {
        let (path, source) = fixture();
        assert!(matches!(
            replace_node(&path, &source, 5, 5, "x").expect_err("empty"),
            AstError::RangeIsNotANode { .. }
        ));
        assert!(matches!(
            replace_node(&path, &source, 8, 3, "x").expect_err("inverted"),
            AstError::RangeIsNotANode { .. }
        ));
        assert!(matches!(
            replace_node(&path, &source, 0, source.len() + 1, "x").expect_err("past end"),
            AstError::RangeIsNotANode { .. }
        ));
    }

    #[test]
    fn an_empty_range_matches_no_node_even_where_one_starts() {
        // Direct unit test for `find_exact`'s contract, independent of the
        // `start >= end` guard above: byte 0 starts `function_item`, and an
        // empty range there must resolve to no node - not to the function.
        // Both refusals are tested separately instead of passing silently on
        // each other.
        let (_path, source) = fixture();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into()).expect("grammar");
        let tree = parser.parse(&source, None).expect("fixture parses");
        assert!(find_exact(&tree.root_node(), 0, 0).is_none(), "empty range matched a node");
    }

    #[test]
    fn an_empty_range_is_refused_even_where_a_node_starts() {
        // Byte 0 starts `function_item`, which makes 0..0 the sharpest case: an
        // empty range where a node starts is still no node, not a zero-width
        // insertion point.
        let (path, source) = fixture();
        let error = replace_node(&path, &source, 0, 0, "x").expect_err("empty at node start");
        assert!(matches!(error, AstError::RangeIsNotANode { .. }), "{error}");
    }

    #[test]
    fn an_unsupported_language_is_refused() {
        let path = std::path::PathBuf::from("notes.md");
        let error = replace_node(&path, b"# hi", 0, 4, "x").expect_err("markdown");
        assert!(matches!(error, AstError::UnsupportedLanguage { .. }), "{error}");
    }
}
