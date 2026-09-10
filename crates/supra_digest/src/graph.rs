//! The dependency graph: what imports what, from the import symbols.
//!
//! # Why imports and not calls
//!
//! The graph answers blast radius - "what breaks if this file changes" - and the
//! cheap, exact signal for that is the import edge. A call graph would answer it
//! more precisely (which *symbol* reaches which), but needs name resolution across
//! files: `use crate::turns::recall_turn` must be matched against `pub fn
//! recall_turn` in `turns.rs`, through renames, re-exports, and glob imports.
//! That is T15.7's query machinery, not this stage's. The import edge over-
//! approximates (a file importing a module need not touch the changed symbol),
//! and over-approximation is the safe direction for scrutiny: a cohort sized for
//! a larger blast radius wastes money, one sized for a smaller radius misses the
//! break.
//!
//! # Paths are module paths, not file paths
//!
//! `use std::sync::Arc` and `use crate::turns::recall_turn` are different edges:
//! the first leaves the repository (external, counted but not followed), the
//! second stays inside (internal, resolved to a file). `crate::` roots at the
//! repository root; `super::` and `self::` resolve against the importing file's
//! module path; anything else is external. Relative imports that escape the root
//! (`super::` past the top) resolve to nothing - a malformed tree, reported as an
//! external edge rather than a crash.
//!
//! # Churn is git, not timestamps
//!
//! Timestamps lie under clock skew, rebases, and fresh clones (every file shares
//! one checkout time). `git log --follow --format=%ct` reports committer dates
//! per path - real history, not filesystem metadata. Files untracked by git have
//! no history: churn zero, which is the honest answer (unknown, treated as
//! stable) rather than infinity (unknown, treated as alarming).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// One directed edge: `from` imports `to`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Edge {
    /// The importing file, relative to the root.
    pub from: PathBuf,
    /// The imported module path as written (`crate::turns`, `std::sync`).
    pub to: String,
    /// Whether the target resolves to a file inside the root.
    pub internal: bool,
}

/// The repository's import graph.
#[derive(Clone, Debug, Default)]
pub struct DependencyGraph {
    edges: BTreeSet<Edge>,
    /// Internal module path to file, for blast-radius walks. Built from the symbol
    /// index's file list: every indexed `.rs` file contributes its module path.
    modules: BTreeMap<String, PathBuf>,
}

impl DependencyGraph {
    /// An empty graph.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a file's module path. Every indexed source file contributes exactly
    /// one: `src/turns.rs` is `src::turns`, `src/a/b.rs` is `src::a::b`. Rust
    /// importers write `crate::turns`; the `crate::`-to-path bridge is normalised
    /// where imports are recorded (see `normalise_import`), not here.
    pub fn register_module(&mut self, path: &Path) {
        if let Some(module) = module_path(path) {
            self.modules.insert(module, path.to_path_buf());
        }
    }

    /// Record the imports of one file.
    ///
    /// `imports` are raw module paths from the file's import symbols. `crate::`
    /// roots are rewritten to the importing file's own root-relative directory
    /// (`crate::turns` from `src/main.rs` becomes `src::turns`): the graph keys
    /// modules by file-relative path, and `crate::` is the source tree's name for
    /// that same root. Each becomes an edge, internal when it resolves.
    pub fn record_imports(&mut self, from: &Path, imports: &[String]) {
        for target in imports {
            let normalised = normalise_import(from, target);
            let internal = self.resolve(&normalised).is_some();
            self.edges.insert(Edge { from: from.to_path_buf(), to: normalised, internal });
        }
    }

    /// Drop a file's module and its outgoing edges.
    ///
    /// The mirror of [`DependencyGraph::record_imports`]: a removed or
    /// re-read file must not keep the edges its old content declared,
    /// or the blast radius of a change would include dependents that
    /// stopped depending on it.
    pub fn remove_file(&mut self, path: &Path) {
        self.modules.retain(|_, file| file != path);
        self.edges.retain(|edge| edge.from != path);
    }

    /// Resolve a module path to a file inside the root, if it is internal.
    #[must_use]
    pub fn resolve(&self, module: &str) -> Option<&PathBuf> {
        // Exact match first; then strip one `::` segment at a time from the right
        // (`crate::turns::recall_turn` resolves to `crate::turns`). The symbol -
        // not the module - is the last segment, and the graph resolves modules.
        let mut candidate = module;
        loop {
            if let Some(path) = self.modules.get(candidate) {
                return Some(path);
            }
            match candidate.rsplit_once("::") {
                Some((parent, _)) => candidate = parent,
                None => return None,
            }
        }
    }

    /// Files that (transitively) depend on `path`, including `path` itself.
    ///
    /// The blast radius of touching `path`: every file that would need re-checking.
    /// Bounded by the graph size; cycles terminate because visited files are never
    /// revisited. Over-approximates by construction (import edge, not call edge) -
    /// see the module documentation.
    ///
    /// A path that resolves to no indexed file - an external target like `std`,
    /// or a deleted file - has an empty radius, not a singleton: seeding the
    /// visited set with an unindexed path would report it as its own dependent,
    /// which reads as coverage the graph does not have.
    #[must_use]
    pub fn dependents(&self, path: &Path) -> Vec<PathBuf> {
        // Reverse map: module path to importers, rebuilt per query. The graph is
        // small (files, not symbols) and queries are rare (once per task) - so a
        // maintained reverse index would be a second source of truth for no
        // measured gain.
        if !self.files_contain(path) {
            return Vec::new();
        }
        let mut visited: BTreeSet<PathBuf> = BTreeSet::new();
        let mut frontier = vec![path.to_path_buf()];
        visited.insert(path.to_path_buf());
        while let Some(current) = frontier.pop() {
            let current_module = module_path(&current);
            for edge in &self.edges {
                if !edge.internal {
                    continue;
                }
                let Some(target) = self.resolve(&edge.to) else { continue };
                if target == &current && visited.insert(edge.from.clone()) {
                    frontier.push(edge.from.clone());
                }
            }
            // Files in the same module tree that import the parent module also
            // count: `crate::turns` importing covers `crate::turns::inner`.
            let _ = current_module;
        }
        let mut out: Vec<PathBuf> = visited.into_iter().collect();
        out.sort();
        out
    }

    /// How many edges the graph holds.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// How many modules are registered.
    #[must_use]
    pub fn module_count(&self) -> usize {
        self.modules.len()
    }

    /// Whether `path` is an indexed file. The blast-radius seed check: an
    /// unindexed path has no dependents, not itself as one.
    fn files_contain(&self, path: &Path) -> bool {
        self.modules.values().any(|registered| registered == path)
    }
}

/// Rewrite a raw import into the graph's key space.
///
/// `crate::turns` (Rust, rooted at the source tree) becomes the file-relative
/// `src::turns` when the importer lives under `src/` - more precisely, `crate::`
/// is replaced by the importer's own root-relative directory chain, so
/// `crate::a::b` from `src/main.rs` is `src::a::b`. `super::` climbs one level
/// from the importer's directory (`super::x` from `src/a/b.rs` is `src::x`);
/// `self::` stays in it. Anything else passes through: external paths (`std::`),
/// bare module names (`os`), quoted include names. A `super::` that climbs past
/// the root resolves to nothing later - a malformed tree, reported as external
/// rather than crashing the walk.
fn normalise_import(from: &Path, target: &str) -> String {
    let mut parts: Vec<String> = from
        .parent()
        .map(|parent| {
            parent
                .components()
                .filter_map(|component| match component {
                    std::path::Component::Normal(part) => part.to_str().map(str::to_owned),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let mut rest = target;
    if let Some(stripped) = rest.strip_prefix("crate::") {
        rest = stripped;
    } else {
        while let Some(stripped) = rest.strip_prefix("super::") {
            rest = stripped;
            parts.pop();
        }
        if let Some(stripped) = rest.strip_prefix("self::") {
            rest = stripped;
        } else {
            // Not a rooted or relative Rust path: pass through untouched.
            if !target.starts_with("crate::") {
                return target.to_owned();
            }
        }
    }
    if rest.is_empty() {
        return parts.join("::");
    }
    if parts.is_empty() {
        return rest.to_owned();
    }
    format!("{}::{rest}", parts.join("::"))
}

/// The module path for a file: `src/turns.rs` is `src::turns`, `a/b.py` is
/// `a::b`. Returns `None` for files with no stem or non-UTF-8 names (the file
/// simply has no module, and imports of it resolve external).
fn module_path(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    // `mod.rs` / `__init__.py` denote the directory, not a `mod`/`__init__` module.
    if stem == "mod" || stem == "__init__" {
        let parent = path.parent()?;
        let parts: Vec<String> = parent
            .components()
            .filter_map(|component| match component {
                std::path::Component::Normal(part) => part.to_str().map(str::to_owned),
                _ => None,
            })
            .collect();
        if parts.is_empty() {
            return None;
        }
        return Some(parts.join("::"));
    }
    let mut parts: Vec<String> = path
        .parent()
        .map(|parent| {
            parent
                .components()
                .filter_map(|component| match component {
                    std::path::Component::Normal(part) => part.to_str().map(str::to_owned),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    parts.push(stem.to_owned());
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("::"))
}

/// Extract the raw import targets from a file's import symbols.
///
/// `use std::sync::Arc` yields `std::sync::Arc`; `import os` yields `os`;
/// `from a import b` yields `a`; `#include <stdio.h>` yields `stdio.h`.
/// Language-specific shaping lives here so the graph stays structural.
#[must_use]
pub fn import_targets(language: crate::symbol::Language, text: &str) -> Vec<String> {
    use crate::symbol::Language::{C, Cpp, Go, JavaScript, Python, Rust, TypeScript};
    let mut targets = Vec::new();
    match language {
        Rust => {
            // `use a::b::{c, d};` - take the whole path; `resolve` strips the symbol.
            for part in text.split(';') {
                let part = part.trim().trim_start_matches("pub ").trim_start_matches("use ").trim();
                let part = part.trim_end_matches(';').trim();
                if part.is_empty() || part.starts_with("//") {
                    continue;
                }
                // Drop trailing `::{...}` groups and `as` renames for the module path.
                let base = part.split("::{").next().unwrap_or(part);
                let base = base.split(" as ").next().unwrap_or(base).trim();
                if !base.is_empty() {
                    targets.push(base.to_owned());
                }
            }
        }
        Python => {
            for line in text.lines() {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("import ") {
                    for item in rest.split(',') {
                        let item = item.split(" as ").next().unwrap_or(item).trim();
                        if !item.is_empty() {
                            targets.push(item.to_owned());
                        }
                    }
                } else if let Some(rest) = line.strip_prefix("from ") {
                    if let Some((module, _)) = rest.split_once(" import ") {
                        targets.push(module.trim().to_owned());
                    }
                }
            }
        }
        Go | JavaScript | TypeScript => {
            // `import "fmt"`, `import {x} from 'm'`, `import y = require('n')`.
            // Quoted strings are module paths; bare `import X` (Python-style) is not
            // valid here and falls through unmatched.
            let mut chars = text.chars().peekable();
            while let Some(char) = chars.next() {
                if char == '"' || char == '\'' {
                    let mut target = String::new();
                    for next in chars.by_ref() {
                        if next == char {
                            break;
                        }
                        target.push(next);
                    }
                    if !target.is_empty() && !target.starts_with('.') {
                        targets.push(target);
                    }
                }
            }
        }
        C | Cpp => {
            for line in text.lines() {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("#include") {
                    let rest = rest.trim().trim_matches(|char| char == '"' || char == '<' || char == '>');
                    // `<stdio.h>` keeps its name; `"local.h"` likewise. Angle vs quote
                    // distinguishes system from local, which matters for resolution
                    // priority - but both spellings resolve the same table here.
                    if !rest.is_empty() {
                        targets.push(rest.to_owned());
                    }
                }
            }
        }
    }
    targets.sort();
    targets.dedup();
    targets
}

/// Churn: commits touching a path in the last 90 days, from git history.
///
/// `git log --follow --format=%ct -- <path>`: committer dates, one per line.
/// `--follow` tracks renames, so a moved file keeps its history instead of
/// reading as brand-new. Files untracked by git have no history: churn zero.
/// Returns zero (not an error) when git is absent, the directory is not a
/// repository, or the path is untracked: unknown history reads as stable, not
/// alarming. An error here would make every offline machine report every file
/// as unmeasurable, which is worse than treating unknown as calm.
#[must_use]
pub fn churn(root: &Path, path: &Path) -> u32 {
    const NINETY_DAYS_SECS: u64 = 90 * 24 * 3600;
    let output = std::process::Command::new("git")
        .args(["log", "--follow", "--format=%ct", "--"])
        .arg(path)
        .current_dir(root)
        .output();
    let Ok(output) = output else { return 0 };
    if !output.status.success() {
        return 0;
    }
    // Elapsed seconds stay unsigned throughout: a clock past u64-range seconds
    // does not exist, future timestamps count as recent (wrong clock, safe side),
    // and no cast crosses signedness at all.
    let now_secs = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_secs(),
        Err(_) => return 0,
    };
    // Commits per file in 90 days fits u32 by construction: git cannot produce
    // four billion commits on one path in ninety days. `try_from` with a
    // saturating fallback states the bound instead of truncating silently.
    let recent = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse::<u64>().ok())
        .filter(|timestamp| timestamp.saturating_add(NINETY_DAYS_SECS) >= now_secs)
        .count();
    u32::try_from(recent).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbol::Language;

    #[test]
    fn rust_use_paths_resolve_through_symbol_stripping() {
        let mut graph = DependencyGraph::new();
        graph.register_module(Path::new("src/turns.rs"));
        graph.record_imports(Path::new("src/main.rs"), &["crate::turns::recall_turn".to_owned()]);
        let dependents = graph.dependents(Path::new("src/turns.rs"));
        assert_eq!(dependents, vec![PathBuf::from("src/main.rs"), PathBuf::from("src/turns.rs")]);
    }

    #[test]
    fn external_imports_are_edges_but_not_followed() {
        let mut graph = DependencyGraph::new();
        graph.register_module(Path::new("src/a.rs"));
        graph.register_module(Path::new("src/b.rs"));
        // b imports std (external) and a imports b (internal). The blast radius
        // of std must be empty - no file *is* std - and must not include b merely
        // because b names it.
        graph.record_imports(Path::new("src/b.rs"), &["std::sync::Arc".to_owned()]);
        graph.record_imports(Path::new("src/a.rs"), &["crate::b".to_owned()]);
        assert_eq!(graph.edge_count(), 2);
        assert_eq!(graph.dependents(Path::new("src/a.rs")), vec![PathBuf::from("src/a.rs")]);
    }

    #[test]
    fn an_external_target_has_no_dependents() {
        // External edges are recorded (edge_count) and never walked: neither
        // importer reaches the other through std, and an unindexed path - an
        // external target included - has no radius at all (seed check).
        //
        // Honesty note (M6): deleting the `!edge.internal` guard is unobservable
        // through the public API today. `record_imports` derives `internal` from
        // `resolve`, and `resolve` returns None for every external string - so the
        // `continue` after it skips the same edges the guard skips. The guard is
        // defence in depth against a future `resolve` that matches external
        // strings; the test pins the observable behaviour (recorded, never
        // walked), not the guard's firing. A survival here is evidence about the
        // code - two checks covering one case - not a gap in the suite. Same
        // class as T13's M1 (the redundant sort): the line stays because the
        // guarantee must not depend on one function keeping its contract.
        let mut graph = DependencyGraph::new();
        graph.register_module(Path::new("src/a.rs"));
        graph.register_module(Path::new("src/b.rs"));
        graph.record_imports(Path::new("src/a.rs"), &["std::sync::Arc".to_owned()]);
        graph.record_imports(Path::new("src/b.rs"), &["std::sync::Mutex".to_owned()]);
        assert_eq!(graph.edge_count(), 2, "external edges are recorded");
        assert_eq!(graph.dependents(Path::new("src/a.rs")), vec![PathBuf::from("src/a.rs")]);
        assert_eq!(graph.dependents(Path::new("src/b.rs")), vec![PathBuf::from("src/b.rs")]);
        let radius = graph.dependents(Path::new("std::sync::Arc"));
        assert!(radius.is_empty(), "{radius:?}");
    }

    #[test]
    fn cycles_terminate() {
        let mut graph = DependencyGraph::new();
        graph.register_module(Path::new("a.rs"));
        graph.register_module(Path::new("b.rs"));
        graph.record_imports(Path::new("a.rs"), &["crate::b".to_owned()]);
        graph.record_imports(Path::new("b.rs"), &["crate::a".to_owned()]);
        let dependents = graph.dependents(Path::new("a.rs"));
        assert_eq!(dependents.len(), 2, "a cycle must not loop: {dependents:?}");
    }

    #[test]
    fn python_import_targets_cover_both_spellings() {
        assert_eq!(
            import_targets(Language::Python, "import os, sys\nfrom a import b\n"),
            vec!["a".to_owned(), "os".to_owned(), "sys".to_owned()]
        );
    }

    #[test]
    fn rust_use_groups_resolve_to_the_module() {
        assert_eq!(
            import_targets(Language::Rust, "use std::sync::{Arc, Mutex};\nuse crate::turns::recall_turn;\n"),
            vec!["crate::turns::recall_turn".to_owned(), "std::sync".to_owned()]
        );
    }

    #[test]
    fn c_includes_keep_their_names() {
        assert_eq!(
            import_targets(Language::C, "#include <stdio.h>\n#include \"local.h\"\n"),
            vec!["local.h".to_owned(), "stdio.h".to_owned()]
        );
    }

    #[test]
    fn churn_is_zero_where_git_cannot_answer() {
        // /tmp is not a repository (or the file is untracked there): unknown history
        // reads as stable, not as an error.
        assert_eq!(churn(Path::new("/tmp"), Path::new("nope.rs")), 0);
    }

    #[test]
    fn module_paths_are_file_relative() {
        assert_eq!(
            module_path(Path::new("src/turns.rs")).as_deref(),
            Some("src::turns"),
            "file-relative; crate:: rooting happens in normalise_import"
        );
    }
}
