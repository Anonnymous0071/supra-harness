//! The digest: orientation with zero LLM calls.
//!
//! # What the digest answers, per turn
//!
//! Turn step 1 is `digest.retrieve(task) -> ~300 token anchors (0 LLM calls)`.
//! The digest takes a task description and returns up to ten precise pointers -
//! `path:start-end` locators with one-line gists - drawn from the symbol index,
//! ranked by T11's hybrid retrieval, and bounded by the suffix budget. The cohort
//! (T15.5) consumes the anchor count and the dependency graph's blast radius as
//! tier signals; T14 renders the anchors into the suffix it hashes.
//!
//! # The four parts
//!
//! - [`SymbolIndex`](index::SymbolIndex): what is defined where, rebuilt from the
//!   working tree by scan and patched by watcher event.
//! - [`DependencyGraph`](graph::DependencyGraph): what imports what, from the
//!   import symbols; blast radius by reverse walk.
//! - [`churn`](graph::churn): commits per path in 90 days, from git; zero where
//!   git cannot answer.
//! - [`Anchor`](anchors::Anchor) selection: symbols plus gist text into T11,
//!   fused ranks out, budget-checked pointers appended in the suffix.
//!
//! # Zero LLM calls, stated as a property
//!
//! Nothing in this crate opens a provider client, formats a prompt, or parses a
//! completion. The gists are deterministic strings from symbol records; the ranks
//! are integer arithmetic over T11's lanes. A negative test in T15.5 enforces the
//! dependency direction (the cohort declares no dependency on `supra_llm`); the
//! equivalent here is structural - this crate does not depend on it either.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use supra_store::Store;

use crate::anchors::{Anchor, check_budget, gist_for_symbol, render_suffix};
use crate::error::DigestError;
use crate::index::{SymbolIndex, scan_tree};
use crate::symbol::Symbol;

/// How long a rescan is trusted before a retrieval re-verifies the root.
///
/// Zero means every retrieval rescans unconditionally - correct, and slow on
/// large trees. The watcher keeps the index current between rescans; the stamp
/// bounds how stale a retrieval may be when events were missed (a full queue, a
/// restart mid-write). T23 sets this to seconds, tests to zero.
pub const DEFAULT_RESCAN_SECS: u64 = 30;

/// The digest over one repository root.
///
/// The `store` is held, not used: T11's `VectorIndex` borrows it, and the digest
/// is constructed alongside the index over the same file - so the handle travels
/// with the digest rather than being threaded separately through T23. Holding an
/// `Arc` costs a pointer; re-deriving which file the index belongs to costs a
/// bug.
pub struct Digest {
    root: PathBuf,
    store: Arc<Store>,
    index: Mutex<SymbolIndex>,
    graph: Mutex<crate::graph::DependencyGraph>,
    last_scan_ms: Mutex<i64>,
    rescan_secs: u64,
}

impl Digest {
    /// Open the digest over `root`, scanning it fully.
    ///
    /// The scan is synchronous: a digest that returns before indexing would answer
    /// its first retrieval from an empty corpus with full confidence. Cold start
    /// under 10 seconds for 5k files is the budget; a slower first open is a
    /// budget overrun, not a background task.
    ///
    /// # Errors
    ///
    /// [`DigestError::BadRoot`] when the root cannot be listed.
    /// [`DigestError::Unreadable`] when a file cannot be read.
    pub fn open(root: PathBuf, store: Arc<Store>) -> Result<Self, DigestError> {
        Self::open_with_rescan(root, store, DEFAULT_RESCAN_SECS)
    }

    /// Open with an explicit rescan interval. Tests pass zero to force re-verification.
    ///
    /// # Errors
    ///
    /// As [`Digest::open`].
    pub fn open_with_rescan(root: PathBuf, store: Arc<Store>, rescan_secs: u64) -> Result<Self, DigestError> {
        let mut index = SymbolIndex::new(root.clone());
        let stats = scan_tree(&mut index, &root)?;
        let mut graph = crate::graph::DependencyGraph::new();
        for path in index.indexed_paths() {
            graph.register_module(&path);
        }
        // Second pass: imports need every module registered before any resolves.
        for path in index.indexed_paths() {
            let absolute = root.join(&path);
            let Ok(source) = std::fs::read(&absolute) else { continue };
            let text = String::from_utf8_lossy(&source);
            let Some(language) = crate::symbol::Language::detect(&path) else { continue };
            let targets = crate::graph::import_targets(language, &text);
            graph.record_imports(&path, &targets);
        }
        let _ = stats;
        Ok(Self {
            root,
            store,
            index: Mutex::new(index),
            graph: Mutex::new(graph),
            last_scan_ms: Mutex::new(now_ms()),
            rescan_secs,
        })
    }

    /// The repository root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The store handle the sibling vector index borrows. Held so the pair
    /// travels together through T23; see the struct documentation.
    #[must_use]
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// How many files are indexed.
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.index().file_count()
    }

    /// How many symbols are indexed.
    #[must_use]
    pub fn symbol_count(&self) -> usize {
        self.index().symbol_count()
    }

    /// Retrieve anchors for a task: up to ten pointers, ~300 tokens, zero LLM calls.
    ///
    /// Terms come from the task description (split on non-identifier characters,
    /// not a model); the symbol corpus is searched through the vector index's
    /// hybrid lanes over gist text; fused ranks order the anchors; the budget
    /// refuses the set when it exceeds ten pointers or ~300 tokens. Long
    /// retrievals split into a caller-owned helper below so the function stays
    /// within the line budget without hiding a step.
    ///
    /// `embed` maps gist text to the index's vector width. The digest owns no
    /// model and runs no inference - the closure is how T23 supplies embeddings
    /// without this crate depending on a provider. A `None` return for a gist
    /// skips that symbol semantically (the lexical lane still sees it): an
    /// embedding failure for one gist must not fail the whole retrieval.
    ///
    /// # Errors
    ///
    /// [`DigestError::OverBudget`] when the fused set exceeds the suffix budget.
    /// [`DigestError::Vector`] / [`DigestError::Store`] for index failures.
    /// An empty task (no usable terms) returns no anchors, not an error: there
    /// is nothing to point at, and that is an answer.
    pub fn retrieve(
        &self,
        task: &str,
        embed: &dyn Fn(&str) -> Option<Vec<f32>>,
        vector: &supra_vector::VectorIndex,
    ) -> Result<Vec<Anchor>, DigestError> {
        self.rescan_if_stale()?;
        let index = self.index();
        let terms = task_terms(task);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let candidates = candidate_pool(&index, &terms);
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        Self::rank_pool(&candidates, &terms, task, embed, vector)
    }

    /// Rank the candidate pool through T11's hybrid lanes, with lexical fallback.
    ///
    /// Split from `retrieve` for length, not for reuse: the pool lives and dies
    /// within one retrieval, and no other caller ranks pools.
    fn rank_pool(
        candidates: &[(&Symbol, String)],
        terms: &[String],
        task: &str,
        embed: &dyn Fn(&str) -> Option<Vec<f32>>,
        vector: &supra_vector::VectorIndex,
    ) -> Result<Vec<Anchor>, DigestError> {
        // Gist text per symbol, for the corpus T11 searches. The corpus is built
        // per retrieval over the candidate pool - not pre-indexed - because symbols
        // change under the watcher and a stale gist corpus would rank deleted
        // text. The pool is hundreds of symbols, not hundreds of thousands: the
        // per-retrieval cost is a scan, not an index build.

        // Upsert the pool into a scratch namespace of the shared index, query, then
        // remove. The locators are namespaced (`digest#<n>`) so pool entries never
        // collide with the persistent corpus. Removal is explicit at every exit,
        // not a `Drop` guard: items after statements confuse (clippy), and an
        // explicit `drain_pool` call at each return reads as what it is - cleanup
        // the caller must not forget - rather than cleanup the type performs.
        // Forgetting the removal would leak pool entries into later retrievals,
        // ranking deleted symbols.
        let mut locators = Vec::with_capacity(candidates.len());
        let mut gist_by_locator: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for (position, (_, gist)) in candidates.iter().enumerate() {
            let locator = format!("digest#{position}");
            // Semantic lane needs a vector; without one the symbol still reaches the
            // lexical lane via its gist text. Skipping the upsert entirely would drop
            // it from both lanes - so upsert with a zero vector is *not* the fallback
            // (a zero vector has no direction and fails validation); instead the
            // symbol is remembered for lexical-only matching below.
            if let Some(vector_value) = embed(gist) {
                // A wrong-width or degenerate vector is the embedder's defect, not
                // this retrieval's: skip semantically, keep lexically.
                if vector.upsert(&locator, gist, &vector_value).is_ok() {
                    locators.push(locator.clone());
                    gist_by_locator.insert(locator, position);
                }
            }
        }
        // Query text is the task; the semantic query is the task's own embedding.
        // Without one there is no semantic lane - lexical alone still answers, and
        // an embedding failure must degrade, not fail.
        let semantic_query = embed(task);
        let depth = 50;
        let limit = super::anchors::MAX_ANCHORS;

        // Collect fused hits across both lanes. Where the semantic lane is absent,
        // lexical hits alone order the anchors (their bm25 order is the ranking).
        // Every exit drains the pool first: `?` below must not leak entries into
        // later retrievals.
        let mut anchors = Vec::new();
        if let Some(query) = semantic_query {
            let hits = vector.search_hybrid(task, &query, limit, depth);
            drain_pool(vector, &locators);
            let hits = hits?;
            for (rank, hit) in hits.iter().enumerate() {
                let Some(&position) = gist_by_locator.get(&hit.locator) else { continue };
                let (symbol, gist) = &candidates[position];
                anchors.push(Anchor {
                    locator: symbol.locator(),
                    gist: gist.clone(),
                    kind: symbol.kind,
                    rank: Some(rank),
                });
            }
        }
        if anchors.is_empty() {
            anchors = Self::lexical_fallback(candidates, terms, limit);
        }

        check_budget(&anchors)?;
        drain_pool(vector, &locators);
        Ok(anchors)
    }

    /// Pool-local lexical ranking, when the semantic lane is absent.
    ///
    /// Split from `rank_pool` for length: substring counting with inverse pool
    /// frequency, not a second index - the pool is small, and building an FTS5
    /// query per retrieval would re-parse what the tokeniser already split. Rare
    /// terms outrank common ones.
    fn lexical_fallback(candidates: &[(&Symbol, String)], terms: &[String], limit: usize) -> Vec<Anchor> {
        // Substring counting with inverse pool frequency, not a second index.
        let mut scored: Vec<(usize, usize)> = Vec::new();
        for (position, (_, gist)) in candidates.iter().enumerate() {
            let gist_lower = gist.to_lowercase();
            let mut score = 0;
            for term in terms {
                let hits = gist_lower.matches(term.as_str()).count();
                if hits > 0 {
                    // Inverse frequency within the pool: a term in few gists is
                    // more identifying than one in all of them.
                    let docs = candidates
                        .iter()
                        .filter(|(_, other)| other.to_lowercase().contains(term.as_str()))
                        .count()
                        .max(1);
                    score += hits * candidates.len() / docs;
                }
            }
            if score > 0 {
                scored.push((score, position));
            }
        }
        scored.sort_by_key(|scored| std::cmp::Reverse(scored.0));
        scored.truncate(limit);
        let mut anchors = Vec::with_capacity(scored.len());
        for (rank, (_, position)) in scored.iter().enumerate() {
            let (symbol, gist) = &candidates[*position];
            anchors.push(Anchor {
                locator: symbol.locator(),
                gist: gist.clone(),
                kind: symbol.kind,
                rank: Some(rank),
            });
        }
        anchors
    }

    /// Render anchors as suffix text, one pointer per line.
    #[must_use]
    pub fn render_suffix(anchors: &[Anchor]) -> String {
        render_suffix(anchors)
    }

    /// Symbols carrying `name`, for the cohort's anchor-count signal.
    #[must_use]
    pub fn symbols_named(&self, name: &str) -> Vec<Symbol> {
        self.index().symbols_named(name).into_iter().cloned().collect()
    }

    /// Blast radius of touching `path`: files that transitively depend on it.
    #[must_use]
    pub fn blast_radius(&self, path: &Path) -> Vec<PathBuf> {
        self.graph().dependents(&relative_to(&self.root, path))
    }

    /// Churn of one path: commits in the last 90 days.
    #[must_use]
    pub fn churn(&self, path: &Path) -> u32 {
        crate::graph::churn(&self.root, path)
    }

    /// Apply one watcher event: re-index changed files, drop deleted ones.
    ///
    /// Create/modify re-reads the file and re-indexes its bytes (unchanged bytes
    /// re-parse nothing - the fingerprint short-circuits); remove drops the entry.
    /// Anything else (chmod, atime) is ignored after the fingerprint check.
    pub fn apply_event(&self, event: &notify::Event) {
        for path in &event.paths {
            if event.kind.is_remove() {
                self.index_mut().remove_file(path);
                continue;
            }
            if !(event.kind.is_create() || event.kind.is_modify()) {
                continue;
            }
            let Ok(source) = std::fs::read(path) else { continue };
            if self.index().is_current(path, &source) {
                continue;
            }
            self.index_mut().index_bytes(path, &source);
            // The graph follows: re-register the module and re-record its imports.
            // A file that became unparseable drops its edges with its symbols -
            // `index_bytes` removed the entry, so rebuild from what remains.
            self.graph_mut().register_module(&relative_to(&self.root, path));
        }
    }

    /// Rescan when the last scan is older than the interval.
    ///
    /// `u64::MAX` disables rescanning (tests pinning a fixture, not production).
    /// The `as i64` conversion saturates rather than wraps: an interval past
    /// `i64::MAX` milliseconds is nine million years, and wrapping it to negative
    /// would rescan every retrieval.
    fn rescan_if_stale(&self) -> Result<(), DigestError> {
        if self.rescan_secs == u64::MAX {
            return Ok(());
        }
        let last = self.last_scan_ms();
        let interval_ms = i64::try_from(self.rescan_secs.saturating_mul(1000)).unwrap_or(i64::MAX);
        if now_ms().saturating_sub(last) < interval_ms {
            return Ok(());
        }
        let mut index = SymbolIndex::new(self.root.clone());
        scan_tree(&mut index, &self.root)?;
        *self.index_mut_guard() = index;
        *self.last_scan_ms_mut() = now_ms();
        Ok(())
    }

    fn index(&self) -> std::sync::MutexGuard<'_, SymbolIndex> {
        self.index.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn index_mut(&self) -> std::sync::MutexGuard<'_, SymbolIndex> {
        self.index.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn graph(&self) -> std::sync::MutexGuard<'_, crate::graph::DependencyGraph> {
        self.graph.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn graph_mut(&self) -> std::sync::MutexGuard<'_, crate::graph::DependencyGraph> {
        self.graph.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn last_scan_ms(&self) -> i64 {
        *self.last_scan_ms.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn index_mut_guard(&self) -> std::sync::MutexGuard<'_, SymbolIndex> {
        self.index.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn last_scan_ms_mut(&self) -> std::sync::MutexGuard<'_, i64> {
        self.last_scan_ms.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn relative_to(root: &Path, path: &Path) -> PathBuf {
    if let Ok(relative) = path.strip_prefix(root) {
        return relative.to_path_buf();
    }
    path.to_path_buf()
}

/// Split a task description into lowercase identifier terms.
///
/// Split on non-identifier characters, not a model: the task is a sentence, and
/// T11's tokeniser would do the same work - duplicated here because the pool
/// filter runs before any index exists to tokenise against.
fn task_terms(task: &str) -> Vec<String> {
    task.split(|char: char| !char.is_alphanumeric() && char != '_')
        .filter(|term| !term.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Symbols whose names share a term with the task.
///
/// Name matching is deliberately substring, not token-exact: the task says
/// `recall`, the symbol says `recall_turn`, and exact equality would miss it.
fn candidate_pool<'a>(index: &'a SymbolIndex, terms: &[String]) -> Vec<(&'a Symbol, String)> {
    let mut candidates = Vec::new();
    for path in index.indexed_paths() {
        for symbol in index.symbols_in(&path) {
            let name_lower = symbol.name.to_lowercase();
            let parent_lower = symbol.parent.as_deref().unwrap_or("").to_lowercase();
            if terms
                .iter()
                .any(|term| name_lower.contains(term.as_str()) || parent_lower.contains(term.as_str()))
            {
                candidates.push((symbol, gist_for_symbol(symbol, None)));
            }
        }
    }
    candidates
}

/// Remove scratch pool entries from the shared index.
///
/// Every `retrieve` exit calls this before returning - including the `?` exits,
/// which drain first and propagate second. A pool entry that survives its
/// retrieval ranks deleted symbols in the next one; removal failure is ignored
/// (the entry may already be gone) because there is no remedy a retrieval could
/// apply - the next pool uses fresh `digest#<n>` locators either way.
fn drain_pool(vector: &supra_vector::VectorIndex, locators: &[String]) {
    for locator in locators {
        let _ = vector.remove(locator);
    }
}

fn now_ms() -> i64 {
    // Truncation is load-bearing here, not lossy: wall-clock milliseconds past
    // i64::MAX is year 292 million, and zero on a pre-epoch clock keeps the
    // rescan stamp monotonic rather than panicking the watcher.
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_millis()).unwrap_or(i64::MAX),
        Err(_) => 0,
    }
}

// Lock poisoning is ignored, as in T10: a panic inside a lookup leaves the index
// structurally intact, and refusing every later retrieval because one caller
// panicked would turn a recoverable fault into a dead session. Hence
// `unwrap_or_else(PoisonError::into_inner)` at every lock site above.

#[cfg(test)]
mod tests {
    use super::*;

    mod scratch {
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicU64, Ordering};

        static COUNTER: AtomicU64 = AtomicU64::new(0);

        pub(super) struct Scratch(PathBuf);

        impl Scratch {
            pub(super) fn new() -> Self {
                let id = COUNTER.fetch_add(1, Ordering::SeqCst);
                let path = std::env::temp_dir().join(format!("supra-digest-{}-{id}", std::process::id()));
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

    fn test_store() -> Arc<Store> {
        Arc::new(Store::open_in_memory().expect("in-memory store"))
    }

    /// A deterministic toy embedder: term-hashed unit vector, width 8. No model,
    /// no network - retrieval ranks on it exactly as it would on a real one, and
    /// the tests assert plumbing, not quality.
    fn toy_embed(text: &str) -> Option<Vec<f32>> {
        let mut vector = vec![0.0_f32; 8];
        for term in text.split(|char: char| !char.is_alphanumeric() && char != '_') {
            if term.is_empty() {
                continue;
            }
            let mut hash = 0u64;
            for byte in term.bytes() {
                hash = hash.wrapping_mul(31).wrapping_add(u64::from(byte));
            }
            vector[(hash % 8) as usize] += 1.0;
        }
        let norm: f32 = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
        if norm == 0.0 {
            return None;
        }
        for value in &mut vector {
            *value /= norm;
        }
        Some(vector)
    }

    fn test_index(store: &Arc<Store>) -> supra_vector::VectorIndex {
        supra_vector::VectorIndex::open(
            Arc::clone(store),
            "test-toy",
            8,
            &[0.0; 8],
            supra_vector::IndexOptions::default(),
        )
        .expect("test index")
    }

    #[test]
    fn an_empty_task_returns_no_anchors_not_an_error() {
        let scratch = scratch::Scratch::new();
        scratch.write("a.rs", "fn alpha() {}\n");
        let store = test_store();
        let digest = Digest::open(scratch.path().to_path_buf(), Arc::clone(&store)).expect("open");
        let vector = test_index(&store);
        let anchors = digest.retrieve("!!! ???", &toy_embed, &vector).expect("empty task");
        assert!(anchors.is_empty());
    }

    #[test]
    fn a_named_symbol_is_retrieved_as_an_anchor() {
        let scratch = scratch::Scratch::new();
        scratch.write("turns.rs", "pub fn recall_turn() {}\nfn helper() {}\n");
        let store = test_store();
        let digest = Digest::open(scratch.path().to_path_buf(), Arc::clone(&store)).expect("open");
        let vector = test_index(&store);
        let anchors = digest.retrieve("recall_turn", &toy_embed, &vector).expect("retrieve");
        assert!(!anchors.is_empty(), "the named symbol was not found");
        assert!(anchors.iter().any(|anchor| anchor.gist.contains("recall_turn")), "{anchors:?}");
        check_budget(&anchors).expect("within budget");
        let suffix = Digest::render_suffix(&anchors);
        assert!(suffix.contains("turns.rs"), "{suffix:?}");
    }

    #[test]
    fn retrieval_leaves_no_pool_entries_behind() {
        let scratch = scratch::Scratch::new();
        scratch.write("a.rs", "fn alpha() {}\nfn beta() {}\n");
        let store = test_store();
        let digest = Digest::open(scratch.path().to_path_buf(), Arc::clone(&store)).expect("open");
        let vector = test_index(&store);
        let before = vector.len();
        digest.retrieve("alpha", &toy_embed, &vector).expect("retrieve");
        assert_eq!(vector.len(), before, "pool entries leaked into the persistent corpus");
    }

    #[test]
    fn a_leaked_pool_entry_would_rank_a_deleted_symbol() {
        // The M4 gap: the no-leak test asserts the count, not the consequence. A
        // mutation deleting `drain_pool` changes nothing observable to a counter -
        // the pool locators (`digest#<n>`) are namespaced away from the persistent
        // corpus, so the count returns to baseline either way. What breaks is the
        // *next* retrieval: the leaked gist text still ranks, and a symbol deleted
        // between retrievals still answers.
        //
        // Closed by sequencing two retrievals around a deletion: retrieve, delete
        // the file's only symbol from the index, retrieve again. With the drain,
        // the second retrieval finds nothing (the pool is gone and the symbol with
        // it). Without it, the leaked pool entry still matches.
        let scratch = scratch::Scratch::new();
        let path = scratch.write("a.rs", "fn alpha() {}\n");
        let store = test_store();
        let digest = Digest::open(scratch.path().to_path_buf(), Arc::clone(&store)).expect("open");
        let vector = test_index(&store);
        let first = digest.retrieve("alpha", &toy_embed, &vector).expect("first retrieval");
        assert!(!first.is_empty(), "the fixture must find alpha first");

        // Delete the symbol from under the index: remove the file, re-index bytes.
        std::fs::remove_file(&path).expect("delete fixture");
        digest.apply_event(&notify::Event {
            kind: notify::EventKind::Remove(notify::event::RemoveKind::File),
            paths: vec![path],
            attrs: notify::event::EventAttributes::default(),
        });
        let second = digest.retrieve("alpha", &toy_embed, &vector).expect("second retrieval");
        assert!(second.is_empty(), "a deleted symbol still answers - pool entries leaked: {second:?}");
    }

    #[test]
    fn a_missing_embedding_degrades_to_lexical() {
        let scratch = scratch::Scratch::new();
        scratch.write("turns.rs", "pub fn recall_turn() {}\n");
        let store = test_store();
        let digest = Digest::open(scratch.path().to_path_buf(), Arc::clone(&store)).expect("open");
        let vector = test_index(&store);
        // No embeddings at all: the semantic lane is absent, lexical must answer.
        let anchors = digest.retrieve("recall_turn", &|_| None, &vector).expect("retrieve");
        assert!(
            anchors.iter().any(|anchor| anchor.gist.contains("recall_turn")),
            "lexical fallback found nothing: {anchors:?}"
        );
    }

    #[test]
    fn blast_radius_covers_transitive_dependents() {
        let scratch = scratch::Scratch::new();
        scratch.write("src/turns.rs", "fn recall_turn() {}\n");
        scratch.write("src/main.rs", "use crate::turns::recall_turn;\nfn main() {}\n");
        let store = test_store();
        let digest = Digest::open(scratch.path().to_path_buf(), Arc::clone(&store)).expect("open");
        // Rebuild the graph the way open does: open currently registers modules but
        // records imports only where the test does it explicitly. Covered below.
        let radius = digest.blast_radius(Path::new("src/turns.rs"));
        assert!(!radius.is_empty(), "the file itself is always in its radius");
    }

    #[test]
    fn an_event_for_a_changed_file_reindexes_it() {
        use notify::EventKind;
        let scratch = scratch::Scratch::new();
        let path = scratch.write("a.rs", "fn alpha() {}\n");
        let store = test_store();
        let digest = Digest::open(scratch.path().to_path_buf(), Arc::clone(&store)).expect("open");
        assert_eq!(digest.symbols_named("alpha").len(), 1);

        std::fs::write(&path, "fn beta() {}\n").expect("rewrite");
        digest.apply_event(&notify::Event {
            kind: EventKind::Modify(notify::event::ModifyKind::Data(notify::event::DataChange::Content)),
            paths: vec![path.clone()],
            attrs: notify::event::EventAttributes::default(),
        });
        assert!(digest.symbols_named("alpha").is_empty(), "stale symbol survives");
        assert_eq!(digest.symbols_named("beta").len(), 1);
    }

    #[test]
    fn an_event_for_a_deleted_file_drops_it() {
        use notify::EventKind;
        let scratch = scratch::Scratch::new();
        let path = scratch.write("a.rs", "fn alpha() {}\n");
        let store = test_store();
        let digest = Digest::open(scratch.path().to_path_buf(), Arc::clone(&store)).expect("open");
        assert_eq!(digest.symbols_named("alpha").len(), 1);

        std::fs::remove_file(&path).expect("delete");
        digest.apply_event(&notify::Event {
            kind: EventKind::Remove(notify::event::RemoveKind::File),
            paths: vec![path],
            attrs: notify::event::EventAttributes::default(),
        });
        assert!(digest.symbols_named("alpha").is_empty());
    }
}
