//! Outline and query: what the file declares, and who refers to what.
//!
//! # Outline is the digest's harvest, reused
//!
//! T15's `parse_file` already turns bytes into symbols with byte ranges; the
//! outline of a file *is* that symbol list, rendered as an indented tree by
//! parent. Reimplementing the harvest here would give two harvests that can
//! disagree about what a file declares - so this module calls T15 and formats,
//! owning only the rendering. What the outline adds over the raw list is
//! nesting made visible: methods under their type, functions at top level,
//! imports grouped last.
//!
//! # Query resolves references through imports, syntactically
//!
//! "Who calls `recall_turn`?" has two halves: which files can *see* the name
//! (import edges, T15's graph), and which of those actually *mention* it
//! (identifier occurrences in the tree). The first half narrows the search;
//! the second confirms it. Both are syntactic: no type resolution, no overload
//! disambiguation, no dynamic dispatch - which is exactly why every result is
//! flagged `semantic: false`.
//!
//! # Why `semantic: false` is a field, not a footnote
//!
//! The architecture document states it plainly: syntactic rename without LSP
//! can be wrong under shadowing or overloading. A boolean the caller must
//! read is harder to ignore than a sentence the caller must remember - and a
//! downstream stage (T24's language servers, when they land) can flip the
//! flag it computes rather than re-derive the whole result shape.

use std::path::Path;

use crate::error::AstError;

/// One outline entry: a symbol with its depth.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineEntry {
    /// The symbol, as T15 harvested it.
    pub symbol: supra_digest::Symbol,
    /// Nesting depth: top-level declarations at 0, methods at 1.
    pub depth: usize,
}

/// The outline of one file: its declarations as an indented list.
///
/// Calls T15's harvest and derives depth from parents: a symbol with no parent
/// sits at 0, a symbol whose parent names another symbol in the same file sits
/// one deeper than that parent. A parent naming nothing in the file (an `impl`
/// target from another module, a re-exported name) leaves the depth at 0 -
/// depth describes nesting *within this file*, not across the repository.
///
/// # Errors
///
/// As T15's `parse_file`: [`AstError::UnsupportedLanguage`] outside the seven
/// grammars. Broken files yield no outline - same rule, same reason: ranges
/// around an error are guesses, and an outline of guesses misdirects.
pub fn outline(path: &Path, source: &[u8]) -> Result<Vec<OutlineEntry>, AstError> {
    let relative = path.to_path_buf();
    let symbols = match supra_digest::parse_file(&relative, source) {
        Ok(supra_digest::ParseOutcome::Clean(symbols)) => symbols,
        Ok(supra_digest::ParseOutcome::HasErrors { .. }) => {
            return Err(AstError::HasErrors {
                path: path.to_path_buf(),
                detail: "the source contains error nodes".to_owned(),
            });
        }
        Err(error) => {
            return Err(match error {
                supra_digest::DigestError::UnsupportedLanguage { path, extension } => {
                    AstError::UnsupportedLanguage { path, extension }
                }
                other => AstError::HasErrors { path: path.to_path_buf(), detail: other.to_string() },
            });
        }
    };
    // Depth by parent linkage within this file, computed iteratively: a symbol
    // whose parent names a harvested symbol sits one deeper than the shallowest
    // such symbol. Iterative because depth propagates down chains (a method in
    // an impl in a module): one pass cannot settle a grandchild.
    let mut depths = vec![0_usize; symbols.len()];
    let mut changed = true;
    while changed {
        changed = false;
        for index in 0..symbols.len() {
            let parent = symbols[index].parent.clone();
            if let Some(parent) = parent {
                // Shallowest harvested symbol with the parent's name, excluding
                // the symbol itself (a type named like its own parent is a
                // sibling coincidence, not nesting).
                let mut best: Option<usize> = None;
                for (other, candidate) in symbols.iter().enumerate() {
                    if other != index && candidate.name == parent {
                        best = Some(best.map_or(depths[other], |depth| depth.min(depths[other])));
                    }
                }
                if let Some(parent_depth) = best {
                    if depths[index] < parent_depth + 1 {
                        depths[index] = parent_depth + 1;
                        changed = true;
                    }
                }
            }
        }
    }
    Ok(symbols.into_iter().zip(depths).map(|(symbol, depth)| OutlineEntry { symbol, depth }).collect())
}

/// One reference: a file that can see a name and mentions it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    /// File containing the reference, relative to the repository root.
    pub file: std::path::PathBuf,
    /// Byte offsets of the mentioning identifiers.
    pub occurrences: Vec<usize>,
    /// Always `false` today: syntactic matching cannot see shadowing or
    /// overloading. The field exists so the claim is read, not remembered -
    /// and so T24 can flip what it computes without reshaping the result.
    pub semantic: bool,
}

/// Who refers to `name`: files that import its module and mention it.
///
/// `graph` narrows (only files with an import edge toward the defining file),
/// the tree confirms (identifier occurrences in each candidate). Files outside
/// the graph's reach are not consulted: unindexed files cannot import, and an
/// unindexed importer is a coverage gap the digest owns, not a silence this
/// query fills by guessing.
///
/// `defining_file` is the file T15 says defines the name; `candidates` are the
/// files to check, with their bytes. Returns one entry per file that mentions
/// the name at least once, in path order.
///
/// Occurrences are identifier-byte offsets found by walking the tree for
/// `identifier`-kind nodes with matching text - not substring search, which
/// would match `recall_turntable` for `recall_turn`. The defining occurrence
/// itself is included when the defining file is among the candidates: the
/// definition *is* a reference for rename purposes (it must be rewritten too).
#[must_use]
pub fn query_references(
    name: &str,
    defining_file: &Path,
    graph: &supra_digest::DependencyGraph,
    candidates: &[(&Path, &[u8])],
) -> Vec<Reference> {
    let mut out = Vec::new();
    for (path, source) in candidates {
        // Reachability first: the file must import the defining file's module,
        // or *be* the defining file. Anything else cannot name the symbol
        // through the module system - a textual match there is coincidence, and
        // coincidence renamed is corruption.
        let reachable = *path == defining_file
            || graph.dependents(defining_file).iter().any(|dependent| dependent.as_path() == *path);
        if !reachable {
            continue;
        }
        let Some(language) = supra_digest::Language::detect(path) else { continue };
        let mut parser = tree_sitter::Parser::new();
        let grammar = match language {
            supra_digest::Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            supra_digest::Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            supra_digest::Language::Python => tree_sitter_python::LANGUAGE.into(),
            supra_digest::Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            supra_digest::Language::Go => tree_sitter_go::LANGUAGE.into(),
            supra_digest::Language::C => tree_sitter_c::LANGUAGE.into(),
            supra_digest::Language::Cpp => tree_sitter_cpp::LANGUAGE.into(),
        };
        if parser.set_language(&grammar).is_err() {
            continue;
        }
        let Some(tree) = parser.parse(source, None) else { continue };
        let mut occurrences = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if matches!(node.kind(), "identifier" | "type_identifier" | "property_identifier")
                && node.utf8_text(source).is_ok_and(|text| text == name)
            {
                occurrences.push(node.start_byte());
            }
            let mut cursor = node.walk();
            let children: Vec<_> = node.children(&mut cursor).collect();
            stack.extend(children);
        }
        if !occurrences.is_empty() {
            // Sorted: tree-walk order is LIFO (stack), not byte order, and an
            // unsorted offset list would splice front-to-back downstream.
            occurrences.sort_unstable();
            out.push(Reference { file: (*path).to_path_buf(), occurrences, semantic: false });
        }
    }
    out.sort_by(|left, right| left.file.cmp(&right.file));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_outline_nests_methods_under_their_type() {
        let path = std::path::PathBuf::from("store.rs");
        let source = b"use std::sync::Arc;\npub struct Store { c: u32 }\nimpl Store {\n    pub fn open() {}\n}\nfn main() {}\n";
        let entries = outline(&path, source).expect("outline");
        let open = entries.iter().find(|entry| entry.symbol.name == "open").expect("open");
        assert_eq!(open.depth, 1, "{open:?}");
        let main = entries.iter().find(|entry| entry.symbol.name == "main").expect("main");
        assert_eq!(main.depth, 0, "{main:?}");
    }

    #[test]
    fn a_broken_file_has_no_outline() {
        let path = std::path::PathBuf::from("a.rs");
        let error = outline(&path, b"fn broken( { nope").expect_err("broken");
        assert!(matches!(error, AstError::HasErrors { .. }), "{error}");
    }

    #[test]
    fn references_require_reachability_not_just_text() {
        // Two files mention `open`; only the importer is reachable from the
        // defining file. The coincidental mention is not renamed.
        let mut graph = supra_digest::DependencyGraph::new();
        graph.register_module(std::path::Path::new("src/store.rs"));
        graph.register_module(std::path::Path::new("src/main.rs"));
        graph.register_module(std::path::Path::new("src/other.rs"));
        graph.record_imports(std::path::Path::new("src/main.rs"), &["crate::store".to_owned()]);
        let defining = std::path::Path::new("src/store.rs");
        let main = std::path::Path::new("src/main.rs");
        let other = std::path::Path::new("src/other.rs");
        let main_src = b"use crate::store::Store;\nfn main() { Store::open(); }\n";
        let other_src = b"fn open() {}\n";
        let refs = query_references(
            "open",
            defining,
            &graph,
            &[(main, main_src.as_slice()), (other, other_src.as_slice())],
        );
        assert_eq!(refs.len(), 1, "{refs:?}");
        assert_eq!(refs[0].file, main);
        assert!(!refs[0].semantic, "syntactic results must say so");
    }

    #[test]
    fn substring_names_do_not_match() {
        // `recall_turntable` must not match a query for `recall_turn`: the walk
        // compares whole identifier text, not substrings.
        let mut graph = supra_digest::DependencyGraph::new();
        graph.register_module(std::path::Path::new("a.rs"));
        let defining = std::path::Path::new("a.rs");
        let source = b"fn recall_turntable() {}\nfn recall_turn() {}\n";
        let refs = query_references("recall_turn", defining, &graph, &[(defining, source.as_slice())]);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].occurrences.len(), 1, "{refs:?}");
    }

    #[test]
    fn references_come_out_in_path_order() {
        // The M12 gap: every fixture passed candidates already in path order,
        // so deleting the file sort changed nothing observable. Candidates in
        // reverse-alphabetical order isolate it: output must still be path
        // order regardless of input order.
        let mut graph = supra_digest::DependencyGraph::new();
        graph.register_module(std::path::Path::new("src/b.rs"));
        graph.register_module(std::path::Path::new("src/a.rs"));
        graph.register_module(std::path::Path::new("src/z.rs"));
        graph.record_imports(std::path::Path::new("src/a.rs"), &["crate::b".to_owned()]);
        graph.record_imports(std::path::Path::new("src/z.rs"), &["crate::b".to_owned()]);
        let defining = std::path::Path::new("src/b.rs");
        let b_src = b"pub fn ping() {}\n";
        let a_src = b"use crate::b::ping;\nfn x() { ping(); }\n";
        let z_src = b"use crate::b::ping;\nfn y() { ping(); }\n";
        // Reverse-alphabetical candidate order: z, b, a.
        let refs = query_references(
            "ping",
            defining,
            &graph,
            &[
                (std::path::Path::new("src/z.rs"), z_src.as_slice()),
                (defining, b_src.as_slice()),
                (std::path::Path::new("src/a.rs"), a_src.as_slice()),
            ],
        );
        let names: Vec<_> = refs.iter().map(|reference| reference.file.clone()).collect();
        assert_eq!(
            names,
            vec![
                std::path::PathBuf::from("src/a.rs"),
                std::path::PathBuf::from("src/b.rs"),
                std::path::PathBuf::from("src/z.rs"),
            ],
            "{names:?}"
        );
    }

    #[test]
    fn occurrences_come_out_in_byte_order() {
        // The M9 gap: tree-walk order is LIFO, not byte order, and no test
        // asserted the sort - so deleting it changed nothing observable. Three
        // occurrences in one file isolate the order: sorted is ascending,
        // walk order is not guaranteed to be.
        let mut graph = supra_digest::DependencyGraph::new();
        graph.register_module(std::path::Path::new("a.rs"));
        let defining = std::path::Path::new("a.rs");
        let source = b"fn ping() {}\nfn a() { ping(); }\nfn b() { ping(); }\nfn c() { ping(); }\n";
        let refs = query_references("ping", defining, &graph, &[(defining, source.as_slice())]);
        assert_eq!(refs.len(), 1);
        let offsets = &refs[0].occurrences;
        assert!(offsets.len() >= 3, "{refs:?}");
        let mut sorted = offsets.clone();
        sorted.sort_unstable();
        assert_eq!(offsets, &sorted, "walk order leaked through: {offsets:?}");
    }
}
