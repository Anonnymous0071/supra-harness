//! Syntactic rename: every eligible occurrence rewritten, or nothing.
//!
//! # What rename does
//!
//! Given a name, its defining file, the reachable candidate files with their
//! bytes, and the replacement: find every eligible occurrence via
//! [`crate::query::query_references`] and splice each one with
//! [`crate::splice::replace_node`]'s range discipline. Every splice passes the
//! reparse gate; the whole rename is atomic (all files verify, or no file is
//! returned changed).
//!
//! # What rename does not do
//!
//! Resolve shadowing, overloading, or dynamic dispatch. An inner `let open`
//! shadowing a method `open` will be rewritten alongside it - textually
//! eligible, semantically distinct. That is exactly why every result carries
//! `semantic: false` (T15's query contract): syntactic rename without LSP can
//! be wrong, so it says so on every result rather than in a footnote. The
//! caller - T24's language servers when they land, a human review until then -
//! reads the flag, not the footnote.
//!
//! # The three refusals
//!
//! `NothingToRename` when no occurrence is eligible anywhere: an empty rename
//! reporting success is how a misspelled target passes silently.
//! `WouldShadow` when the new name already heads a live declaration in any
//! touched file: silently creating a shadow changes which declaration every
//! existing reference resolves to. `GateRefused` when any single splice fails
//! its reparse: the rename stops, and no file is returned changed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::AstError;
use crate::query::query_references;

/// One file rewritten by a rename.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenamedFile {
    /// File rewritten, relative to the repository root.
    pub file: PathBuf,
    /// New bytes of the file.
    pub bytes: Vec<u8>,
    /// How many occurrences were rewritten in this file.
    pub rewritten: usize,
}

/// The outcome of a rename: every touched file, or nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenameOutcome {
    /// Files rewritten, in path order.
    pub files: Vec<RenamedFile>,
    /// Always `false`: see the module documentation.
    pub semantic: bool,
}

/// Rename `old` to `new` across the reachable candidates.
///
/// `defining_file` is where T15 says the name is defined; `graph` is the
/// import graph narrowing reachability; `candidates` maps each candidate file
/// to its current bytes (the defining file must be among them - its own
/// declaration is rewritten too). Returns the rewritten files without touching
/// the filesystem: persistence belongs to the caller (T16.6's journal
/// snapshots before any byte lands), not to a query crate.
///
/// Atomicity is structural: every splice is verified before any file is
/// returned, and the first refusal aborts the whole rename - so the caller
/// either receives all files or an error, never a prefix of the work.
///
/// # Errors
///
/// [`AstError::NothingToRename`] when no occurrence is eligible anywhere.
/// [`AstError::WouldShadow`] when `new` already heads a live declaration in a
/// touched file. [`AstError::GateRefused`] when any splice fails its reparse -
/// with no file returned changed.
pub fn rename(
    old: &str,
    new: &str,
    defining_file: &Path,
    graph: &supra_digest::DependencyGraph,
    candidates: &BTreeMap<PathBuf, Vec<u8>>,
) -> Result<RenameOutcome, AstError> {
    if old.is_empty() || new.is_empty() || old == new {
        return Err(AstError::NothingToRename { path: defining_file.to_path_buf(), old: old.to_owned() });
    }
    let candidate_refs: Vec<(&Path, &[u8])> =
        candidates.iter().map(|(path, bytes)| (path.as_path(), bytes.as_slice())).collect();
    let references = query_references(old, defining_file, graph, &candidate_refs);
    if references.is_empty() {
        return Err(AstError::NothingToRename { path: defining_file.to_path_buf(), old: old.to_owned() });
    }
    // Shadow check before any splice: `new` heading a live declaration in any
    // file we would touch means the rename would silently rebind every existing
    // reference to that declaration. Checked against the outline (harvested
    // declarations), not substring search: only a declaration shadows.
    for reference in &references {
        let Some(source) = candidates.get(&reference.file) else { continue };
        let outline = crate::query::outline(&reference.file, source).map_err(|_| AstError::GateRefused {
            path: reference.file.clone(),
            detail: "the file does not parse cleanly; refusing to rename within guesses".to_owned(),
        })?;
        if let Some(shadowed) = outline.iter().find(|entry| entry.symbol.name == *new) {
            return Err(AstError::WouldShadow {
                path: reference.file.clone(),
                old: old.to_owned(),
                new: new.to_owned(),
                shadowed: shadowed.symbol.name.clone(),
            });
        }
    }
    // Splice back to front within each file: earlier offsets stay valid while
    // later bytes move. Each occurrence is its own identifier node, replaced by
    // the new name - same kind (identifier to identifier), so the gate's
    // same-kind rule holds by construction rather than by luck.
    //
    // The sort is load-bearing only when names differ in length: same-length
    // renames shift nothing, so forward order passes the gate by accident.
    // Tests must use length-changing renames to distinguish the orders.
    let mut files = Vec::with_capacity(references.len());
    for reference in &references {
        let Some(source) = candidates.get(&reference.file) else { continue };
        let mut bytes = source.clone();
        let mut offsets = reference.occurrences.clone();
        offsets.sort_unstable_by(|left, right| right.cmp(left));
        for offset in &offsets {
            let end = offset + old.len();
            bytes = crate::splice::replace_node(&reference.file, &bytes, *offset, end, new)?;
        }
        files.push(RenamedFile { file: reference.file.clone(), rewritten: offsets.len(), bytes });
    }
    // `references` arrives in path order from `query_references`, and this loop
    // preserves it - so the sort below is redundant today. It stays for the
    // same reason T13's `keys.sort()` stays: the guarantee must not depend on
    // an upstream iterator's order, which no signature promises. A redundant
    // sort is cheap; an order-dependent guarantee is not one.
    files.sort_by(|left, right| left.file.cmp(&right.file));
    Ok(RenameOutcome { files, semantic: false })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn graph_two() -> supra_digest::DependencyGraph {
        let mut graph = supra_digest::DependencyGraph::new();
        graph.register_module(std::path::Path::new("src/store.rs"));
        graph.register_module(std::path::Path::new("src/main.rs"));
        graph.register_module(std::path::Path::new("src/other.rs"));
        graph.record_imports(std::path::Path::new("src/main.rs"), &["crate::store".to_owned()]);
        graph
    }

    fn candidates() -> BTreeMap<PathBuf, Vec<u8>> {
        let mut map = BTreeMap::new();
        map.insert(
            PathBuf::from("src/store.rs"),
            b"pub struct Store;\nimpl Store {\n    pub fn open() {}\n}\n".to_vec(),
        );
        map.insert(
            PathBuf::from("src/main.rs"),
            b"use crate::store::Store;\nfn main() { Store::open(); }\n".to_vec(),
        );
        map.insert(PathBuf::from("src/other.rs"), b"fn open() {}\n".to_vec());
        map
    }

    #[test]
    fn a_rename_rewrites_the_definition_and_the_reachable_call() {
        let graph = graph_two();
        let outcome = rename("open", "launch", std::path::Path::new("src/store.rs"), &graph, &candidates())
            .expect("rename");
        assert!(!outcome.semantic, "syntactic results must say so");
        assert_eq!(outcome.files.len(), 2, "{outcome:?}");
        let store = outcome
            .files
            .iter()
            .find(|file| file.file.as_path() == std::path::Path::new("src/store.rs"))
            .expect("store");
        assert!(store.bytes.windows(6).any(|window| window == b"launch"), "{store:?}");
        assert!(!store.bytes.windows(4).any(|window| window == b"open"), "old name survives: {store:?}");
        // The unreachable file is untouched: coincidence renamed is corruption.
        assert!(outcome.files.iter().all(|file| file.file.as_path() != std::path::Path::new("src/other.rs")));
    }

    #[test]
    fn rename_order_is_back_to_front_within_each_file() {
        // The M7 gap: every fixture renames single-occurrence names, so splice
        // order is unobservable - forward or backward, one splice cannot shift
        // another. Two occurrences of one name in one file isolate the order:
        // back-to-front keeps later offsets valid; forward shifts them, and the
        // second splice either hits the wrong bytes (silently wrong) or no node
        // (GateRefused). The test asserts both occurrences rewritten, which only
        // the correct order achieves.
        let mut graph = supra_digest::DependencyGraph::new();
        graph.register_module(std::path::Path::new("a.rs"));
        let mut files = BTreeMap::new();
        files.insert(PathBuf::from("a.rs"), b"fn ping() {}\nfn caller() { ping(); ping(); }\n".to_vec());
        // 4 bytes to 6: length-changing, so forward order shifts the second
        // occurrence and the gate refuses it. Same-length (`ping`->`pong`)
        // would pass either order and prove nothing about the sort.
        let outcome = rename("ping", "pongxx", std::path::Path::new("a.rs"), &graph, &files).expect("rename");
        assert_eq!(outcome.files.len(), 1, "{outcome:?}");
        let file = &outcome.files[0];
        assert_eq!(file.rewritten, 3, "definition plus two calls: {file:?}");
        assert!(!file.bytes.windows(4).any(|window| window == b"ping"), "stale occurrence: {file:?}");
        assert_eq!(file.bytes.windows(6).filter(|window| window == b"pongxx").count(), 3);
    }

    #[test]
    fn files_come_out_in_path_order() {
        // Defensive, not load-bearing: `query_references` sorts by file and the
        // loop preserves order, so deleting the sort below changes nothing
        // today. The test pins the output bytes against the day an upstream
        // iterator stops promising order - at which point this test, not a
        // consumer applying files positionally, is what catches it. Same class
        // as T13's M1 (the redundant sort): M10's survival is evidence about
        // the code, with the guarantee pinned rather than the line removed.
        // The defining file is deliberately last in the input order below.
        let mut graph = supra_digest::DependencyGraph::new();
        graph.register_module(std::path::Path::new("src/b.rs"));
        graph.register_module(std::path::Path::new("src/a.rs"));
        graph.register_module(std::path::Path::new("src/z.rs"));
        graph.record_imports(std::path::Path::new("src/a.rs"), &["crate::b".to_owned()]);
        graph.record_imports(std::path::Path::new("src/z.rs"), &["crate::b".to_owned()]);
        let mut files = BTreeMap::new();
        files.insert(PathBuf::from("src/b.rs"), b"pub fn ping() {}\n".to_vec());
        files.insert(PathBuf::from("src/a.rs"), b"use crate::b::ping;\nfn x() { ping(); }\n".to_vec());
        files.insert(PathBuf::from("src/z.rs"), b"use crate::b::ping;\nfn y() { ping(); }\n".to_vec());
        let outcome =
            rename("ping", "pongxx", std::path::Path::new("src/b.rs"), &graph, &files).expect("rename");
        let names: Vec<_> = outcome.files.iter().map(|file| file.file.clone()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "output order follows input order: {names:?}");
        assert_eq!(names.len(), 3, "{names:?}");
    }

    #[test]
    fn a_rename_to_a_live_name_is_refused() {
        let graph = graph_two();
        let mut files = candidates();
        files.insert(
            PathBuf::from("src/main.rs"),
            b"use crate::store::Store;\nfn launch() {}\nfn main() { Store::open(); }\n".to_vec(),
        );
        let error = rename("open", "launch", std::path::Path::new("src/store.rs"), &graph, &files)
            .expect_err("would shadow");
        assert!(matches!(error, AstError::WouldShadow { .. }), "{error}");
    }

    #[test]
    fn a_rename_of_a_missing_name_is_refused_not_empty() {
        let graph = graph_two();
        let error = rename("absent", "present", std::path::Path::new("src/store.rs"), &graph, &candidates())
            .expect_err("nothing to rename");
        assert!(matches!(error, AstError::NothingToRename { .. }), "{error}");
    }

    #[test]
    fn degenerate_requests_are_refused() {
        let graph = graph_two();
        for (old, new) in [("", "x"), ("x", ""), ("same", "same")] {
            let error = rename(old, new, std::path::Path::new("src/store.rs"), &graph, &candidates())
                .expect_err("degenerate");
            assert!(matches!(error, AstError::NothingToRename { .. }), "{old:?}->{new:?}: {error}");
        }
    }
}
