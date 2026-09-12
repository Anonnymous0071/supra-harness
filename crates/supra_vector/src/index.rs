//! The index itself: the SQL, the two tiers, and the three queries.
//!
//! # What a caller gets
//!
//! - [`VectorIndex::search_semantic`] - nearest by meaning, exact ordering.
//! - [`VectorIndex::search_lexical`] - FTS5 with `bm25()`, for the terms a query names outright.
//! - [`VectorIndex::search_hybrid`] - both, fused over ranks.
//!
//! Both lanes exist because they fail differently. The semantic lane finds a function that
//! *does* what was asked without sharing a word with the question, and misses an exact
//! identifier it has never seen in that context. The lexical lane does the opposite: it finds
//! `recall_turn` when the query says `recall_turn`, and nothing when the query says "get the
//! old message back". Fusing them is not an average of two opinions - it is a union of two
//! different competences.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rusqlite::{Connection, OptionalExtension as _, Transaction, TransactionBehavior};
use supra_store::Store;

use crate::cache::{CacheStats, DEFAULT_CACHE_BYTES, ExactCache};
use crate::codes;
use crate::error::VectorError;
use crate::schema::{self, MAX_DIMS, Meta};
use crate::search::{self, CodeTable, Scored};

/// Candidates the code scan hands to the exact rerank, by default.
///
/// Measured over a hundred queries against a corpus with cluster structure: `R = 10` recovers
/// 0.898 of the exhaustive top-10, `R = 50` recovers all of it, and `R = 100` still recovers all
/// of it on every corpus whose top-10 similarity exceeds its mean by 0.29 or more. 100 rather
/// than 50 because the extra fifty cost about 130 µs against a 5 ms budget, and the corpora
/// where 50 is not enough are the ones nobody measured.
pub const DEFAULT_RERANK_WIDTH: usize = 100;

/// Most tokens taken from one lexical query.
///
/// A task description is not a search box: it can be a paragraph. Every token widens the FTS5
/// query, and past a few dozen the terms are common words that rank nothing. Capped rather than
/// truncated silently - [`VectorIndex::search_lexical`] reports what it used.
pub const MAX_QUERY_TOKENS: usize = 32;

/// How an index is opened.
#[derive(Clone, Copy, Debug)]
pub struct IndexOptions {
    /// Candidates the code scan passes to the exact rerank.
    pub rerank_width: usize,
    /// Byte budget for the exact-vector cache.
    pub cache_bytes: usize,
}

impl Default for IndexOptions {
    fn default() -> Self {
        Self { rerank_width: DEFAULT_RERANK_WIDTH, cache_bytes: DEFAULT_CACHE_BYTES }
    }
}

impl IndexOptions {
    /// Use a different rerank width.
    #[must_use]
    pub const fn with_rerank_width(mut self, width: usize) -> Self {
        self.rerank_width = width;
        self
    }

    /// Use a different cache budget.
    #[must_use]
    pub const fn with_cache_bytes(mut self, bytes: usize) -> Self {
        self.cache_bytes = bytes;
        self
    }
}

/// Hybrid retrieval over one store.
pub struct VectorIndex {
    store: Arc<Store>,
    meta: Meta,
    options: IndexOptions,
    state: Mutex<State>,
}

struct State {
    codes: CodeTable,
    cache: ExactCache,
    revision: u64,
}

impl VectorIndex {
    /// Open the index on `store`, creating its tables and configuration if they are absent.
    ///
    /// `model` is an opaque identity for the embedding model - this crate compares it and never
    /// parses it. `threshold` is the binarisation threshold, one value per dimension, and it is
    /// **frozen** at first initialisation: see [`crate::schema`] for why moving it later would
    /// silently invalidate every code already written.
    ///
    /// # Errors
    ///
    /// [`VectorError::ModelMismatch`] when the file was built for a different model or width -
    /// which is not recoverable here, because a cosine between two models' embeddings is a
    /// number with no meaning and it would still rank. [`VectorError::WrongWidth`] when
    /// `threshold` is not `dims` long, [`VectorError::Degenerate`] when `dims` is unusable, and
    /// [`VectorError::Store`] for a migration failure.
    pub fn open(
        store: Arc<Store>,
        model: &str,
        dims: usize,
        threshold: &[f32],
        options: IndexOptions,
    ) -> Result<Self, VectorError> {
        if dims == 0 || dims % 8 != 0 || dims > MAX_DIMS {
            return Err(VectorError::Degenerate {
                detail: format!(
                    "{dims} dimensions is not usable: it must be a positive multiple of 8, at \
                     most {MAX_DIMS}"
                ),
            });
        }
        if threshold.len() != dims {
            return Err(VectorError::WrongWidth { expected: dims, found: threshold.len() });
        }
        if model.is_empty() {
            return Err(VectorError::Degenerate {
                detail: "the model identity is empty, so a later session could not tell whether \
                         it matched"
                    .to_owned(),
            });
        }
        for (index, value) in threshold.iter().enumerate() {
            if !value.is_finite() {
                return Err(VectorError::Degenerate {
                    detail: format!("threshold dimension {index} is {value}, which no value can be above"),
                });
            }
        }

        store.migrate_component(schema::COMPONENT, schema::MIGRATIONS)?;

        let existing =
            store.with_transaction::<_, VectorError>(TransactionBehavior::Immediate, |transaction| {
                if let Some(found) = read_meta(transaction)? {
                    return Ok(Some(found));
                }
                write_meta(transaction, model, dims, threshold)?;
                Ok(None)
            })?;

        let meta = match existing {
            Some(found) => {
                if found.model != model || found.dims != dims {
                    return Err(VectorError::ModelMismatch {
                        path: store.path().to_path_buf(),
                        stored: found.model,
                        stored_dims: found.dims,
                        expected: model.to_owned(),
                        expected_dims: dims,
                    });
                }
                // Compared as bytes, not as floats. Both sides come from `to_le_bytes`, so the
                // comparison is exact and deterministic - where comparing `f32` values for
                // equality would be the one operation this project keeps out of stored data.
                if codes::encode_embedding(&found.threshold) != codes::encode_embedding(threshold) {
                    return Err(VectorError::FrozenThresholdMismatch { path: store.path().to_path_buf() });
                }
                found
            }
            None => {
                Meta { dims, model: model.to_owned(), threshold: threshold.to_vec(), created_at: now_ms() }
            }
        };

        let (codes, revision) =
            store.with_transaction::<_, VectorError>(TransactionBehavior::Deferred, |transaction| {
                let revision = read_revision(transaction)?;
                let codes = load_codes(transaction, &meta)?;
                Ok((codes, revision))
            })?;
        Ok(Self {
            store,
            meta,
            options,
            state: Mutex::new(State { codes, cache: ExactCache::new(options.cache_bytes), revision }),
        })
    }

    /// The frozen configuration.
    #[must_use]
    pub const fn meta(&self) -> &Meta {
        &self.meta
    }

    /// The options in force.
    #[must_use]
    pub const fn options(&self) -> IndexOptions {
        self.options
    }

    /// How many entries are indexed.
    #[must_use]
    pub fn len(&self) -> usize {
        self.state().codes.len()
    }

    /// Whether nothing is indexed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.state().codes.is_empty()
    }

    /// Memory the resident tiers cost right now.
    #[must_use]
    pub fn resident_bytes(&self) -> usize {
        let state = self.state();
        state.codes.resident_bytes() + state.cache.used_bytes()
    }

    /// What the exact-vector cache has been doing.
    #[must_use]
    pub fn cache_stats(&self) -> CacheStats {
        self.state().cache.stats()
    }

    /// Add or replace one entry.
    ///
    /// `locator` is the caller's name for it, and the unique key. `body` is the text the lexical
    /// lane indexes; it is **not stored**, because the corpus is the source of truth for its own
    /// text. `embedding` is the exact vector, which must be unit length for the similarity to be
    /// a cosine - this crate checks that it is usable, not that it is normalised, because
    /// normalising here would hide a caller that forgot.
    ///
    /// Returns the slot. The database write and both tiers move together: a failure leaves the
    /// index exactly as it was.
    ///
    /// # Errors
    ///
    /// [`VectorError::WrongWidth`] or [`VectorError::Degenerate`] for an unusable vector,
    /// [`VectorError::Store`] or [`VectorError::Sqlite`] for a write failure.
    pub fn upsert(&self, locator: &str, body: &str, embedding: &[f32]) -> Result<i64, VectorError> {
        if locator.is_empty() {
            return Err(VectorError::Degenerate {
                detail: "an empty locator cannot name an entry".to_owned(),
            });
        }
        codes::validate(embedding, self.meta.dims)?;

        let code = codes::encode(embedding, &self.meta.threshold)?;
        let blob = codes::encode_embedding(embedding);
        let stamp = now_ms();

        let (slot, revision) =
            self.store.with_transaction::<_, VectorError>(TransactionBehavior::Immediate, |transaction| {
                let slot: i64 = transaction.query_row(
                    "INSERT INTO vector_entry (locator, embedding, code, updated_at) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT (locator) DO UPDATE SET \
                     embedding = excluded.embedding, \
                     code = excluded.code, \
                     updated_at = excluded.updated_at \
                 RETURNING slot",
                    rusqlite::params![locator, &blob, &code, stamp],
                    |row| row.get(0),
                )?;

                // Delete before insert, because an FTS5 row is keyed by rowid and inserting over
                // one would leave two postings lists for the same entry - so a renamed symbol
                // would still match its old text for ever.
                transaction.execute("DELETE FROM vector_text WHERE rowid = ?1", [slot])?;
                transaction.execute(
                    "INSERT INTO vector_text (rowid, body) VALUES (?1, ?2)",
                    rusqlite::params![slot, body],
                )?;
                let revision = bump_revision(transaction)?;
                Ok((slot, revision))
            })?;

        // Only after the commit. The resident tier is a cache of what is durable, and updating
        // it first would leave it describing a write that failed.
        let mut state = self.state();
        state.codes.upsert(slot, &code);
        state.cache.invalidate(slot);
        state.revision = revision;
        Ok(slot)
    }

    /// Remove one entry by locator. Returns whether it was there.
    ///
    /// # Errors
    ///
    /// [`VectorError::Store`] or [`VectorError::Sqlite`] for a write failure.
    pub fn remove(&self, locator: &str) -> Result<bool, VectorError> {
        let removed =
            self.store.with_transaction::<_, VectorError>(TransactionBehavior::Immediate, |transaction| {
                let slot: Option<i64> = transaction
                    .query_row(
                        "DELETE FROM vector_entry WHERE locator = ?1 RETURNING slot",
                        [locator],
                        |row| row.get(0),
                    )
                    .optional()?;
                let Some(slot) = slot else { return Ok(None) };
                transaction.execute("DELETE FROM vector_text WHERE rowid = ?1", [slot])?;
                let revision = bump_revision(transaction)?;
                Ok(Some((slot, revision)))
            })?;

        let Some((slot, revision)) = removed else { return Ok(false) };
        let mut state = self.state();
        state.codes.remove(slot);
        state.cache.invalidate(slot);
        state.revision = revision;
        Ok(true)
    }

    /// Nearest by meaning: scan the codes, then order the candidates by their exact vectors.
    ///
    /// `limit` is how many to return. The candidate width comes from
    /// [`IndexOptions::rerank_width`], and is raised to `limit` when a caller asks for more
    /// results than candidates - otherwise the request would silently return fewer.
    ///
    /// # Errors
    ///
    /// [`VectorError::WrongWidth`] or [`VectorError::Degenerate`] for an unusable query,
    /// [`VectorError::Malformed`] when a stored vector is not the width the schema promised.
    pub fn search_semantic(&self, query: &[f32], limit: usize) -> Result<Vec<Hit>, VectorError> {
        codes::validate(query, self.meta.dims)?;
        if limit == 0 {
            return Ok(Vec::new());
        }

        let query_code = codes::encode(query, &self.meta.threshold)?;
        let width = self.options.rerank_width.max(limit);

        self.store.with_transaction::<_, VectorError>(TransactionBehavior::Deferred, |transaction| {
            self.sync_if_stale(transaction)?;

            let candidates = self.state().codes.nearest(&query_code, width);
            if candidates.is_empty() {
                return Ok(Vec::new());
            }
            run_before_exact_hook();

            let mut scored = Vec::with_capacity(candidates.len());
            for candidate in &candidates {
                let vector = self.exact(transaction, candidate.slot)?;
                scored.push(Scored { slot: candidate.slot, similarity: codes::similarity(query, &vector) });
            }

            let mut ordered = search::order_exact(scored);
            ordered.truncate(limit);
            Self::name(transaction, &ordered)
        })
    }

    /// Best by term match, ranked by `bm25()`.
    ///
    /// The query text is tokenised here rather than passed to FTS5 as written. A task
    /// description contains parentheses, colons and hyphens, all of which are FTS5 query
    /// syntax - so passing it through would make retrieval fail on the punctuation in its own
    /// input. See [`lexical_query`].
    ///
    /// # Errors
    ///
    /// [`VectorError::Sqlite`] when the query fails.
    pub fn search_lexical(&self, text: &str, limit: usize) -> Result<Vec<Hit>, VectorError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let Some(query) = lexical_query(text) else { return Ok(Vec::new()) };

        self.store.with_connection(|connection| {
            let mut statement = connection.prepare(
                // Ascending: bm25() is negative and a better match is *more* negative, so
                // ordering descending - or taking an absolute value - inverts the ranking.
                //
                // The table is named in full rather than aliased. An FTS5 auxiliary function
                // takes the table as its argument and does not accept an alias: `bm25(t)` is
                // rejected as "no such column: t", which is a confusing way to be told that.
                "SELECT vector_text.rowid, vector_entry.locator, bm25(vector_text) \
                 FROM vector_text \
                 JOIN vector_entry ON vector_entry.slot = vector_text.rowid \
                 WHERE vector_text MATCH ?1 \
                 ORDER BY bm25(vector_text) \
                 LIMIT ?2",
            )?;
            let rows = statement.query_map(
                rusqlite::params![query, i64::try_from(limit).unwrap_or(i64::MAX)],
                |row| {
                    Ok(Hit { slot: row.get(0)?, locator: row.get(1)?, similarity: None, bm25: row.get(2)? })
                },
            )?;
            rows.collect::<rusqlite::Result<Vec<Hit>>>().map_err(VectorError::Sqlite)
        })
    }

    /// Both lanes, fused over ranks.
    ///
    /// Each lane is asked for `depth` results, and the fused list is cut to `limit`. `depth`
    /// wider than `limit` is the point: an entry that neither lane ranked first can lead the
    /// fusion, and a lane cut to `limit` could never contribute it.
    ///
    /// # Errors
    ///
    /// As the two lanes.
    pub fn search_hybrid(
        &self,
        text: &str,
        query: &[f32],
        limit: usize,
        depth: usize,
    ) -> Result<Vec<Hit>, VectorError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let depth = depth.max(limit);

        let semantic = self.search_semantic(query, depth)?;
        let lexical = self.search_lexical(text, depth)?;

        let semantic_slots: Vec<i64> = semantic.iter().map(|hit| hit.slot).collect();
        let lexical_slots: Vec<i64> = lexical.iter().map(|hit| hit.slot).collect();
        let fused = search::fuse(&[&semantic_slots, &lexical_slots]);

        let mut hits = Vec::with_capacity(limit.min(fused.len()));
        for entry in fused.into_iter().take(limit) {
            // Carry each lane's own number through: a caller that wants to show why an anchor
            // was chosen needs the cosine and the bm25, and the fused score is unitless.
            let semantic_hit = semantic.iter().find(|hit| hit.slot == entry.slot);
            let lexical_hit = lexical.iter().find(|hit| hit.slot == entry.slot);
            let locator = semantic_hit.or(lexical_hit).map(|hit| hit.locator.clone()).ok_or_else(|| {
                VectorError::Malformed { detail: format!("slot {} was fused from no lane", entry.slot) }
            })?;
            hits.push(Hit {
                slot: entry.slot,
                locator,
                similarity: semantic_hit.and_then(|hit| hit.similarity),
                bm25: lexical_hit.and_then(|hit| hit.bm25),
            });
        }
        Ok(hits)
    }

    /// The exact vector for one locator, for a caller that wants to compare outside a search.
    ///
    /// # Errors
    ///
    /// [`VectorError::NotFound`] when the locator is not indexed, [`VectorError::Malformed`]
    /// when the stored blob is not the promised width.
    pub fn embedding(&self, locator: &str) -> Result<Vec<f32>, VectorError> {
        let blob: Option<Vec<u8>> = self.store.with_connection(|connection| {
            connection
                .query_row("SELECT embedding FROM vector_entry WHERE locator = ?1", [locator], |row| {
                    row.get(0)
                })
                .optional()
                .map_err(VectorError::Sqlite)
        })?;
        let blob = blob.ok_or_else(|| VectorError::NotFound { locator: locator.to_owned() })?;
        codes::decode_embedding(&blob, self.meta.dims)
    }

    /// Whether a locator is indexed.
    ///
    /// # Errors
    ///
    /// [`VectorError::Sqlite`] when the query fails.
    pub fn contains(&self, locator: &str) -> Result<bool, VectorError> {
        self.store.with_connection(|connection| {
            let found: Option<i64> = connection
                .query_row("SELECT slot FROM vector_entry WHERE locator = ?1", [locator], |row| row.get(0))
                .optional()?;
            Ok(found.is_some())
        })
    }

    /// Read one exact vector, through the cache.
    fn exact(&self, connection: &Connection, slot: i64) -> Result<Vec<f32>, VectorError> {
        if let Some(cached) = self.state().cache.get(slot) {
            return Ok(cached);
        }

        let blob: Option<Vec<u8>> = connection
            .query_row("SELECT embedding FROM vector_entry WHERE slot = ?1", [slot], |row| row.get(0))
            .optional()?;

        // A slot in the resident tier with no row behind it means the two disagree, which is a
        // damaged index rather than a missing entry: every slot in the tier was put there by a
        // committed write.
        let blob = blob.ok_or_else(|| VectorError::Malformed {
            detail: format!("slot {slot} is in the resident tier but has no row"),
        })?;

        let vector = codes::decode_embedding(&blob, self.meta.dims)?;
        self.state().cache.put(slot, vector.clone());
        Ok(vector)
    }

    /// Attach locators to scored slots, in one query rather than one per hit.
    fn name(connection: &Connection, scored: &[Scored]) -> Result<Vec<Hit>, VectorError> {
        let mut hits = Vec::with_capacity(scored.len());
        let mut statement = connection.prepare_cached("SELECT locator FROM vector_entry WHERE slot = ?1")?;
        for entry in scored {
            let locator: Option<String> = statement.query_row([entry.slot], |row| row.get(0)).optional()?;
            let locator = locator.ok_or_else(|| VectorError::Malformed {
                detail: format!("slot {} is in the resident tier but has no row", entry.slot),
            })?;
            hits.push(Hit { slot: entry.slot, locator, similarity: Some(entry.similarity), bm25: None });
        }
        Ok(hits)
    }

    /// Poisoning is ignored, as in T10: a panic inside a lookup leaves both tiers structurally
    /// intact, and refusing every later query because one caller panicked would turn a
    /// recoverable fault into a dead session.
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Reload resident state when another handle has written the file.
    ///
    /// Two `VectorIndex` handles on one store are legal - the digest and a
    /// sibling lane both want one - and neither can see the other's writes
    /// through its resident tier or its exact cache. The revision the
    /// writer bumped is the signal: when it moved, the codes are reloaded
    /// and the cache dropped before the query reads either.
    fn sync_if_stale(&self, connection: &Connection) -> Result<(), VectorError> {
        let current = read_revision(connection)?;
        if self.state().revision == current {
            return Ok(());
        }
        let codes = load_codes(connection, &self.meta)?;
        let mut state = self.state();
        if state.revision == current {
            return Ok(());
        }
        state.codes = codes;
        state.cache = ExactCache::new(self.options.cache_bytes);
        state.revision = current;
        Ok(())
    }
}

/// Lock-free, and therefore partial: reporting the resident count would take the mutex the
/// queries hold, so a `Debug` inside a diagnostic could block on a search. The two tiers are
/// omitted on purpose rather than by oversight - [`VectorIndex::len`] and
/// [`VectorIndex::resident_bytes`] are the reporting path, and they are allowed to wait.
#[allow(clippy::missing_fields_in_debug, reason = "both tiers sit behind a mutex that a Debug must not take")]
impl core::fmt::Debug for VectorIndex {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("VectorIndex")
            .field("path", &self.store.path())
            .field("model", &self.meta.model)
            .field("dims", &self.meta.dims)
            .field("rerank_width", &self.options.rerank_width)
            .finish()
    }
}

/// One retrieved entry.
///
/// `similarity` and `bm25` are `Option` because a lane that did not return the entry has no
/// number for it. A zero would be a lie in both directions: zero cosine means orthogonal, and
/// zero `bm25()` means a perfect non-match.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    /// The entry's slot.
    pub slot: i64,
    /// The caller's name for it.
    pub locator: String,
    /// Exact cosine against the query, when the semantic lane returned it.
    pub similarity: Option<f32>,
    /// `bm25()` score, when the lexical lane returned it. Negative, and more negative is better.
    pub bm25: Option<f64>,
}

/// Build an FTS5 query from free text.
///
/// Returns `None` when the text holds no usable term, so the lane returns nothing rather than
/// failing.
///
/// Terms are runs of alphanumeric characters and underscores, each quoted as a phrase and joined
/// with `OR`. Three decisions worth stating:
///
/// - **Quoting, not escaping.** FTS5 query syntax gives meaning to `"`, `*`, `(`, `)`, `:`, `-`,
///   `^` and the bare words `AND`, `OR`, `NOT`, `NEAR`. A task description contains those. Passed
///   through, `fix the parser (see #12)` is a syntax error, and retrieval would fail on the
///   punctuation in its own input.
/// - **`OR`, not `AND`.** A query is a sentence, not a filter. `AND` over a dozen terms matches
///   nothing; `OR` lets `bm25()` do what it is for, which is to weight the rare terms above the
///   common ones.
/// - **Underscores kept inside terms.** FTS5's default tokeniser splits `recall_turn` into
///   `recall` and `turn`, so both the whole identifier and its parts match a document containing
///   it. Splitting here as well would only lose the caller's intent when they typed it in full.
#[must_use]
pub fn lexical_query(text: &str) -> Option<String> {
    let mut terms: Vec<String> = Vec::new();
    let mut current = String::new();

    for character in text.chars() {
        if character.is_alphanumeric() || character == '_' {
            current.push(character);
        } else if !current.is_empty() {
            push_term(&mut terms, &current);
            current.clear();
        }
    }
    if !current.is_empty() {
        push_term(&mut terms, &current);
    }

    if terms.is_empty() {
        return None;
    }
    Some(terms.join(" OR "))
}

fn push_term(terms: &mut Vec<String>, term: &str) {
    if terms.len() >= MAX_QUERY_TOKENS {
        return;
    }
    // A term cannot contain a double quote - it is alphanumeric plus underscore by construction -
    // so quoting is sufficient and no escaping is needed. Asserted by a test rather than left as
    // a remark, because that invariant is what makes this safe.
    let quoted = format!("\"{}\"", term.to_lowercase());
    if terms.contains(&quoted) {
        return;
    }
    terms.push(quoted);
}

/// The store's current revision, as writers bumped it.
fn read_revision(connection: &Connection) -> Result<u64, VectorError> {
    let revision: Option<i64> = connection
        .query_row("SELECT revision FROM vector_meta WHERE id = 1", [], |row| row.get(0))
        .optional()?;
    Ok(u64::try_from(revision.unwrap_or(0)).unwrap_or(0))
}

/// Bump the revision inside a write transaction and return the new value.
fn bump_revision(transaction: &Transaction<'_>) -> Result<u64, VectorError> {
    let revision: i64 = transaction
        .query_row(
            "UPDATE vector_meta SET revision = revision + 1 WHERE id = 1 RETURNING revision",
            [],
            |row| row.get(0),
        )
        .map_err(VectorError::Sqlite)?;
    Ok(u64::try_from(revision).unwrap_or(0))
}

fn read_meta(connection: &Connection) -> Result<Option<Meta>, VectorError> {
    let row: Option<(i64, String, Vec<u8>, i64)> = connection
        .query_row("SELECT dims, model, threshold, created_at FROM vector_meta WHERE id = 1", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .optional()?;

    let Some((dims, model, threshold, created_at)) = row else { return Ok(None) };
    let dims = usize::try_from(dims)
        .map_err(|_| VectorError::Malformed { detail: format!("the index records {dims} dimensions") })?;
    Ok(Some(Meta { dims, model, threshold: codes::decode_embedding(&threshold, dims)?, created_at }))
}

fn write_meta(
    connection: &Connection,
    model: &str,
    dims: usize,
    threshold: &[f32],
) -> Result<(), VectorError> {
    connection.execute(
        "INSERT INTO vector_meta (id, dims, model, threshold, created_at) VALUES (1, ?1, ?2, ?3, ?4)",
        rusqlite::params![
            i64::try_from(dims).unwrap_or(i64::MAX),
            model,
            codes::encode_embedding(threshold),
            now_ms(),
        ],
    )?;
    Ok(())
}

/// Read every code into the resident tier.
///
/// One pass, with the row count read first so the buffer is allocated once: growing a 9.6 MB
/// buffer by doubling copies it about seventeen times on the way up.
fn load_codes(connection: &Connection, meta: &Meta) -> Result<CodeTable, VectorError> {
    let count: i64 = connection.query_row("SELECT count(*) FROM vector_entry", [], |row| row.get(0))?;
    let capacity = usize::try_from(count).unwrap_or(0);
    let mut table = CodeTable::with_capacity(meta.code_bytes(), capacity);

    let mut statement = connection.prepare("SELECT slot, code FROM vector_entry ORDER BY slot")?;
    let rows = statement.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)))?;

    for row in rows {
        let (slot, code) = row?;
        if !table.upsert(slot, &code) {
            // The schema ties a row's code and embedding widths to each other, but nothing
            // in SQL ties them to `vector_meta.dims`. This is where that gap is closed, and
            // it is a refusal rather than a skip: an index missing an arbitrary subset of
            // its entries would answer every query slightly wrongly and never say so.
            return Err(VectorError::Malformed {
                detail: format!(
                    "slot {slot} holds a {}-byte code where this index uses {}",
                    code.len(),
                    meta.code_bytes()
                ),
            });
        }
    }
    Ok(table)
}

#[cfg(test)]
type BeforeExactHook = Box<dyn FnOnce() + Send>;

#[cfg(test)]
static BEFORE_EXACT_HOOK: Mutex<Option<BeforeExactHook>> = Mutex::new(None);

#[cfg(test)]
fn run_before_exact_hook() {
    let hook = BEFORE_EXACT_HOOK.lock().unwrap_or_else(PoisonError::into_inner).take();
    if let Some(hook) = hook {
        hook();
    }
}

#[cfg(not(test))]
const fn run_before_exact_hook() {}

/// Milliseconds since the Unix epoch, or 0 for a clock set before it - which the schema's
/// `CHECK (updated_at >= 0)` requires, and which is a better outcome than refusing to index
/// anything on a machine with a wrong clock.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::mpsc;

    const TEST_DIMS: usize = 8;
    const TEST_MODEL: &str = "snapshot-test";

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "supra-vector-snapshot-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("scratch directory");
            Self(path)
        }

        fn store(&self) -> Arc<Store> {
            Arc::new(Store::open(self.0.join("store.db")).expect("open store"))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn test_index(store: Arc<Store>) -> VectorIndex {
        VectorIndex::open(store, TEST_MODEL, TEST_DIMS, &[0.0; TEST_DIMS], IndexOptions::default())
            .expect("open index")
    }

    #[test]
    fn semantic_search_keeps_one_snapshot_during_a_concurrent_remove() {
        let scratch = Scratch::new();
        let seed = test_index(scratch.store());
        let query = [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        seed.upsert("entry", "body", &query).expect("seed entry");

        let reader = test_index(scratch.store());
        let writer = test_index(scratch.store());
        let (candidates_ready, wait_for_candidates) = mpsc::channel();
        let (remove_done, wait_for_remove) = mpsc::channel();
        *BEFORE_EXACT_HOOK.lock().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(move || {
            candidates_ready.send(()).expect("signal selected candidates");
            wait_for_remove.recv().expect("wait for committed remove");
        }));

        std::thread::scope(|scope| {
            scope.spawn(move || {
                wait_for_candidates.recv().expect("wait for selected candidates");
                assert!(writer.remove("entry").expect("remove entry"));
                remove_done.send(()).expect("signal committed remove");
            });

            let hits = reader.search_semantic(&query, 1).expect("search one snapshot");
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].locator, "entry");
        });

        assert!(reader.search_semantic(&query, 1).expect("search current revision").is_empty());
    }
}
