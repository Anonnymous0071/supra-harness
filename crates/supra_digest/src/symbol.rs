//! What a symbol is: one named declaration at a byte range in a file.
//!
//! # Why a flat record, not a tree
//!
//! The digest answers "what is defined where" and "what did the task name". Both
//! are keyed lookups over names, not traversals: the caller holds a task
//! description, extracts terms, and asks which symbols those terms name. A tree
//! would buy nesting queries nobody issues - "methods of this class" is answered
//! by filtering on `parent`, which is a field, not a pointer.
//!
//! # Ranges are byte ranges, not line ranges
//!
//! A `path:start-end` locator names bytes, and T15.7's `replace_node` splices
//! bytes. Line numbers drift with every edit above them; byte ranges are what the
//! parser reports and what the splicer consumes, so the locator carries what both
//! ends already agree on.
//!
//! # Identity is content, not position
//!
//! [`Symbol::fingerprint`] hashes the symbol's own bytes (blake3). A rename changes
//! the name but keeps the body; a move changes the path but keeps both. The
//! fingerprint tells the watcher which of the three happened: same fingerprint at a
//! new path is a move, same path with a new fingerprint is an edit, neither is a
//! delete-plus-add.

use std::path::PathBuf;

/// Which language a file was parsed as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    /// Rust (`.rs`).
    Rust,
    /// TypeScript and TSX (`.ts`, `.tsx`, `.mts`, `.cts`).
    TypeScript,
    /// Python (`.py`).
    Python,
    /// JavaScript and JSX (`.js`, `.jsx`, `.mjs`, `.cjs`).
    JavaScript,
    /// Go (`.go`).
    Go,
    /// C (`.c`, `.h`).
    C,
    /// C++ (`.cpp`, `.cc`, `.cxx`, `.hpp`, `.hh`, `.hxx`).
    Cpp,
}

impl Language {
    /// All seven, in stage-map order.
    pub const ALL: [Self; 7] =
        [Self::Rust, Self::TypeScript, Self::Python, Self::JavaScript, Self::Go, Self::C, Self::Cpp];

    /// Short name, as used in diagnostics and the index.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
            Self::Python => "python",
            Self::JavaScript => "javascript",
            Self::Go => "go",
            Self::C => "c",
            Self::Cpp => "cpp",
        }
    }

    /// Detect from a file extension. Refuses rather than guessing: an unknown
    /// extension is outside coverage by design, and defaulting it to any real
    /// language would parse the file against the wrong grammar - which reports
    /// symbols that do not exist.
    #[must_use]
    pub fn detect(path: &std::path::Path) -> Option<Self> {
        match path.extension()?.to_str()? {
            "rs" => Some(Self::Rust),
            "ts" | "tsx" | "mts" | "cts" => Some(Self::TypeScript),
            "py" => Some(Self::Python),
            "js" | "jsx" | "mjs" | "cjs" => Some(Self::JavaScript),
            "go" => Some(Self::Go),
            "c" | "h" => Some(Self::C),
            "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => Some(Self::Cpp),
            _ => None,
        }
    }
}

/// What kind of declaration a symbol names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SymbolKind {
    /// A callable unit: `function_item`, `function_declaration`, `function_definition`,
    /// `method_definition`, `method_declaration`.
    Function,
    /// A nominal type: `struct_item`, `class_declaration`, `class_definition`,
    /// `type_declaration`, `interface_declaration`, `type_alias_declaration`,
    /// `enum_item`, `enum_declaration`, `enum_specifier`, `trait_item`.
    Type,
    /// A module or namespace: `mod_item`, `namespace_definition`.
    Module,
    /// A file-level constant, static, or type alias value: `const_item`,
    /// `static_item`, `type_item`.
    Constant,
    /// An import or export the dependency graph reads: `use_declaration`,
    /// `import_statement`, `import_from_statement`, `import_declaration`,
    /// `preproc_include`, `export_statement`.
    Import,
}

impl SymbolKind {
    /// Short name, as used in diagnostics and the index.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Type => "type",
            Self::Module => "module",
            Self::Constant => "constant",
            Self::Import => "import",
        }
    }
}

/// One named declaration in one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    /// File containing the declaration, relative to the repository root.
    pub path: PathBuf,
    /// What language the file parsed as.
    pub language: Language,
    /// What kind of declaration.
    pub kind: SymbolKind,
    /// The declared name, exactly as written.
    pub name: String,
    /// Enclosing declaration's name, if nested (a method's class, a function's impl
    /// target). Flat lookup key, not a tree pointer.
    pub parent: Option<String>,
    /// Byte offset of the declaration's first byte.
    pub start_byte: usize,
    /// Byte offset one past the declaration's last byte.
    pub end_byte: usize,
    /// 1-based line of the declaration's first byte.
    pub start_line: u32,
}

/// Content fingerprint: blake3 over the symbol's own bytes.
///
/// A 32-byte digest, hex-rendered where shown. Compared, never parsed: two symbols
/// with equal fingerprints hold equal bytes, whatever their names claim.
pub type Fingerprint = [u8; 32];

impl Symbol {
    /// Byte length of the declaration.
    #[must_use]
    pub fn len_bytes(&self) -> usize {
        self.end_byte.saturating_sub(self.start_byte)
    }

    /// Whether the declaration is empty. Never true for a parsed symbol - the parser
    /// reports ranges covering at least the name - so this is a corruption check on
    /// hand-built fixtures, not a state real parses reach.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.end_byte <= self.start_byte
    }

    /// The `path:start-end` locator T11 indexes and T15.7 splices.
    #[must_use]
    pub fn locator(&self) -> String {
        format!("{}:{}-{}", self.path.display(), self.start_byte, self.end_byte)
    }

    /// Fingerprint the symbol's bytes within `source`.
    ///
    /// Hashes the byte range, not the name: a rename keeps the fingerprint, an edit
    /// changes it. Out-of-range symbols hash the empty string rather than panicking -
    /// a corrupt index must report, never abort the watcher.
    #[must_use]
    pub fn fingerprint(&self, source: &[u8]) -> Fingerprint {
        let bytes = source.get(self.start_byte..self.end_byte).unwrap_or(&[]);
        *blake3::hash(bytes).as_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_detect_their_languages_and_nothing_else() {
        for (file, expected) in [
            ("a.rs", Some(Language::Rust)),
            ("a.ts", Some(Language::TypeScript)),
            ("a.tsx", Some(Language::TypeScript)),
            ("a.mts", Some(Language::TypeScript)),
            ("a.py", Some(Language::Python)),
            ("a.js", Some(Language::JavaScript)),
            ("a.jsx", Some(Language::JavaScript)),
            ("a.mjs", Some(Language::JavaScript)),
            ("a.go", Some(Language::Go)),
            ("a.c", Some(Language::C)),
            ("a.h", Some(Language::C)),
            ("a.cpp", Some(Language::Cpp)),
            ("a.hpp", Some(Language::Cpp)),
            ("a.md", None),
            ("a.json", None),
            ("Makefile", None),
            ("a", None),
        ] {
            assert_eq!(Language::detect(std::path::Path::new(file)), expected, "{file}");
        }
    }

    #[test]
    fn language_names_round_trip() {
        for language in Language::ALL {
            assert!(!language.name().is_empty());
        }
        let mut names: Vec<&str> = Language::ALL.iter().map(|language| language.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Language::ALL.len(), "two languages share a name");
    }

    #[test]
    fn a_locator_names_bytes_not_lines() {
        let symbol = Symbol {
            path: PathBuf::from("src/turns.rs"),
            language: Language::Rust,
            kind: SymbolKind::Function,
            name: "recall_turn".to_owned(),
            parent: None,
            start_byte: 118,
            end_byte: 145,
            start_line: 9,
        };
        assert_eq!(symbol.locator(), "src/turns.rs:118-145");
        assert_eq!(symbol.len_bytes(), 27);
        assert!(!symbol.is_empty());
    }

    #[test]
    fn a_fingerprint_follows_content_not_position() {
        let source = b"fn recall_turn() {} fn recall_turn() {}";
        let first = Symbol {
            path: PathBuf::from("a.rs"),
            language: Language::Rust,
            kind: SymbolKind::Function,
            name: "recall_turn".to_owned(),
            parent: None,
            start_byte: 0,
            end_byte: 19,
            start_line: 1,
        };
        let second = Symbol { start_byte: 20, end_byte: 39, ..first.clone() };
        assert_eq!(first.fingerprint(source), second.fingerprint(source));
    }
}
