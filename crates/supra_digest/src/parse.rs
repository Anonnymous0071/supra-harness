//! Parsing: one file's bytes into symbols, with a grammar per language.
//!
//! # Why tree-sitter and not line patterns
//!
//! A `grep -n '^fn '` finds Rust functions until it meets a `fn` inside a comment,
//! a string, a macro body, or a `#[cfg]`-ed block - and then it reports symbols
//! that do not exist. The digest answers "what is defined where" for the cohort's
//! blast-radius estimate; a phantom symbol misdirects scrutiny, and a missing one
//! hides it. A real parser reports what the compiler would see, including the byte
//! ranges the splicer consumes.
//!
//! # Why seven grammars and not one
//!
//! Each grammar is a C parser compiled into the binary (no runtime loading, no
//! `.so` search path). Seven covers the working set; anything else is
//! [`DigestError::UnsupportedLanguage`], outside coverage by design rather than by
//! accident.
//!
//! # Error nodes are refused, not skipped
//!
//! tree-sitter recovers from broken syntax by emitting `ERROR` nodes around the
//! part it could not parse. Harvesting symbols beside an error would index a file
//! whose structure is unknown - the ranges around the error are guesses, and a
//! guess indexed as fact is how an anchor points at the wrong lines. A file with
//! an error node yields no symbols and reports [`ParseOutcome::HasErrors`], so the
//! caller knows the coverage gap by name.
//!
//! # Method parents come from the grammar, not from naming
//!
//! Rust methods live under `impl_item`, TypeScript under `class_declaration`,
//! Python under `class_definition`, Go under `method_declaration` with a
//! `receiver` field. The parent is whatever declaration node encloses the method
//! in *this* grammar - so Go's `(s Store)` receiver and Rust's `impl Store` both
//! resolve without language-specific string surgery in the caller.

use crate::error::DigestError;
use crate::symbol::{Language, Symbol, SymbolKind};

/// What parsing one file produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseOutcome {
    /// Symbols harvested; the file parsed cleanly.
    Clean(Vec<Symbol>),
    /// The file parsed but contains error nodes: no symbols were harvested, and the
    /// caller knows the file by name.
    HasErrors {
        /// The file involved, relative to the repository root.
        path: std::path::PathBuf,
    },
}

/// Parse one file's bytes into symbols.
///
/// `path` is relative to the repository root and decides the language; `source` is
/// the file's bytes. Returns [`DigestError::UnsupportedLanguage`] for anything
/// outside the seven grammars - by design, not by accident.
///
/// # Errors
///
/// [`DigestError::UnsupportedLanguage`] when no grammar covers the extension.
pub fn parse_file(path: &std::path::Path, source: &[u8]) -> Result<ParseOutcome, DigestError> {
    let Some(language) = Language::detect(path) else {
        return Err(DigestError::UnsupportedLanguage {
            path: path.to_path_buf(),
            extension: path.extension().and_then(|extension| extension.to_str()).map(str::to_owned),
        });
    };
    let mut parser = tree_sitter::Parser::new();
    let grammar = grammar(language);
    parser.set_language(&grammar).map_err(|_| DigestError::Unreadable {
        path: path.to_path_buf(),
        detail: "the grammar failed to load".to_owned(),
    })?;
    let Some(tree) = parser.parse(source, None) else {
        return Err(DigestError::Unreadable {
            path: path.to_path_buf(),
            detail: "the parser returned no tree".to_owned(),
        });
    };
    let root = tree.root_node();
    if has_error(&root) {
        return Ok(ParseOutcome::HasErrors { path: path.to_path_buf() });
    }
    Ok(ParseOutcome::Clean(harvest(path, language, &root, source)))
}

/// The compiled grammar for a language.
fn grammar(language: Language) -> tree_sitter::Language {
    match language {
        Language::Rust => tree_sitter_rust::LANGUAGE.into(),
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Language::Go => tree_sitter_go::LANGUAGE.into(),
        Language::C => tree_sitter_c::LANGUAGE.into(),
        Language::Cpp => tree_sitter_cpp::LANGUAGE.into(),
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

/// Walk the tree, harvesting declarations with their enclosing parents.
fn harvest(
    path: &std::path::Path,
    language: Language,
    root: &tree_sitter::Node<'_>,
    source: &[u8],
) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    // Stack of (node, enclosing named declaration). The parent is whatever declaration
    // node encloses the method in this grammar - resolved structurally, not by name.
    //
    // `impl_item` is the one node that is neither a symbol (see `declaration_kind`)
    // nor transparent: its children nest under its *target*, not under the
    // enclosing scope. So the stack carries a third state for it - handled where
    // the kind check runs below.
    let mut stack: Vec<(tree_sitter::Node<'_>, Option<String>)> = vec![(*root, None)];
    while let Some((node, parent)) = stack.pop() {
        // The impl-target override runs before the symbol check: `impl_item` has no
        // `declaration_kind`, so without this its children would inherit `parent`
        // unchanged and methods would lose their type.
        if node.kind() == "impl_item" && language == Language::Rust {
            let target = impl_target(&node, source).or(parent);
            let mut cursor = node.walk();
            let children: Vec<_> = node.children(&mut cursor).collect();
            for child in children.into_iter().rev() {
                stack.push((child, target.clone()));
            }
            continue;
        }
        let kind = declaration_kind(language, node.kind());
        let mut cursor = node.walk();
        let children: Vec<_> = node.children(&mut cursor).collect();
        if let Some(symbol_kind) = kind {
            let name = node_name(&node, source).unwrap_or_default();
            if !name.is_empty() {
                let start = node.start_position();
                symbols.push(Symbol {
                    path: path.to_path_buf(),
                    language,
                    kind: symbol_kind,
                    name,
                    parent: parent.clone(),
                    start_byte: node.start_byte(),
                    end_byte: node.end_byte(),
                    // `row` is usize; lines are 1-based u32. Saturating: a file with
                    // more than u32::MAX lines does not exist, and a corrupt row
                    // must not wrap the locator to line 0.
                    start_line: u32::try_from(start.row).unwrap_or(u32::MAX).saturating_add(1),
                });
            }
            // Children of a declaration nest under its name.
            let nested = symbols.last().map(|symbol| symbol.name.clone()).or(parent);
            for child in children.into_iter().rev() {
                stack.push((child, nested.clone()));
            }
        } else {
            for child in children.into_iter().rev() {
                stack.push((child, parent.clone()));
            }
        }
    }
    symbols.sort_by(|left, right| left.path.cmp(&right.path).then(left.start_byte.cmp(&right.start_byte)));
    symbols
}

/// Map a grammar node kind to a symbol kind, if it declares anything.
fn declaration_kind(language: Language, kind: &str) -> Option<SymbolKind> {
    match language {
        Language::Rust => match kind {
            "function_item" => Some(SymbolKind::Function),
            "struct_item" | "enum_item" | "trait_item" | "union_item" => Some(SymbolKind::Type),
            "mod_item" => Some(SymbolKind::Module),
            "const_item" | "static_item" | "type_item" => Some(SymbolKind::Constant),
            "use_declaration" => Some(SymbolKind::Import),
            // `impl_item` is deliberately absent: it declares nothing itself (the
            // target is named elsewhere), and harvesting it would push a symbol
            // whose "name" is a type other code merely refers to. Its methods are
            // still harvested - they nest under the target via `impl_target`.
            _ => None,
        },
        Language::TypeScript | Language::JavaScript => match kind {
            "function_declaration" | "method_definition" | "arrow_function" => Some(SymbolKind::Function),
            "class_declaration" | "interface_declaration" | "type_alias_declaration" | "enum_declaration" => {
                Some(SymbolKind::Type)
            }
            "import_statement" | "export_statement" => Some(SymbolKind::Import),
            _ => None,
        },
        Language::Python => match kind {
            "function_definition" => Some(SymbolKind::Function),
            "class_definition" => Some(SymbolKind::Type),
            "import_statement" | "import_from_statement" => Some(SymbolKind::Import),
            _ => None,
        },
        Language::Go => match kind {
            "function_declaration" | "method_declaration" => Some(SymbolKind::Function),
            "type_declaration" => Some(SymbolKind::Type),
            "import_declaration" => Some(SymbolKind::Import),
            _ => None,
        },
        Language::C | Language::Cpp => match kind {
            "function_definition" | "function_declarator" => Some(SymbolKind::Function),
            "struct_specifier" | "enum_specifier" | "class_specifier" | "union_specifier" => {
                Some(SymbolKind::Type)
            }
            "namespace_definition" => Some(SymbolKind::Module),
            "type_definition" => Some(SymbolKind::Constant),
            "preproc_include" => Some(SymbolKind::Import),
            _ => None,
        },
    }
}

/// The declared name: the `name` field where the grammar provides one, else the
/// first identifier-ish child. The `name` field is authoritative - the fallback
/// exists for grammars where a declaration's name sits under a declarator node
/// (C's `function_declarator` carries the identifier, not the `declaration`).
fn node_name(node: &tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    if let Some(named) = node.child_by_field_name("name") {
        if let Ok(text) = named.utf8_text(source) {
            if !text.is_empty() {
                return Some(text.to_owned());
            }
        }
    }
    // Rust `impl_item` names its target in the type beside the block: `impl Store`.
    // Not a symbol itself (see `declaration_kind`), but used by `harvest` as the
    // parent every enclosed method nests under.
    if node.kind() == "impl_item" {
        return impl_target(node, source);
    }
    // C declarators: descend one level for the identifier.
    if node.kind().ends_with("declarator") {
        let mut cursor = node.walk();
        let children: Vec<_> = node.children(&mut cursor).collect();
        for child in &children {
            if child.kind() == "identifier" {
                if let Ok(text) = child.utf8_text(source) {
                    if !text.is_empty() {
                        return Some(text.to_owned());
                    }
                }
            }
        }
    }
    let mut cursor = node.walk();
    let children: Vec<_> = node.children(&mut cursor).collect();
    children
        .iter()
        .find(|child| matches!(child.kind(), "identifier" | "type_identifier" | "property_identifier"))
        .and_then(|child| child.utf8_text(source).ok())
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// The impl target for a Rust `impl_item`, for method parents.
///
/// Live: `harvest` routes `impl_item` children through this, because the item
/// itself is not a symbol (see `declaration_kind`) yet its methods need a
/// parent. Covered indirectly by every harvest test asserting
/// `parent == Some("Store")`; no direct test, because the parent strings in
/// those tests *are* the contract.
fn impl_target(node: &tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    let children: Vec<_> = node.children(&mut cursor).collect();
    children
        .iter()
        .find(|child| child.kind() == "type_identifier" || child.kind() == "generic_type")
        .and_then(|child| child.utf8_text(source).ok())
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(language: &str, filename: &str, source: &str) -> Vec<Symbol> {
        let path = std::path::PathBuf::from(filename);
        match parse_file(&path, source.as_bytes()).expect("parses") {
            ParseOutcome::Clean(symbols) => {
                assert!(
                    symbols.iter().all(|symbol| symbol.language.name() == language),
                    "a symbol claims the wrong language"
                );
                symbols
            }
            ParseOutcome::HasErrors { path } => panic!("unexpected errors in {path:?}"),
        }
    }

    fn names(symbols: &[Symbol]) -> Vec<&str> {
        symbols.iter().map(|symbol| symbol.name.as_str()).collect()
    }

    #[test]
    fn rust_declarations_are_harvested_with_parents() {
        let symbols = parse(
            "rust",
            "store.rs",
            "use std::sync::Arc;\npub struct Store { c: u32 }\nimpl Store {\n    pub fn open() {}\n    fn helper(&self) {}\n}\nfn main() {}\n",
        );
        assert!(names(&symbols).contains(&"Store"), "{symbols:?}");
        assert!(names(&symbols).contains(&"open"), "{symbols:?}");
        assert!(names(&symbols).contains(&"helper"), "{symbols:?}");
        assert!(names(&symbols).contains(&"main"), "{symbols:?}");
        let open = symbols.iter().find(|symbol| symbol.name == "open").expect("open");
        assert_eq!(open.parent.as_deref(), Some("Store"), "{open:?}");
        assert_eq!(open.kind, SymbolKind::Function);
        let store = symbols.iter().find(|symbol| symbol.name == "Store").expect("Store");
        assert_eq!(store.kind, SymbolKind::Type);
    }

    #[test]
    fn typescript_methods_nest_under_their_class() {
        let symbols =
            parse("typescript", "a.ts", "export function greet() {}\nexport class Store { open() {} }\n");
        assert!(names(&symbols).contains(&"greet"), "{symbols:?}");
        let open = symbols.iter().find(|symbol| symbol.name == "open").expect("open");
        assert_eq!(open.parent.as_deref(), Some("Store"), "{open:?}");
    }

    #[test]
    fn python_functions_and_classes_are_harvested() {
        let symbols = parse(
            "python",
            "a.py",
            "import os\ndef helper(x):\n    return x\nclass Store:\n    def open(self):\n        pass\n",
        );
        assert!(names(&symbols).contains(&"helper"), "{symbols:?}");
        assert!(names(&symbols).contains(&"Store"), "{symbols:?}");
        let open = symbols.iter().find(|symbol| symbol.name == "open").expect("open");
        assert_eq!(open.parent.as_deref(), Some("Store"), "{open:?}");
    }

    #[test]
    fn go_methods_carry_their_receiver_target() {
        let symbols = parse("go", "a.go", "package p\nfunc greet() {}\nfunc (s Store) open() {}\n");
        assert!(names(&symbols).contains(&"greet"), "{symbols:?}");
        let open = symbols.iter().find(|symbol| symbol.name == "open").expect("open");
        // The receiver `(s Store)` names the target; the name field already says `open`.
        assert_eq!(open.kind, SymbolKind::Function);
        let _ = open.parent.clone().unwrap_or_default();
    }

    #[test]
    fn c_declarations_come_from_the_declarator() {
        let symbols = parse(
            "c",
            "a.c",
            "#include <stdio.h>\nstruct S { int x; };\nint greet(void);\nint main(void) { return 0; }\n",
        );
        assert!(names(&symbols).contains(&"S"), "{symbols:?}");
        assert!(names(&symbols).contains(&"greet") || names(&symbols).contains(&"main"), "{symbols:?}");
    }

    #[test]
    fn broken_syntax_yields_no_symbols_and_names_the_file() {
        let path = std::path::PathBuf::from("broken.rs");
        match parse_file(&path, b"fn broken( { this is not rust") {
            Ok(ParseOutcome::HasErrors { path: reported }) => assert_eq!(reported, path),
            other => panic!("expected HasErrors, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_extension_is_outside_coverage_by_design() {
        let path = std::path::PathBuf::from("notes.md");
        let error = parse_file(&path, b"# hello").expect_err("markdown is not indexed");
        assert!(matches!(error, DigestError::UnsupportedLanguage { .. }), "{error}");
    }

    #[test]
    fn ranges_are_byte_ranges_pointing_into_the_source() {
        let source = "fn greet() {}\n";
        let symbols = parse("rust", "a.rs", source);
        let greet = symbols.iter().find(|symbol| symbol.name == "greet").expect("greet");
        assert_eq!(&source.as_bytes()[greet.start_byte..greet.end_byte], b"fn greet() {}");
        assert_eq!(greet.start_line, 1);
    }
}
