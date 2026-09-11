//! The index against a real SQLite file.
//!
//! The unit tests in the crate cover the algorithms with no storage under them. These cover the
//! parts that only exist once there is a file: the schema's constraints, the transaction that
//! keeps the resident tiers agreeing with the rows, both lanes' SQL, and the claim the whole
//! design rests on - that a code scan followed by an exact rerank returns what an exhaustive
//! scan would have.

// An integration test is its own crate, so the library's `cfg(test)` allowances do not reach it.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::cast_precision_loss,
    reason = "test assertions, and a recall figure is a ratio"
)]

use std::path::PathBuf;
use std::sync::Arc;

use supra_store::Store;
use supra_vector::{CorpusSignal, IndexOptions, VectorError, VectorIndex, codes};

const DIMS: usize = 128;
const MODEL: &str = "test-embed-v1";

// ------------------------------------------------------------------ fixtures

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("supra-vector-{name}"));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch directory");
        Self(path)
    }

    fn db(&self) -> PathBuf {
        self.0.join("store.db")
    }

    fn store(&self) -> Arc<Store> {
        Arc::new(Store::open(self.db()).expect("open store"))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn index(store: Arc<Store>) -> VectorIndex {
    VectorIndex::open(store, MODEL, DIMS, &[0.0; DIMS], IndexOptions::default()).expect("open index")
}

struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        self.0 ^ (self.0 >> 31)
    }

    fn unit(&mut self) -> f32 {
        let bits = u16::try_from((self.next_u64() >> 48) & 0xFFFF).unwrap_or(0);
        f32::from(bits) / 65_536.0 - 0.5
    }

    /// Four uniforms summed: an Irwin-Hall approximation of a normal, which a Gaussian mixture
    /// needs and a single uniform draw is not.
    fn normalish(&mut self) -> f32 {
        self.unit() + self.unit() + self.unit() + self.unit()
    }
}

/// Standard deviation of `normalish`: four uniforms each with std `1/sqrt(12)`.
const NORMALISH_STD: f32 = 0.577_350_3;

fn normalise(vector: &mut [f32]) {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in vector.iter_mut() {
            *value /= norm;
        }
    }
}

/// A Gaussian mixture whose clusters actually dominate its noise.
///
/// The scaling is the whole point. An earlier version of this generator normalised each centroid
/// and then added per-component noise of std 0.32 against a centroid component of
/// `1/sqrt(dims)`, nine times the signal at 768 dimensions. That produces noise with a faint
/// direction rather than a clustered corpus: every pairwise similarity collapses onto one value,
/// so no method can retrieve anything and every method reports a plausible-looking failure. Two
/// of this stage's measurements were invalidated by it.
///
/// Here the per-component noise is `spread / sqrt(dims)`, so `||noise||` is about `spread`
/// against a unit centroid and the intra-cluster cosine lands near `1 / sqrt(1 + spread^2)`.
/// [`CorpusSignal`] is asserted before any recall claim is made over it.
struct Mixture {
    centroids: Vec<f32>,
    clusters: usize,
    noise_scale: f32,
}

impl Mixture {
    fn new(clusters: usize, spread: f32, seed: u64) -> Self {
        let mut rng = Rng(seed);
        let root = (DIMS as f32).sqrt();
        let bias: Vec<f32> = (0..DIMS).map(|_| rng.normalish() * 0.5 / (NORMALISH_STD * root)).collect();

        let mut centroids = vec![0.0_f32; clusters * DIMS];
        for centroid in centroids.chunks_exact_mut(DIMS) {
            for (value, offset) in centroid.iter_mut().zip(&bias) {
                *value = rng.normalish() / (NORMALISH_STD * root) + offset;
            }
            normalise(centroid);
        }
        Self { centroids, clusters, noise_scale: spread / (NORMALISH_STD * root) }
    }

    /// Draws from the same centroids as every other call on this mixture, so a query is near
    /// something rather than near nothing.
    fn draw(&self, count: usize, seed: u64) -> Vec<Vec<f32>> {
        let mut rng = Rng(seed);
        (0..count)
            .map(|_| {
                let pick = usize::try_from(rng.next_u64()).unwrap_or(0) % self.clusters;
                let centroid = &self.centroids[pick * DIMS..][..DIMS];
                let mut vector: Vec<f32> =
                    centroid.iter().map(|base| base + rng.normalish() * self.noise_scale).collect();
                normalise(&mut vector);
                vector
            })
            .collect()
    }
}

fn simple(marker: usize) -> Vec<f32> {
    let mut vector = vec![0.0_f32; DIMS];
    vector[marker % DIMS] = 1.0;
    vector
}

// ------------------------------------------------------------------ schema and configuration

#[test]
fn opening_creates_the_schema_and_freezes_the_configuration() {
    let scratch = Scratch::new("open");
    let store = scratch.store();
    let index = index(Arc::clone(&store));

    assert_eq!(index.meta().dims, DIMS);
    assert_eq!(index.meta().model, MODEL);
    assert_eq!(index.meta().threshold, vec![0.0; DIMS]);
    assert_eq!(store.component_version(supra_vector::COMPONENT).expect("version"), 2);
}

#[test]
fn the_component_ledger_records_the_version_rather_than_the_files_user_version() {
    // T10's `user_version` is one slot and its core schema owns it. If this crate wrote there
    // instead, opening a store with both would make each stage believe the other had regressed.
    let scratch = Scratch::new("ledger");
    let store = scratch.store();
    let core_before = store.schema_version().expect("core version");
    let _index = index(Arc::clone(&store));

    assert_eq!(store.schema_version().expect("core version"), core_before);
    assert_eq!(store.component_version(supra_vector::COMPONENT).expect("version"), 2);
}

#[test]
fn reopening_reads_the_stored_configuration() {
    let scratch = Scratch::new("reopen-meta");
    let created = {
        let index = index(scratch.store());
        index.meta().clone()
    };
    let reopened = index(scratch.store());
    assert_eq!(reopened.meta().dims, created.dims);
    assert_eq!(reopened.meta().model, created.model);
    assert_eq!(reopened.meta().threshold, created.threshold);
}

#[test]
fn a_second_handle_sees_the_firsts_writes_and_removals() {
    let scratch = Scratch::new("cross-handle-writes");
    let store = scratch.store();
    let writer = index(Arc::clone(&store));
    let reader = index(Arc::clone(&store));

    let query = simple(0);
    writer.upsert("writer::alpha", "alpha body", &simple(1)).expect("insert");
    let hits = reader.search_semantic(&query, 4).expect("search after insert");
    assert!(hits.iter().any(|hit| hit.locator == "writer::alpha"), "{hits:?}");

    assert!(writer.remove("writer::alpha").expect("remove"));
    let hits = reader.search_semantic(&query, 4).expect("search after remove");
    assert!(!hits.iter().any(|hit| hit.locator == "writer::alpha"), "{hits:?}");
}

#[test]
fn a_second_handle_does_not_rank_a_stale_cached_vector() {
    let scratch = Scratch::new("stale-cache");
    let store = scratch.store();
    let writer = index(Arc::clone(&store));
    let reader = index(Arc::clone(&store));

    let query = simple(0);
    writer.upsert("shared::entry", "body", &simple(1)).expect("insert opposite");
    let before = reader.search_semantic(&query, 4).expect("warm the cache");
    assert_eq!(before.len(), 1);

    writer.upsert("shared::entry", "body", &simple(0)).expect("rewrite aligned");
    let after = reader.search_semantic(&query, 4).expect("search again");
    let similarity = after
        .iter()
        .find(|hit| hit.locator == "shared::entry")
        .expect("still present")
        .similarity
        .expect("semantic lane scores");
    assert!(similarity > 0.99, "the rerank used the fresh vector, not the stale cache: {similarity}");
}

#[test]
fn a_different_model_is_refused_rather_than_scored() {
    // Embeddings from two models share no space, so a cosine between them is a number with no
    // meaning - and it would still rank. There is no safe way to answer, so this crate does not.
    let scratch = Scratch::new("model");
    let _index = index(scratch.store());

    let error =
        VectorIndex::open(scratch.store(), "other-model", DIMS, &[0.0; DIMS], IndexOptions::default())
            .expect_err("a different model must be refused");
    assert!(matches!(error, VectorError::ModelMismatch { .. }), "{error}");
    assert!(error.is_index_damage());
}

#[test]
fn a_different_width_is_refused() {
    let scratch = Scratch::new("width");
    let _index = index(scratch.store());

    let error = VectorIndex::open(scratch.store(), MODEL, 256, &[0.0; 256], IndexOptions::default())
        .expect_err("a different width must be refused");
    match error {
        VectorError::ModelMismatch { stored_dims, expected_dims, .. } => {
            assert_eq!(stored_dims, DIMS);
            assert_eq!(expected_dims, 256);
        }
        other => panic!("expected ModelMismatch, got {other}"),
    }
}

#[test]
fn a_different_threshold_is_refused_rather_than_ignored() {
    // The threshold is frozen: codes written under the old one describe a different question
    // from a query binarised under the new one. Silently keeping the stored value would leave
    // the caller believing something false about how its queries are encoded.
    let scratch = Scratch::new("threshold");
    let _index = index(scratch.store());

    let mut different = vec![0.0_f32; DIMS];
    different[0] = 0.25;
    let error = VectorIndex::open(scratch.store(), MODEL, DIMS, &different, IndexOptions::default())
        .expect_err("a different threshold must be refused");
    assert!(matches!(error, VectorError::FrozenThresholdMismatch { .. }), "{error}");
}

#[test]
fn an_unusable_width_is_refused_at_open() {
    let scratch = Scratch::new("bad-width");
    for dims in [0_usize, 7, 100, supra_vector::MAX_DIMS + 8] {
        let threshold = vec![0.0_f32; dims];
        let error = VectorIndex::open(scratch.store(), MODEL, dims, &threshold, IndexOptions::default())
            .expect_err("{dims} must be refused");
        assert!(matches!(error, VectorError::Degenerate { .. }), "{dims} dims gave {error}");
    }
}

#[test]
fn an_empty_model_identity_is_refused() {
    // A later session could not tell whether an empty identity matched, which is the one thing
    // the identity exists to answer.
    let scratch = Scratch::new("empty-model");
    let error = VectorIndex::open(scratch.store(), "", DIMS, &[0.0; DIMS], IndexOptions::default())
        .expect_err("an empty model must be refused");
    assert!(matches!(error, VectorError::Degenerate { .. }), "{error}");
}

#[test]
fn a_non_finite_threshold_is_refused() {
    // No value is above NaN, so every bit of every code would be zero and the scan would report
    // every entry as equally near.
    let scratch = Scratch::new("nan-threshold");
    let mut threshold = vec![0.0_f32; DIMS];
    threshold[5] = f32::NAN;
    let error = VectorIndex::open(scratch.store(), MODEL, DIMS, &threshold, IndexOptions::default())
        .expect_err("a NaN threshold must be refused");
    assert!(error.to_string().contains("dimension 5"), "{error}");
}

// ------------------------------------------------------------------ writes

#[test]
fn an_entry_is_retrievable_by_meaning_after_it_is_written() {
    let scratch = Scratch::new("upsert");
    let index = index(scratch.store());
    index.upsert("a", "the first entry", &simple(0)).expect("upsert");
    index.upsert("b", "the second entry", &simple(1)).expect("upsert");

    let hits = index.search_semantic(&simple(0), 2).expect("search");
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].locator, "a");
    assert!(hits[0].similarity.expect("scored") > 0.99);
}

#[test]
fn upserting_the_same_locator_replaces_the_entry_rather_than_adding_one() {
    let scratch = Scratch::new("replace");
    let index = index(scratch.store());
    let first = index.upsert("a", "first text", &simple(0)).expect("upsert");
    let second = index.upsert("a", "second text", &simple(1)).expect("upsert");

    assert_eq!(first, second, "the slot should be reused");
    assert_eq!(index.len(), 1);
    assert_eq!(index.embedding("a").expect("embedding"), simple(1));
}

#[test]
fn a_replaced_entry_is_reranked_against_its_new_vector() {
    // The failure this catches: the exact-vector cache holding the old embedding under a live
    // slot. The code would be new, the vector old, and the ranking would look ordinary.
    let scratch = Scratch::new("replacement-rerank");
    let index = index(scratch.store());
    index.upsert("a", "text", &simple(0)).expect("upsert");

    // Warm the cache with the old vector.
    let before = index.search_semantic(&simple(0), 1).expect("search");
    assert!(before[0].similarity.expect("scored") > 0.99);

    index.upsert("a", "text", &simple(7)).expect("replace");

    let after = index.search_semantic(&simple(0), 1).expect("search");
    assert!(
        after[0].similarity.expect("scored").abs() < 1e-6,
        "the stale vector was reused: got {:?}",
        after[0].similarity
    );
}

#[test]
fn a_replaced_entry_loses_its_old_text() {
    // An FTS5 row is keyed by rowid, so inserting over one without deleting first leaves two
    // postings lists for the same entry - and a renamed symbol would match its old name for ever.
    let scratch = Scratch::new("stale-text");
    let index = index(scratch.store());
    index.upsert("a", "recall_turn", &simple(0)).expect("upsert");
    assert_eq!(index.search_lexical("recall_turn", 5).expect("search").len(), 1);

    index.upsert("a", "evict_turn", &simple(0)).expect("replace");
    assert!(index.search_lexical("recall_turn", 5).expect("search").is_empty(), "the old text still matches");
    assert_eq!(index.search_lexical("evict_turn", 5).expect("search").len(), 1);
}

#[test]
fn removing_an_entry_clears_both_lanes() {
    let scratch = Scratch::new("remove");
    let index = index(scratch.store());
    index.upsert("a", "recall_turn", &simple(0)).expect("upsert");
    index.upsert("b", "evict_turn", &simple(1)).expect("upsert");

    assert!(index.remove("a").expect("remove"));
    assert_eq!(index.len(), 1);
    assert!(!index.contains("a").expect("contains"));
    assert!(index.search_lexical("recall_turn", 5).expect("search").is_empty());

    let hits = index.search_semantic(&simple(0), 5).expect("search");
    assert!(hits.iter().all(|hit| hit.locator != "a"), "{hits:?}");
}

#[test]
fn removing_something_absent_says_so() {
    let scratch = Scratch::new("remove-absent");
    let index = index(scratch.store());
    assert!(!index.remove("never-there").expect("remove"));
}

#[test]
fn an_unusable_vector_is_refused_at_the_boundary() {
    let scratch = Scratch::new("degenerate");
    let index = index(scratch.store());

    let mut nan = vec![0.5_f32; DIMS];
    nan[2] = f32::NAN;
    assert!(matches!(
        index.upsert("a", "text", &nan).expect_err("must refuse"),
        VectorError::Degenerate { .. }
    ));
    assert!(matches!(
        index.upsert("a", "text", &vec![0.0; DIMS]).expect_err("must refuse"),
        VectorError::Degenerate { .. }
    ));
    assert!(matches!(
        index.upsert("a", "text", &vec![0.5; DIMS + 8]).expect_err("must refuse"),
        VectorError::WrongWidth { .. }
    ));
    assert!(index.is_empty(), "a refused write left something behind");
}

#[test]
fn an_empty_locator_is_refused() {
    let scratch = Scratch::new("empty-locator");
    let index = index(scratch.store());
    assert!(index.upsert("", "text", &simple(0)).is_err());
}

// ------------------------------------------------------------------ persistence

#[test]
fn the_resident_tier_is_rebuilt_from_the_file() {
    let scratch = Scratch::new("persist");
    {
        let index = index(scratch.store());
        for entry in 0..20 {
            index.upsert(&format!("e{entry}"), &format!("entry {entry}"), &simple(entry)).expect("upsert");
        }
        assert_eq!(index.len(), 20);
    }

    let reopened = index(scratch.store());
    assert_eq!(reopened.len(), 20, "the codes were not reloaded");

    let hits = reopened.search_semantic(&simple(5), 1).expect("search");
    assert_eq!(hits[0].locator, "e5");
    assert!(hits[0].similarity.expect("scored") > 0.99);
}

#[test]
fn a_second_index_on_the_same_file_sees_committed_writes() {
    // Two `Store` handles are two connections, which is what a second supra process creates.
    let scratch = Scratch::new("two-handles");
    let first = index(scratch.store());
    first.upsert("a", "written by the first", &simple(0)).expect("upsert");

    let second = index(scratch.store());
    assert_eq!(second.len(), 1);
    assert!(second.contains("a").expect("contains"));
}

#[test]
fn a_code_of_the_wrong_width_in_the_file_is_refused_at_load() {
    // The schema ties a row's code and embedding widths to each other, but nothing in SQL ties
    // them to `vector_meta.dims`. Loading is where that gap closes, and it refuses rather than
    // skipping: an index quietly missing a subset of its entries would answer every query
    // slightly wrongly and never say so.
    let scratch = Scratch::new("bad-code");
    let store = scratch.store();
    {
        let index = index(Arc::clone(&store));
        index.upsert("a", "text", &simple(0)).expect("upsert");
    }

    // Half the width in both blobs, so the row's own CHECK still holds.
    store
        .with_connection(|connection| {
            connection.execute(
                "UPDATE vector_entry SET code = zeroblob(?1), embedding = zeroblob(?2)",
                rusqlite::params![
                    i64::try_from(DIMS / 16).expect("small"),
                    i64::try_from(DIMS * 2).expect("small")
                ],
            )
        })
        .expect("damage the row");

    let error = VectorIndex::open(scratch.store(), MODEL, DIMS, &[0.0; DIMS], IndexOptions::default())
        .expect_err("a wrong-width code must be refused");
    assert!(matches!(error, VectorError::Malformed { .. }), "{error}");
    assert!(error.is_index_damage());
}

// ------------------------------------------------------------------ lexical lane

#[test]
fn the_lexical_lane_finds_an_identifier_and_its_parts() {
    // FTS5's default tokeniser splits on underscores, so both the whole identifier and its parts
    // match a document containing it. That is what makes the lane useful for code.
    let scratch = Scratch::new("lexical");
    let index = index(scratch.store());
    index.upsert("src/turns.rs:118", "fn recall_turn(&self) -> Result<Vec<u8>>", &simple(0)).expect("upsert");
    index.upsert("src/lib.rs:1", "fn open(path: impl AsRef<Path>)", &simple(1)).expect("upsert");

    for term in ["recall_turn", "recall", "turn", "Vec"] {
        let hits = index.search_lexical(term, 5).expect("search");
        assert_eq!(hits.len(), 1, "{term} matched {} entries", hits.len());
        assert_eq!(hits[0].locator, "src/turns.rs:118", "{term}");
    }
}

#[test]
fn bm25_is_negative_and_the_best_match_leads() {
    // `bm25()` returns a negative score and a better match is *more* negative, so ordering
    // ascending is correct. Taking an absolute value, or ordering descending, inverts the lane.
    let scratch = Scratch::new("bm25");
    let index = index(scratch.store());
    index.upsert("weak", "recall something once", &simple(0)).expect("upsert");
    index.upsert("strong", "recall recall recall recall", &simple(1)).expect("upsert");

    let hits = index.search_lexical("recall", 5).expect("search");
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].locator, "strong", "the ranking is inverted");
    for hit in &hits {
        assert!(hit.bm25.expect("scored") < 0.0, "bm25 was not negative: {:?}", hit.bm25);
    }
    assert!(hits[0].bm25.expect("scored") < hits[1].bm25.expect("scored"), "{hits:?}");
}

#[test]
fn a_query_full_of_punctuation_does_not_fail() {
    // FTS5 query syntax gives meaning to quotes, stars, parentheses, colons, hyphens, carets and
    // the bare words AND, OR, NOT and NEAR. A task description contains those, so passing the
    // text through would make retrieval fail on the punctuation in its own input.
    let scratch = Scratch::new("punctuation");
    let index = index(scratch.store());
    index.upsert("a", "fix the parser", &simple(0)).expect("upsert");

    for query in [
        "fix the parser (see #12)",
        "\"unterminated quote",
        "a NEAR b",
        "AND OR NOT",
        "col*n: -dash ^caret",
        "*",
        "((()))",
        "  ",
        "",
        "中文 検索",
        "recall_turn AND NOT evict",
    ] {
        let outcome = index.search_lexical(query, 5);
        assert!(outcome.is_ok(), "query {query:?} failed: {:?}", outcome.err());
    }
    assert_eq!(index.search_lexical("fix the parser (see #12)", 5).expect("search").len(), 1);
}

#[test]
fn a_query_with_no_usable_term_returns_nothing_rather_than_failing() {
    let scratch = Scratch::new("no-terms");
    let index = index(scratch.store());
    index.upsert("a", "some text", &simple(0)).expect("upsert");
    assert!(index.search_lexical("!!! ??? ---", 5).expect("search").is_empty());
    assert!(supra_vector::lexical_query("!!! ???").is_none());
}

#[test]
fn a_lexical_query_is_capped_rather_than_unbounded() {
    let text = (0..200).map(|n| format!("term{n}")).collect::<Vec<_>>().join(" ");
    let query = supra_vector::lexical_query(&text).expect("terms");
    assert_eq!(query.matches(" OR ").count() + 1, supra_vector::MAX_QUERY_TOKENS);
}

#[test]
fn a_lexical_term_can_never_contain_a_quote() {
    // This is the invariant that makes quoting sufficient and escaping unnecessary. If a term
    // could hold a double quote, every query built here would be an injection into FTS5's
    // parser.
    let query = supra_vector::lexical_query("he said \"recall_turn\" then (stopped); x*y").expect("terms");
    let inner: String = query.replace(" OR ", " ");
    let quotes = inner.matches('"').count();
    let terms = inner.split_whitespace().count();
    assert_eq!(quotes, terms * 2, "a term carried an unbalanced quote: {query}");
    assert!(!query.contains("\"\"\""), "{query}");
}

#[test]
fn lexical_terms_are_deduplicated_and_lowercased() {
    let query = supra_vector::lexical_query("Recall recall RECALL turn").expect("terms");
    assert_eq!(query, "\"recall\" OR \"turn\"");
}

// ------------------------------------------------------------------ hybrid

#[test]
fn an_entry_found_by_both_lanes_leads_the_fusion() {
    let scratch = Scratch::new("hybrid");
    let index = index(scratch.store());
    // `both` is near the query vector and contains its term. `semantic` is nearer still but
    // shares no word; `lexical` shares the word but points elsewhere.
    index.upsert("semantic", "unrelated wording entirely", &simple(0)).expect("upsert");
    index.upsert("both", "recall_turn does the thing", &simple(1)).expect("upsert");
    index.upsert("lexical", "recall_turn mentioned again", &simple(40)).expect("upsert");

    let hits = index.search_hybrid("recall_turn", &simple(1), 3, 10).expect("search");
    assert_eq!(hits[0].locator, "both", "{hits:?}");
    assert!(hits[0].similarity.is_some(), "the semantic score was not carried through");
    assert!(hits[0].bm25.is_some(), "the lexical score was not carried through");
}

#[test]
fn a_hybrid_hit_from_one_lane_carries_only_that_lanes_score() {
    // A zero would be a lie in both directions: zero cosine means orthogonal, and zero bm25
    // means a perfect non-match.
    let scratch = Scratch::new("hybrid-one-lane");
    let index = index(scratch.store());
    index.upsert("a", "nothing in common", &simple(0)).expect("upsert");

    let hits = index.search_hybrid("recall_turn", &simple(0), 1, 10).expect("search");
    assert_eq!(hits.len(), 1);
    assert!(hits[0].similarity.is_some());
    assert!(hits[0].bm25.is_none(), "a lane that said nothing produced a score");
}

#[test]
fn a_hybrid_search_with_no_lexical_match_still_returns_semantic_results() {
    let scratch = Scratch::new("hybrid-semantic-only");
    let index = index(scratch.store());
    for entry in 0..5 {
        index.upsert(&format!("e{entry}"), "aaa", &simple(entry)).expect("upsert");
    }
    let hits = index.search_hybrid("zzzzz", &simple(2), 3, 10).expect("search");
    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].locator, "e2");
}

#[test]
fn asking_for_nothing_returns_nothing_from_every_lane() {
    let scratch = Scratch::new("zero-limit");
    let index = index(scratch.store());
    index.upsert("a", "text", &simple(0)).expect("upsert");
    assert!(index.search_semantic(&simple(0), 0).expect("search").is_empty());
    assert!(index.search_lexical("text", 0).expect("search").is_empty());
    assert!(index.search_hybrid("text", &simple(0), 0, 10).expect("search").is_empty());
}

#[test]
fn searching_an_empty_index_returns_nothing() {
    let scratch = Scratch::new("empty");
    let index = index(scratch.store());
    assert!(index.search_semantic(&simple(0), 10).expect("search").is_empty());
    assert!(index.search_lexical("anything", 10).expect("search").is_empty());
    assert!(index.search_hybrid("anything", &simple(0), 10, 10).expect("search").is_empty());
}

// ------------------------------------------------------------------ the design claim

#[test]
fn the_two_stage_search_returns_what_an_exhaustive_scan_would() {
    // The claim the stage rests on. A code scan cannot separate near-duplicates - one bit per
    // dimension records a direction, not a magnitude - so it is only used to choose candidates,
    // and the ordering comes from the exact vectors of those candidates. That is only sound if
    // the candidate set contains the true top-k.
    //
    // The corpus states its own separability first. A recall figure over a corpus with no
    // structure looks exactly like one over a real corpus, and that is how two earlier
    // measurements of this design came to be wrong.
    let scratch = Scratch::new("recall");
    let index = index(scratch.store());

    let mixture = Mixture::new(40, 0.55, 0xA5A5_5A5A_C3C3_3C3C);
    let corpus = mixture.draw(2_000, 0x1357_9BDF_0246_8ACE);
    let queries = mixture.draw(40, 0x7E57_C0DE_1234_5678);

    for (slot, vector) in corpus.iter().enumerate() {
        index.upsert(&format!("e{slot}"), &format!("entry {slot}"), vector).expect("upsert");
    }

    let flat: Vec<f32> = corpus.iter().flat_map(|vector| vector.iter().copied()).collect();
    let signal = CorpusSignal::measure(&queries[0], &flat, DIMS, 10).expect("measurable");
    assert!(
        signal.signal > 0.29,
        "the corpus has no structure to retrieve (mean {:.4}, best {:.4}); every recall figure \
         below would be measuring the generator",
        signal.mean,
        signal.best
    );

    let mut matched = 0_usize;
    let mut compared = 0_usize;
    for query in &queries {
        // Exhaustive, over the same vectors, with the same tie-break.
        let mut exact: Vec<(f32, usize)> = corpus
            .iter()
            .enumerate()
            .map(|(slot, vector)| (codes::similarity(query, vector), slot))
            .collect();
        exact.sort_by(|left, right| {
            right.0.partial_cmp(&left.0).unwrap_or(std::cmp::Ordering::Equal).then(left.1.cmp(&right.1))
        });
        let truth: Vec<String> = exact.iter().take(10).map(|(_, slot)| format!("e{slot}")).collect();

        let found = index.search_semantic(query, 10).expect("search");
        matched += found.iter().filter(|hit| truth.contains(&hit.locator)).count();
        compared += 10;
    }

    let recall = matched as f64 / compared as f64;
    assert!(recall >= 0.99, "recall@10 was {recall:.4} over a corpus with signal {:.4}", signal.signal);
}

#[test]
fn a_wider_rerank_never_returns_worse_results() {
    // Monotonicity is the property that makes the width a safe knob: widening it can only add
    // candidates the exact stage then orders, so a caller tuning for latency cannot accidentally
    // tune for wrongness in the other direction.
    let scratch = Scratch::new("monotone");
    let mixture = Mixture::new(40, 0.55, 0x2545_F491_4F6C_DD1D);
    let corpus = mixture.draw(600, 0x9E37_79B9_7F4A_7C15);
    let query = &mixture.draw(1, 0x0123_4567_89AB_CDEF)[0];

    let mut previous: Option<f32> = None;
    for width in [1_usize, 5, 20, 100, 600] {
        let store = scratch.store();
        let index = VectorIndex::open(
            store,
            MODEL,
            DIMS,
            &[0.0; DIMS],
            IndexOptions::default().with_rerank_width(width),
        )
        .expect("open");
        if index.is_empty() {
            for (slot, vector) in corpus.iter().enumerate() {
                index.upsert(&format!("e{slot}"), "text", vector).expect("upsert");
            }
        }

        let best = index.search_semantic(query, 1).expect("search")[0].similarity.expect("scored");
        if let Some(narrower) = previous {
            assert!(
                best >= narrower - 1e-6,
                "width {width} scored {best} where a narrower search scored {narrower}"
            );
        }
        previous = Some(best);
    }
}

#[test]
fn a_limit_wider_than_the_rerank_width_still_returns_that_many() {
    // The candidate width is raised to the limit rather than silently capping the result, which
    // would return fewer anchors than asked for and look like a sparse corpus.
    let scratch = Scratch::new("limit-over-width");
    let store = scratch.store();
    let index =
        VectorIndex::open(store, MODEL, DIMS, &[0.0; DIMS], IndexOptions::default().with_rerank_width(2))
            .expect("open");
    for entry in 0..20 {
        index.upsert(&format!("e{entry}"), "text", &simple(entry)).expect("upsert");
    }
    assert_eq!(index.search_semantic(&simple(0), 15).expect("search").len(), 15);
}

// ------------------------------------------------------------------ the cache

#[test]
fn the_exact_cache_serves_a_repeated_query() {
    let scratch = Scratch::new("cache");
    let index = index(scratch.store());
    for entry in 0..50 {
        index.upsert(&format!("e{entry}"), "text", &simple(entry)).expect("upsert");
    }

    index.search_semantic(&simple(0), 5).expect("first");
    let after_first = index.cache_stats();
    index.search_semantic(&simple(0), 5).expect("second");
    let after_second = index.cache_stats();

    assert!(after_first.misses > 0, "the first search should have missed");
    assert!(
        after_second.hits > after_first.hits,
        "the second search did not use the cache: {after_first:?} then {after_second:?}"
    );
}

#[test]
fn a_zero_byte_cache_still_answers_correctly() {
    // The cache is an optimisation, so switching it off must change the timing and nothing else.
    let scratch = Scratch::new("no-cache");
    let store = scratch.store();
    let index =
        VectorIndex::open(store, MODEL, DIMS, &[0.0; DIMS], IndexOptions::default().with_cache_bytes(0))
            .expect("open");
    for entry in 0..30 {
        index.upsert(&format!("e{entry}"), "text", &simple(entry)).expect("upsert");
    }

    let hits = index.search_semantic(&simple(7), 3).expect("search");
    assert_eq!(hits[0].locator, "e7");
    assert_eq!(index.cache_stats().hits, 0);
    assert!(index.cache_stats().misses > 0);
}

#[test]
fn resident_memory_is_reported_and_grows_with_the_corpus() {
    let scratch = Scratch::new("resident");
    let index = index(scratch.store());
    let empty = index.resident_bytes();
    for entry in 0..100 {
        index.upsert(&format!("e{entry}"), "text", &simple(entry)).expect("upsert");
    }
    assert!(index.resident_bytes() > empty);
    // One bit per dimension, so a hundred entries at 128 dimensions is 1600 bytes of codes plus
    // the slot list and its map. Asserted as a bound rather than an equality so the figure stays
    // a sanity check rather than a copy of the implementation.
    assert!(index.resident_bytes() < 100 * (DIMS / 8 + 32) + 4096, "{}", index.resident_bytes());
}

// ------------------------------------------------------------------ concurrency

#[test]
fn the_index_is_usable_from_several_threads() {
    let scratch = Scratch::new("threads");
    let index = Arc::new(index(scratch.store()));

    std::thread::scope(|scope| {
        for thread in 0..4_usize {
            let index = Arc::clone(&index);
            scope.spawn(move || {
                for entry in 0..25_usize {
                    let slot = thread * 25 + entry;
                    index.upsert(&format!("e{slot}"), "text", &simple(slot)).expect("upsert");
                    index.search_semantic(&simple(slot), 3).expect("search");
                }
            });
        }
    });

    assert_eq!(index.len(), 100);
    for slot in 0..100_usize {
        assert!(index.contains(&format!("e{slot}")).expect("contains"), "e{slot} is missing");
    }
}
