//! Hybrid retrieval for supra-harness.
//!
//! **T11** of the stage sequence: the lexical and semantic lanes behind the repo digest's
//! anchors, over the same SQLite file T10 opened.
//!
//! # What this stage is for
//!
//! > A local hybrid BM25 + vector retrieval selects ~10 anchors, or ~300 tokens of precise
//! > pointers, appended in the suffix.
//!
//! Ten anchors, once per turn, with no model call. Everything here serves that: it has to be
//! fast enough to disappear inside a turn, cheap enough in memory to sit beside a coding
//! session, and exact enough that an anchor is the one the corpus would have chosen.
//!
//! # Shape
//!
//! | Module | Owns |
//! |---|---|
//! | [`codes`] | binary codes, the exact dot product, and vector validation |
//! | [`search`] | the resident code tier, candidate selection, and rank fusion |
//! | [`cache`] | the bounded second tier of exact vectors |
//! | [`schema`] | the tables, and the configuration that is frozen at first write |
//! | [`index`] | the queries, and the transaction that keeps both tiers honest |
//!
//! # The measurement this stage rests on
//!
//! Retrieval has to look at every entry, so its cost is bytes moved. At 768 dimensions an exact
//! vector is 3072 bytes and a code is 96. Measured on a 4-core i3-8100T, worst of 20:
//!
//! ```text
//!                        10k         100k        200k     resident at 100k
//!   exact f32 scan     2.735 ms    24.584 ms   46.985 ms      307 MB
//!   code scan          1.001 ms     4.037 ms    7.002 ms      9.6 MB
//!   + rerank of 100                 0.065 ms                  (from memory)
//!   + fetch of 100                  0.256 ms                  (from SQLite)
//! ```
//!
//! The exact scan is not slow because of its inner loop: it runs at about 12.5 GB/s, which is
//! this host's single-core streaming limit. It is slow because it reads 307 MB.
//!
//! The codes cost nothing in accuracy because they are not the answer - they only choose the
//! hundred candidates the exact vectors then order. Measured over a hundred queries on a corpus
//! with cluster structure, that recovers the exhaustive top-10 in full, and it keeps doing so on
//! every corpus whose top-10 similarity exceeds its mean by 0.29 or more.
//!
//! # Two corrections worth carrying forward
//!
//! Both mistakes were in the measurement, and both produced numbers that looked reportable.
//!
//! 1. The first exact-scan measurement reported 11.2 ms at 10k and condemned exhaustive search.
//!    It used one accumulator: float addition is not associative, so `sum()` forces a single
//!    dependency chain. Four accumulators cut it to 3.05 ms. The nested `Vec<Vec<f32>>` layout,
//!    which the diagnosis had blamed first, made no difference at all.
//! 2. The first recall measurement reported 0.544 for this design - and 0.055 for an HNSW index,
//!    which is the tell, because no graph index is that bad. The corpus generator had normalised
//!    each centroid and then added noise nine times the size of the centroid's own components,
//!    and drawn its queries from a differently seeded mixture. That is not a clustered corpus; it
//!    is noise with a faint direction, the pathological case for every approximate method at
//!    once.
//!
//! Hence [`search::CorpusSignal`]: a corpus states its own separability before a recall figure
//! taken over it means anything.
//!
//! # Why no approximate index, and what would change that
//!
//! An embedded HNSW was measured against this, on the corrected corpus: recall 0.998, p99 1.2 ms
//! at ten thousand entries - and 67 MB of graph for those ten thousand, a 5-second build, and a
//! C++ dependency. At a hundred thousand it wanted 554 MB and 115 seconds. It is faster per
//! query and costs more of everything else, against a budget this design already meets.
//!
//! What would change the answer: a corpus past a few hundred thousand entries, where a linear
//! scan leaves the budget however few bytes each entry costs. The largest repository measured on
//! this machine has 77,250 symbols.
//!
//! # Usage
//!
//! ```no_run
//! use std::sync::Arc;
//! use supra_store::Store;
//! use supra_vector::{IndexOptions, VectorIndex};
//!
//! let store = Arc::new(Store::open("/tmp/supra/store.db")?);
//! // The threshold is frozen at first initialisation. All-zero means plain sign quantisation.
//! let index = VectorIndex::open(store, "bge-small-en-v1.5", 384, &[0.0; 384], IndexOptions::default())?;
//!
//! index.upsert("src/turns.rs:118-145", "fn recall_turn returns the body byte-identical", &[0.1; 384])?;
//! let anchors = index.search_hybrid("get the old turn back", &[0.1; 384], 10, 50)?;
//! # Ok::<(), supra_vector::VectorError>(())
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]
// Tests assert with `.expect()` and `panic!`. Scoped to `cfg(test)` so no allow reaches a
// shipped path.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

pub mod cache;
pub mod codes;
pub mod error;
pub mod index;
pub mod schema;
pub mod search;

pub use cache::{CacheStats, DEFAULT_CACHE_BYTES, ExactCache};
pub use error::VectorError;
pub use index::{DEFAULT_RERANK_WIDTH, Hit, IndexOptions, MAX_QUERY_TOKENS, VectorIndex, lexical_query};
pub use schema::{COMPONENT, MAX_DIMS, Meta};
pub use search::{Candidate, CodeTable, CorpusSignal, Fused, LaneRank, MAX_FUSION_DEPTH, RRF_K, RRF_SCALE};

/// The index is opened once and queried from the turn loop while the digest's watcher updates
/// it, so this is a requirement rather than an observation.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<VectorIndex>();
};
