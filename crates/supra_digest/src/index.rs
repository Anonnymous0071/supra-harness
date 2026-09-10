//! The index: what is defined where, rebuilt from the working tree.
//!
//! # Why an in-memory map over a persistent table
//!
//! The corpus is the source of truth for its own text - T11's FTS5 table is
//! contentless for exactly this reason - and the symbol index is a *derivative*
//! of that text. Persisting a derivative beside its source creates the
//! invalidation problem twice: once for the vectors (solved by the watcher
//! re-upserting on change) and once for the symbols. So the index lives in
//! memory, rebuilt by [`crate::Digest::open`] and patched by [`crate::Digest::apply_event`].
//! A restart rescans; a rescan is a cold start under 10 seconds for 5k files
//! (the budget), not a migration.
//!
//! # What the index answers
//!
//! Two queries, both keyed lookups, never traversals:
//!
//! - `symbols_named(name)`: what declarations carry this name, in which files.
//!   The task names `recall_turn`; the index answers `src/turns.rs:118-145`.
//! - `symbols_in(path)`: what one file defines, for blast-radius estimates.
//!
//! The cohort (T15.5) consumes both: anchor count feeds tier estimation, and the
//! file's symbol list bounds the blast radius of touching it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::DigestError;
use crate::parse::{ParseOutcome, parse_file};
use crate::symbol::{Fingerprint, Symbol};

/// One file's indexed state.
#[derive(Clone, Debug)]
struct FileEntry {
    /// Fingerprint of the whole file at index time. Compared on rescan to skip
    /// unchanged files without re-parsing: blake3 over megabytes is cheaper than
    /// tree-sitter over megabytes, and the comparison is exact, not heuristic.
    file_fingerprint: Fingerprint,
    /// Symbols harvested from the file.
    symbols: Vec<Symbol>,
}

/// The in-memory symbol index over one repository root.
#[derive(Clone, Debug, Default)]
pub struct SymbolIndex {
    root: PathBuf,
    files: HashMap<PathBuf, FileEntry>,
}

impl SymbolIndex {
    /// An empty index over `root`. Reads nothing; [`crate::Digest::open`] fills it.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root, files: HashMap::new() }
    }

    /// The repository root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// How many files are indexed.
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// How many symbols are indexed.
    #[must_use]
    pub fn symbol_count(&self) -> usize {
        self.files.values().map(|entry| entry.symbols.len()).sum()
    }

    /// Index one file's bytes, replacing its previous entry.
    ///
    /// Unsupported languages are skipped without error: coverage is per-file, and
    /// an index that refuses a whole scan for one markdown file has the wrong
    /// failure domain. Files with syntax errors are likewise skipped, with the
    /// path remembered by the caller through [`ParseOutcome`].
    pub fn index_bytes(&mut self, path: &Path, source: &[u8]) {
        let relative = relative_to(&self.root, path);
        match parse_file(&relative, source) {
            Ok(ParseOutcome::Clean(symbols)) => {
                self.files.insert(
                    relative,
                    FileEntry { file_fingerprint: *blake3::hash(source).as_bytes(), symbols },
                );
            }
            Ok(ParseOutcome::HasErrors { .. }) | Err(_) => {
                // A file that cannot be parsed contributes no symbols. It also keeps
                // no stale entry: a file that *became* unparseable must stop answering
                // queries, not answer them from its last good parse.
                self.files.remove(&relative);
            }
        }
    }

    /// Drop one file's entry. Returns whether it was there.
    pub fn remove_file(&mut self, path: &Path) -> bool {
        self.files.remove(&relative_to(&self.root, path)).is_some()
    }

    /// Every declaration carrying `name`, in path order.
    #[must_use]
    pub fn symbols_named(&self, name: &str) -> Vec<&Symbol> {
        let mut found: Vec<&Symbol> = self
            .files
            .values()
            .flat_map(|entry| entry.symbols.iter())
            .filter(|symbol| symbol.name == name)
            .collect();
        found.sort_by(|left, right| left.path.cmp(&right.path));
        found
    }

    /// Every symbol in one file, in byte order.
    #[must_use]
    pub fn symbols_in(&self, path: &Path) -> Vec<&Symbol> {
        let relative = relative_to(&self.root, path);
        let Some(entry) = self.files.get(&relative) else { return Vec::new() };
        entry.symbols.iter().collect()
    }

    /// Whether the file's bytes match the indexed fingerprint.
    ///
    /// The watcher's fast path: an event for a file whose bytes did not change
    /// (a chmod, an atime touch, a save that wrote identical bytes) re-parses
    /// nothing. The comparison is exact - blake3, not mtime - because mtimes lie
    /// under clock skew and some editors preserve them.
    #[must_use]
    pub fn is_current(&self, path: &Path, source: &[u8]) -> bool {
        let relative = relative_to(&self.root, path);
        self.files
            .get(&relative)
            .is_some_and(|entry| entry.file_fingerprint == *blake3::hash(source).as_bytes())
    }

    /// Every indexed path, sorted. For the watcher reconciling a rescan against
    /// the filesystem: paths present here but absent there were deleted.
    #[must_use]
    pub fn indexed_paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = self.files.keys().cloned().collect();
        paths.sort();
        paths
    }
}

/// Relativise `path` against `root`. Absolute paths under the root become
/// relative; already-relative paths pass through; anything else is used as-is -
/// the index keys on what it is given, and the watcher always gives root-joined
/// absolutes.
fn relative_to(root: &Path, path: &Path) -> PathBuf {
    if let Ok(relative) = path.strip_prefix(root) {
        return relative.to_path_buf();
    }
    path.to_path_buf()
}

/// Walk the root and index every supported file.
///
/// Skips hidden directories (`.git`, `.hg`, `.jj`, `target`, `node_modules`) by
/// name, not by marker file: a repository with an unconventional layout still has
/// conventional directory names for its build artefacts, and descending into
/// `target/` would index generated code as if it were source. Symlinks are not
/// followed, for the same reason T12.5's L4 distrusts names: a link pointing at
/// `/etc` would index the operating system.
///
/// # Errors
///
/// [`DigestError::BadRoot`] when the root cannot be listed.
/// [`DigestError::Unreadable`] when a file cannot be read. The scan stops at the
/// first unreadable file rather than skipping it: see the variant's documentation.
pub fn scan_tree(index: &mut SymbolIndex, root: &Path) -> Result<ScanStats, DigestError> {
    if !root.is_dir() {
        return Err(DigestError::BadRoot { root: root.to_path_buf(), detail: "not a directory".to_owned() });
    }
    let mut stats = ScanStats::default();
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        let entries = std::fs::read_dir(&directory)
            .map_err(|error| DigestError::BadRoot { root: directory.clone(), detail: error.to_string() })?;
        // Sorted for determinism: `read_dir` order is filesystem-dependent, and the
        // index feeds an order-sensitive pipeline (T14's ledger). Two scans of the
        // same tree must produce the same symbol order.
        let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
        entries.sort_by_key(std::fs::DirEntry::path);
        for entry in entries {
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|error| DigestError::Unreadable { path: path.clone(), detail: error.to_string() })?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                    if is_skipped_dir(name) {
                        continue;
                    }
                }
                directories.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            stats.files_seen += 1;
            // Unsupported languages are outside coverage by design: counted, not failed.
            if crate::symbol::Language::detect(&path).is_none() {
                stats.skipped_unsupported += 1;
                continue;
            }
            let source = std::fs::read(&path)
                .map_err(|error| DigestError::Unreadable { path: path.clone(), detail: error.to_string() })?;
            stats.files_read += 1;
            let before = index.symbol_count();
            index.index_bytes(&path, &source);
            let after = index.symbol_count();
            // `index_bytes` replaces the file's entry; the delta is this file's symbols
            // only when the file is new. Count precisely instead: re-derive from the entry.
            let _ = (before, after);
            stats.symbols_indexed = index.symbol_count();
        }
    }
    Ok(stats)
}

/// Directories never descended into.
fn is_skipped_dir(name: &str) -> bool {
    // Hidden directories (`.git` and friends) plus the two build-artefact names that
    // dwarf every other directory in a working tree. `target/` alone can hold more
    // generated files than the repository holds source; indexing it would answer
    // "what is defined where" with build output.
    name.starts_with('.') || name == "target" || name == "node_modules"
}

/// What a scan covered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanStats {
    /// Files encountered, of any kind.
    pub files_seen: usize,
    /// Files read (supported language, not a symlink).
    pub files_read: usize,
    /// Files skipped for lack of a grammar.
    pub skipped_unsupported: usize,
    /// Symbols indexed in total after the scan.
    pub symbols_indexed: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rooted() -> (tempfile_like::Scratch, SymbolIndex) {
        let scratch = tempfile_like::Scratch::new();
        let index = SymbolIndex::new(scratch.path().to_path_buf());
        (scratch, index)
    }

    /// A minimal scratch directory without pulling `tempfile` into the workspace:
    /// unique per test via process id and a counter, removed on drop.
    mod tempfile_like {
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicU64, Ordering};

        static COUNTER: AtomicU64 = AtomicU64::new(0);

        pub(super) struct Scratch(PathBuf);

        impl Scratch {
            pub(super) fn new() -> Self {
                let id = COUNTER.fetch_add(1, Ordering::SeqCst);
                let path =
                    std::env::temp_dir().join(format!("supra-digest-index-{}-{id}", std::process::id()));
                let _ = std::fs::remove_dir_all(&path);
                std::fs::create_dir_all(&path).expect("scratch directory");
                Self(path)
            }

            pub(super) fn path(&self) -> &Path {
                &self.0
            }

            pub(super) fn write(&self, name: &str, content: &str) -> PathBuf {
                let path = self.0.join(name);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).expect("parent directories");
                }
                std::fs::write(&path, content).expect("write fixture");
                path
            }
        }

        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn a_scan_indexes_supported_files_and_counts_the_rest() {
        let (scratch, mut index) = rooted();
        scratch.write("a.rs", "fn alpha() {}\n");
        scratch.write("b.py", "def beta():\n    pass\n");
        scratch.write("notes.md", "# not code\n");
        scratch.write("dir/c.ts", "export function gamma() {}\n");

        let stats = scan_tree(&mut index, scratch.path()).expect("scan");
        assert_eq!(stats.files_read, 3, "{stats:?}");
        assert_eq!(stats.skipped_unsupported, 1, "{stats:?}");
        assert_eq!(index.file_count(), 3);
        assert_eq!(index.symbols_named("alpha").len(), 1);
        assert_eq!(index.symbols_named("beta").len(), 1);
        assert_eq!(index.symbols_named("gamma").len(), 1);
    }

    #[test]
    fn hidden_and_build_directories_are_never_descended_into() {
        let (scratch, mut index) = rooted();
        scratch.write("src/a.rs", "fn real() {}\n");
        scratch.write(".git/objects/fake.rs", "fn phantom_git() {}\n");
        scratch.write("target/debug/fake.rs", "fn phantom_target() {}\n");
        scratch.write("node_modules/pkg/fake.js", "function phantom_node() {}\n");

        scan_tree(&mut index, scratch.path()).expect("scan");
        assert!(index.symbols_named("real").len() == 1);
        assert!(index.symbols_named("phantom_git").is_empty(), "indexed .git");
        assert!(index.symbols_named("phantom_target").is_empty(), "indexed target/");
        assert!(index.symbols_named("phantom_node").is_empty(), "indexed node_modules/");
    }

    #[test]
    fn symlinks_are_never_followed() {
        let (scratch, mut index) = rooted();
        scratch.write("real.rs", "fn real() {}\n");
        #[cfg(unix)]
        std::os::unix::fs::symlink(scratch.path().join("real.rs"), scratch.path().join("link.rs"))
            .expect("symlink");
        scan_tree(&mut index, scratch.path()).expect("scan");
        // The link itself is skipped; the target is indexed once, under its own name.
        assert_eq!(index.symbols_named("real").len(), 1);
    }

    #[test]
    fn a_broken_file_contributes_nothing_and_keeps_nothing_stale() {
        let (scratch, mut index) = rooted();
        let path = scratch.write("a.rs", "fn good() {}\n");
        scan_tree(&mut index, scratch.path()).expect("scan");
        assert_eq!(index.symbols_named("good").len(), 1);

        std::fs::write(&path, "fn broken( { nope").expect("break it");
        scan_tree(&mut index, scratch.path()).expect("rescan");
        assert!(index.symbols_named("good").is_empty(), "stale symbols survive a breaking edit");
    }

    #[test]
    fn an_unchanged_file_is_current_and_a_touched_one_is_not() {
        let (scratch, mut index) = rooted();
        let path = scratch.write("a.rs", "fn alpha() {}\n");
        scan_tree(&mut index, scratch.path()).expect("scan");
        let source = std::fs::read(&path).expect("read");
        assert!(index.is_current(&path, &source));
        assert!(!index.is_current(&path, b"fn alpha() {}\nfn beta() {}\n"));
    }

    #[test]
    fn symbols_named_returns_path_order_and_symbols_in_returns_byte_order() {
        let (scratch, mut index) = rooted();
        scratch.write("b.rs", "fn dup() {}\nfn second() {}\n");
        scratch.write("a.rs", "fn dup() {}\n");
        scan_tree(&mut index, scratch.path()).expect("scan");

        let named: Vec<_> = index.symbols_named("dup").iter().map(|symbol| symbol.path.clone()).collect();
        assert_eq!(named, vec![PathBuf::from("a.rs"), PathBuf::from("b.rs")]);

        let in_file: Vec<_> =
            index.symbols_in(Path::new("b.rs")).iter().map(|symbol| symbol.name.clone()).collect();
        assert_eq!(in_file, vec!["dup".to_owned(), "second".to_owned()]);
    }

    #[test]
    fn removing_a_file_drops_its_symbols() {
        let (scratch, mut index) = rooted();
        let path = scratch.write("a.rs", "fn gone() {}\n");
        scan_tree(&mut index, scratch.path()).expect("scan");
        assert_eq!(index.symbols_named("gone").len(), 1);
        std::fs::remove_file(&path).expect("delete");
        assert!(index.remove_file(&path));
        assert!(index.symbols_named("gone").is_empty());
        assert!(!index.remove_file(&path), "second removal reports absence");
    }

    #[test]
    fn a_missing_root_is_bad_root_not_an_empty_index() {
        let (_, mut index) = rooted();
        let error = scan_tree(&mut index, Path::new("/nonexistent/supra-digest-probe")).expect_err("no root");
        assert!(matches!(error, DigestError::BadRoot { .. }), "{error}");
        assert_eq!(index.file_count(), 0, "a failed scan indexes nothing");
    }

    #[test]
    fn indexed_paths_are_sorted_for_deterministic_reconciliation() {
        let (scratch, mut index) = rooted();
        scratch.write("z.rs", "fn z() {}\n");
        scratch.write("a.rs", "fn a() {}\n");
        scan_tree(&mut index, scratch.path()).expect("scan");
        let paths = index.indexed_paths();
        assert_eq!(paths, vec![PathBuf::from("a.rs"), PathBuf::from("z.rs")]);
    }
}
