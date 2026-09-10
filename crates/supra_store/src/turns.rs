//! Verbatim turn eviction, and byte-identical recall.
//!
//! This is the mechanism invariant I4 rests on:
//!
//! > Old turns are **not summarised**. They are evicted verbatim to SQLite and replaced
//! > by a ~15 token index entry. The `recall` tool returns the original
//! > **byte-identical**.
//!
//! Everything here exists to make the last word of that sentence true, and to fail loudly
//! rather than quietly when it cannot be.
//!
//! # The bytes are stored, not a re-rendering of them
//!
//! A turn arrives as bytes and is written as a `BLOB`. Nothing here parses, normalises, or
//! re-serialises it. The alternative, storing a structure and rendering it back on recall,
//! would make the guarantee depend on the renderer staying identical for the life of the
//! store - which is not a property anyone can promise across versions.
//!
//! # Recall verifies before it returns
//!
//! Every row carries a digest of its own body, and [`Store::recall_turn`] recomputes it.
//! A mismatch returns [`crate::StoreError::Corrupt`] and **no bytes**. Returning them with
//! a warning attached would be worse than returning nothing: the model would carry on with
//! content that is no longer what the conversation contained, and nothing downstream could
//! tell the difference.
//!
//! # Canonical kind numbering
//!
//! The digest reuses T6's canonical encoding rather than introducing a second hashing
//! scheme, which means picking a `CANONICAL_KIND`. The convention this crate establishes:
//! **T6 owns `0x00`-`0x0F`, downstream crates take `0x10` upward**, and `0xF0` upward stays
//! reserved for tests. Without that split a later T6 type would silently collide with this
//! one, and two different values would hash the same.

use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{OptionalExtension as _, TransactionBehavior};
use supra_types::{CanonicalWriter, ContentHash, Sealable, TurnId};

use crate::Store;
use crate::error::StoreError;

/// The bytes of one evicted turn, for hashing.
///
/// A wrapper rather than a bare `blake3` call, so the digest goes through the same
/// length-prefixed, domain-separated encoding as everything else in the system. The length
/// prefix is what makes a truncated blob hash differently from the whole one.
struct TurnBody<'a>(&'a [u8]);

// Checked at compile time rather than by a test. A kind outside the downstream range would
// collide with T6's own numbering or with the range reserved for tests, and a collision
// makes two different values hash alike - which is not a thing to discover from a test run.
const _: () = {
    assert!(TurnBody::CANONICAL_KIND >= 0x10, "below the range reserved for downstream crates");
    assert!(TurnBody::CANONICAL_KIND < 0xF0, "inside the range reserved for tests");
};

impl Sealable for TurnBody<'_> {
    /// First value in the downstream range. See the module documentation.
    const CANONICAL_KIND: u8 = 0x10;

    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.bytes(1, self.0);
    }
}

/// Digest of a turn body, as the store records it.
#[must_use]
pub fn body_digest(body: &[u8]) -> ContentHash {
    ContentHash::of(&TurnBody(body))
}

/// What the store knows about one evicted turn without reading its body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EvictedTurn {
    /// Which turn.
    pub turn: TurnId,
    /// Digest of the stored body.
    pub digest: ContentHash,
    /// Size of the stored body.
    pub byte_len: u64,
    /// Milliseconds since the Unix epoch, for diagnostics only.
    ///
    /// Deliberately outside the digest: the same turn evicted at two different times still
    /// verifies, because what must be identical is the conversation, not the bookkeeping.
    pub stored_at_ms: i64,
}

impl Store {
    /// Store a turn verbatim.
    ///
    /// Idempotent for identical bytes, so a retry after a partial failure is safe.
    ///
    /// # Errors
    ///
    /// [`StoreError::Conflict`] when the turn is already stored with **different** bytes.
    /// A turn has one body; accepting a second would silently discard whichever version
    /// some other component still believes in. Any database failure otherwise.
    pub fn evict_turn(&self, turn: TurnId, body: &[u8]) -> Result<ContentHash, StoreError> {
        let digest = body_digest(body);
        let id = turn.to_string();
        let byte_len = i64::try_from(body.len()).map_err(|_| StoreError::Malformed {
            detail: format!("a turn body of {} bytes is beyond what SQLite stores", body.len()),
        })?;

        let mut connection = self.connection();
        // IMMEDIATE rather than the default DEFERRED: this reads and then writes, and a
        // deferred transaction that has already read has to *upgrade* to a write lock,
        // which is where SQLITE_BUSY comes from when two processes evict at once. Taking
        // the write lock up front makes the pair atomic against another process too, not
        // only against another thread holding this connection's mutex.
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Sqlite)?;

        let existing: Option<(Vec<u8>, Vec<u8>)> = transaction
            .query_row("SELECT body, body_hash FROM evicted_turn WHERE turn_id = ?1", [&id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()
            .map_err(StoreError::Sqlite)?;

        if let Some((stored_body, stored_bytes)) = existing {
            let stored = decode_digest(&stored_bytes, turn)?;
            if stored == digest {
                // Same turn, same digest: only a retry when the stored bytes still
                // match it. A row whose body was damaged in place keeps its hash
                // column, so treating it as idempotent would let the caller discard
                // its last good copy over silently corrupted data.
                if body_digest(&stored_body) == digest {
                    return Ok(digest);
                }
                let computed = body_digest(&stored_body);
                return Err(StoreError::Corrupt { turn, stored, computed });
            }
            return Err(StoreError::Conflict { turn, stored, offered: digest });
        }

        transaction
            .execute(
                "INSERT INTO evicted_turn (turn_id, body, body_hash, byte_len, stored_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![&id, body, digest.as_bytes().as_slice(), byte_len, now_ms()],
            )
            .map_err(StoreError::Sqlite)?;
        transaction.commit().map_err(StoreError::Sqlite)?;

        Ok(digest)
    }

    /// Return a turn exactly as it was stored.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] when the turn was never evicted.
    /// [`StoreError::Corrupt`] when the stored bytes no longer match their digest - in
    /// which case **no bytes are returned**, because a caller cannot tell altered content
    /// from the real thing once it has it.
    pub fn recall_turn(&self, turn: TurnId) -> Result<Vec<u8>, StoreError> {
        let id = turn.to_string();
        let connection = self.connection();

        let row: Option<(Vec<u8>, Vec<u8>)> = connection
            .query_row("SELECT body, body_hash FROM evicted_turn WHERE turn_id = ?1", [&id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()
            .map_err(StoreError::Sqlite)?;

        let Some((body, stored_bytes)) = row else {
            return Err(StoreError::NotFound { turn });
        };

        let stored = decode_digest(&stored_bytes, turn)?;
        let computed = body_digest(&body);
        if computed != stored {
            return Err(StoreError::Corrupt { turn, stored, computed });
        }

        Ok(body)
    }

    /// Whether a turn has been evicted, without reading its body.
    ///
    /// # Errors
    ///
    /// Any database failure.
    pub fn has_turn(&self, turn: TurnId) -> Result<bool, StoreError> {
        let id = turn.to_string();
        let connection = self.connection();
        let found: Option<i64> = connection
            .query_row("SELECT 1 FROM evicted_turn WHERE turn_id = ?1", [&id], |row| row.get(0))
            .optional()
            .map_err(StoreError::Sqlite)?;
        Ok(found.is_some())
    }

    /// Metadata for one evicted turn, without reading its body.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] when the turn was never evicted, or any database failure.
    pub fn turn_metadata(&self, turn: TurnId) -> Result<EvictedTurn, StoreError> {
        let id = turn.to_string();
        let connection = self.connection();
        let row: Option<(Vec<u8>, i64, i64)> = connection
            .query_row(
                "SELECT body_hash, byte_len, stored_at FROM evicted_turn WHERE turn_id = ?1",
                [&id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(StoreError::Sqlite)?;

        let Some((digest_bytes, byte_len, stored_at_ms)) = row else {
            return Err(StoreError::NotFound { turn });
        };

        Ok(EvictedTurn {
            turn,
            digest: decode_digest(&digest_bytes, turn)?,
            byte_len: u64::try_from(byte_len).map_err(|_| StoreError::Malformed {
                detail: format!("turn {turn} records a negative byte length"),
            })?,
            stored_at_ms,
        })
    }

    /// How many turns have been evicted.
    ///
    /// # Errors
    ///
    /// Any database failure.
    pub fn evicted_turn_count(&self) -> Result<u64, StoreError> {
        let connection = self.connection();
        let count: i64 = connection
            .query_row("SELECT count(*) FROM evicted_turn", [], |row| row.get(0))
            .map_err(StoreError::Sqlite)?;
        u64::try_from(count)
            .map_err(|_| StoreError::Malformed { detail: format!("a row count of {count} is not a count") })
    }

    /// Total size of every stored body.
    ///
    /// Reads the `byte_len` column rather than measuring the blobs, which is the reason
    /// that column exists - and the schema's `CHECK` is what keeps the two from
    /// disagreeing.
    ///
    /// `coalesce(sum(...), 0)` rather than `total(...)`. Both handle the empty table, but
    /// **`total()` returns a REAL** - measured, not assumed, after a test caught it failing
    /// to deserialise. A float byte count would also start losing whole bytes past 2^53, and
    /// this crate has no business introducing the one primitive T6 went out of its way to
    /// exclude.
    ///
    /// # Errors
    ///
    /// Any database failure.
    pub fn evicted_bytes(&self) -> Result<u64, StoreError> {
        let connection = self.connection();
        let total: i64 = connection
            .query_row("SELECT coalesce(sum(byte_len), 0) FROM evicted_turn", [], |row| row.get(0))
            .map_err(StoreError::Sqlite)?;
        u64::try_from(total)
            .map_err(|_| StoreError::Malformed { detail: format!("a total size of {total} is not a size") })
    }

    /// Every evicted turn, oldest first.
    ///
    /// # Errors
    ///
    /// Any database failure, or [`StoreError::Malformed`] for an unreadable row.
    pub fn evicted_turns(&self) -> Result<Vec<EvictedTurn>, StoreError> {
        let connection = self.connection();
        let mut statement = connection
            .prepare(
                "SELECT turn_id, body_hash, byte_len, stored_at FROM evicted_turn \
                 ORDER BY stored_at, turn_id",
            )
            .map_err(StoreError::Sqlite)?;

        let rows = statement
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let digest: Vec<u8> = row.get(1)?;
                let byte_len: i64 = row.get(2)?;
                let stored_at_ms: i64 = row.get(3)?;
                Ok((id, digest, byte_len, stored_at_ms))
            })
            .map_err(StoreError::Sqlite)?;

        let mut turns = Vec::new();
        for row in rows {
            let (id, digest_bytes, byte_len, stored_at_ms) = row.map_err(StoreError::Sqlite)?;
            let turn = id.parse::<TurnId>().map_err(|error| StoreError::Malformed {
                detail: format!("a stored turn id is not a ULID: {error}"),
            })?;
            turns.push(EvictedTurn {
                turn,
                digest: decode_digest(&digest_bytes, turn)?,
                byte_len: u64::try_from(byte_len).map_err(|_| StoreError::Malformed {
                    detail: format!("turn {turn} records a negative byte length"),
                })?,
                stored_at_ms,
            });
        }
        Ok(turns)
    }
}

/// Read a digest column back into a [`ContentHash`].
///
/// The schema constrains the width, so a wrong length here means the file was edited
/// around SQLite rather than through it.
fn decode_digest(bytes: &[u8], turn: TurnId) -> Result<ContentHash, StoreError> {
    let sized: [u8; ContentHash::LEN] = bytes.try_into().map_err(|_| StoreError::Malformed {
        detail: format!("turn {turn} records a {}-byte digest, expected {}", bytes.len(), ContentHash::LEN),
    })?;
    Ok(ContentHash::from_bytes(sized))
}

/// Milliseconds since the Unix epoch, saturating rather than panicking.
///
/// A clock before the epoch or beyond `i64` is not worth failing an eviction over: this
/// column is diagnostics, and losing a turn because the machine's clock is wrong would be
/// the wrong trade.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory().expect("in-memory store")
    }

    /// Bytes that no text-oriented path could carry unchanged: a NUL, a lone continuation
    /// byte, an overlong sequence, an unpaired surrogate encoding, and a CRLF.
    fn awkward_body() -> Vec<u8> {
        vec![
            0x00, 0xFF, 0xFE, b'h', b'i', 0x00, 0x80, 0xC0, 0x80, 0xED, 0xA0, 0x80, b'\r', b'\n', 0xF4, 0x90,
            0x80, 0x80,
        ]
    }

    #[test]
    fn a_turn_comes_back_byte_identical() {
        // The whole point of the stage. If this ever fails, invariant I4's "lossless"
        // claim is false and compaction is silently destructive.
        let store = store();
        let turn = TurnId::generate();
        let body = awkward_body();

        store.evict_turn(turn, &body).expect("evict");
        let recalled = store.recall_turn(turn).expect("recall");

        assert_eq!(recalled, body, "the bytes changed in the store");
    }

    #[test]
    fn every_byte_value_survives_a_round_trip() {
        // Exhaustive over the byte range rather than a sample, because a single value
        // mishandled by an encoding assumption is exactly the bug this must not have.
        let store = store();
        let turn = TurnId::generate();
        let body: Vec<u8> = (0..=u8::MAX).collect();

        store.evict_turn(turn, &body).expect("evict");
        assert_eq!(store.recall_turn(turn).expect("recall"), body);
    }

    #[test]
    fn an_empty_body_is_stored_and_recalled() {
        // A boundary a length-prefixed encoding and a NOT NULL column can both get wrong.
        let store = store();
        let turn = TurnId::generate();
        store.evict_turn(turn, &[]).expect("evict");
        assert_eq!(store.recall_turn(turn).expect("recall"), Vec::<u8>::new());
        assert_eq!(store.turn_metadata(turn).expect("metadata").byte_len, 0);
    }

    #[test]
    fn a_large_body_survives() {
        let store = store();
        let turn = TurnId::generate();
        // A megabyte of non-repeating bytes, so a truncation would change the digest.
        let body: Vec<u8> = (0..1_048_576_u32).map(|index| (index % 251) as u8).collect();

        store.evict_turn(turn, &body).expect("evict");
        let recalled = store.recall_turn(turn).expect("recall");
        assert_eq!(recalled.len(), body.len());
        assert_eq!(recalled, body);
    }

    #[test]
    fn recalling_a_turn_that_was_never_evicted_says_so() {
        let store = store();
        let turn = TurnId::generate();
        match store.recall_turn(turn) {
            Err(StoreError::NotFound { turn: reported }) => assert_eq!(reported, turn),
            other => panic!("expected NotFound, got {other:?}"),
        }
        assert!(!store.has_turn(turn).expect("has_turn"));
    }

    #[test]
    fn evicting_the_same_bytes_twice_is_a_no_op() {
        // A retry after a partial failure has to be safe, or a crash mid-compaction leaves
        // the store unable to accept the turn it was in the middle of.
        let store = store();
        let turn = TurnId::generate();
        let body = awkward_body();

        let first = store.evict_turn(turn, &body).expect("first");
        let second = store.evict_turn(turn, &body).expect("second must be accepted");
        assert_eq!(first, second);
        assert_eq!(store.evicted_turn_count().expect("count"), 1, "no duplicate row");
    }

    #[test]
    fn evicting_the_same_turn_with_different_bytes_is_refused() {
        // A turn has one body. Accepting the second would silently discard whichever
        // version some other component still believes in.
        let store = store();
        let turn = TurnId::generate();
        store.evict_turn(turn, b"the original").expect("first");

        match store.evict_turn(turn, b"something else") {
            Err(StoreError::Conflict { turn: reported, stored, offered }) => {
                assert_eq!(reported, turn);
                assert_eq!(stored, body_digest(b"the original"));
                assert_eq!(offered, body_digest(b"something else"));
            }
            other => panic!("expected Conflict, got {other:?}"),
        }

        // And the original is untouched.
        assert_eq!(store.recall_turn(turn).expect("recall"), b"the original");
    }

    #[test]
    fn a_tampered_body_is_refused_rather_than_returned() {
        // The enforcement of I4's promise. A caller that received altered content with a
        // warning attached would have no way to tell the difference downstream, so the
        // bytes are withheld.
        let store = store();
        let turn = TurnId::generate();
        store.evict_turn(turn, b"the real conversation").expect("evict");

        // Edit the blob around the API, leaving the digest as it was.
        {
            let connection = store.connection();
            connection
                .execute(
                    "UPDATE evicted_turn SET body = ?1, byte_len = length(?1) WHERE turn_id = ?2",
                    rusqlite::params![b"a forged conversation".as_slice(), turn.to_string()],
                )
                .expect("tamper");
        }

        match store.recall_turn(turn) {
            Err(error @ StoreError::Corrupt { .. }) => {
                assert!(error.is_damage());
                assert!(error.to_string().contains("cannot be recalled"), "{error}");
            }
            other => panic!("expected Corrupt, got {other:?}"),
        }
    }

    #[test]
    fn a_truncated_body_is_detected() {
        // The length prefix in the canonical encoding is what makes this work: without it
        // a prefix of the bytes could hash the same as the whole.
        let store = store();
        let turn = TurnId::generate();
        store.evict_turn(turn, b"0123456789").expect("evict");

        {
            let connection = store.connection();
            connection
                .execute(
                    "UPDATE evicted_turn SET body = ?1, byte_len = length(?1) WHERE turn_id = ?2",
                    rusqlite::params![b"01234".as_slice(), turn.to_string()],
                )
                .expect("truncate");
        }

        assert!(matches!(store.recall_turn(turn), Err(StoreError::Corrupt { .. })));
    }

    #[test]
    fn a_tampered_body_makes_the_eviction_retry_refuse_not_certify() {
        // Idempotence must be earned by the bytes, not the digest column alone: a row
        // damaged in place keeps its hash, so a retry that compared only hashes would
        // tell the caller "already stored, safe to drop your copy" over corrupted data.
        let store = store();
        let turn = TurnId::generate();
        store.evict_turn(turn, b"the real conversation").expect("evict");

        {
            let connection = store.connection();
            connection
                .execute(
                    "UPDATE evicted_turn SET body = ?1, byte_len = length(?1) WHERE turn_id = ?2",
                    rusqlite::params![b"a forged conversation".as_slice(), turn.to_string()],
                )
                .expect("tamper");
        }

        match store.evict_turn(turn, b"the real conversation") {
            Err(error @ StoreError::Corrupt { .. }) => assert!(error.is_damage(), "{error}"),
            other => panic!("expected Corrupt, got {other:?}"),
        }
    }

    #[test]
    fn metadata_reads_without_touching_the_body() {
        let store = store();
        let turn = TurnId::generate();
        let body = b"a turn of some length";
        let digest = store.evict_turn(turn, body).expect("evict");

        let metadata = store.turn_metadata(turn).expect("metadata");
        assert_eq!(metadata.turn, turn);
        assert_eq!(metadata.digest, digest);
        assert_eq!(usize::try_from(metadata.byte_len).expect("fits"), body.len());
        assert!(metadata.stored_at_ms >= 0);
    }

    #[test]
    fn the_digest_covers_the_body_and_nothing_beside_it() {
        // `stored_at` is outside the digest on purpose: the same conversation evicted at
        // two different times must still verify, because what has to be identical is the
        // turn, not the bookkeeping.
        let store = store();
        let turn = TurnId::generate();
        store.evict_turn(turn, b"same body").expect("evict");

        {
            let connection = store.connection();
            connection
                .execute(
                    "UPDATE evicted_turn SET stored_at = stored_at + 60000 WHERE turn_id = ?1",
                    [turn.to_string()],
                )
                .expect("move the clock");
        }

        assert_eq!(store.recall_turn(turn).expect("recall"), b"same body");
    }

    #[test]
    fn the_size_query_returns_an_integer_not_a_float() {
        // `total()` reads like the right function - it handles the empty table where `sum()`
        // returns NULL - and it returns a REAL. That cost a failing test to discover, and a
        // float byte count would start losing whole bytes past 2^53 besides.
        let store = store();
        store.evict_turn(TurnId::generate(), b"0123456789").expect("evict");

        let connection = store.connection();
        let coalesced: String = connection
            .query_row("SELECT typeof(coalesce(sum(byte_len), 0)) FROM evicted_turn", [], |row| row.get(0))
            .expect("typeof");
        assert_eq!(coalesced, "integer", "the query this crate uses must yield an integer");

        let totalled: String = connection
            .query_row("SELECT typeof(total(byte_len)) FROM evicted_turn", [], |row| row.get(0))
            .expect("typeof");
        assert_eq!(totalled, "real", "and total() is why it is not used");
    }

    #[test]
    fn counts_and_sizes_add_up() {
        let store = store();
        assert_eq!(store.evicted_turn_count().expect("count"), 0);
        assert_eq!(store.evicted_bytes().expect("bytes"), 0, "total() of no rows must be 0");

        let mut expected_bytes = 0_u64;
        for length in [0_usize, 1, 10, 1000] {
            let turn = TurnId::generate();
            let body = vec![b'x'; length];
            store.evict_turn(turn, &body).expect("evict");
            expected_bytes += length as u64;
        }

        assert_eq!(store.evicted_turn_count().expect("count"), 4);
        assert_eq!(store.evicted_bytes().expect("bytes"), expected_bytes);
    }

    #[test]
    fn listing_returns_every_turn_in_a_stable_order() {
        let store = store();
        let mut evicted = Vec::new();
        for index in 0..5_u8 {
            let turn = TurnId::generate();
            store.evict_turn(turn, &[index]).expect("evict");
            evicted.push(turn);
        }

        let listed = store.evicted_turns().expect("list");
        assert_eq!(listed.len(), 5);

        // Ordered by stored_at then turn id. The clock may not tick between evictions, so
        // the id is what makes the order total - which is why it is in the ORDER BY.
        let mut ids: Vec<TurnId> = listed.iter().map(|entry| entry.turn).collect();
        let mut expected = evicted.clone();
        ids.sort_unstable();
        expected.sort_unstable();
        assert_eq!(ids, expected, "every evicted turn must be listed exactly once");

        // And the listing is stable across calls.
        let again = store.evicted_turns().expect("list again");
        let first: Vec<TurnId> = listed.iter().map(|entry| entry.turn).collect();
        let second: Vec<TurnId> = again.iter().map(|entry| entry.turn).collect();
        assert_eq!(first, second);
    }

    #[test]
    fn the_digest_is_domain_separated_from_other_hashed_types() {
        // The reason for the kind convention. Two different types hashing the same bytes
        // must not produce the same digest, or a collision between them would look like
        // agreement.
        struct SameBytes<'a>(&'a [u8]);
        impl Sealable for SameBytes<'_> {
            const CANONICAL_KIND: u8 = 0xF6;
            fn write_canonical(&self, writer: &mut CanonicalWriter) {
                writer.bytes(1, self.0);
            }
        }

        let body = b"identical content";
        assert_ne!(
            body_digest(body),
            ContentHash::of(&SameBytes(body)),
            "a different kind must produce a different digest"
        );
    }

    #[test]
    fn two_connections_can_evict_the_same_turn_at_once() {
        // The reason the eviction path takes an IMMEDIATE transaction. Within one process
        // the connection mutex serialises writers, so nothing here exercises the
        // difference - a mutation to DEFERRED survived the whole suite until this test
        // existed.
        //
        // Two `Store` handles on one file are two connections, which is the situation a
        // second supra process creates. Under DEFERRED both would take a shared lock, read,
        // and then both try to upgrade; SQLite refuses that rather than deadlocking, and
        // `busy_timeout` cannot help because the upgrade is unsafe to retry. Taking the
        // write lock at BEGIN makes the second writer wait and then observe the first
        // writer's row, so the eviction is the idempotent no-op it is supposed to be.
        use std::sync::{Arc, Barrier};

        let directory = std::env::temp_dir().join("supra-store-two-writers");
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("scratch");
        let path = directory.join("store.db");

        let first = Store::open(&path).expect("first handle");
        let second = Store::open(&path).expect("second handle");

        let turn = TurnId::generate();
        let body = b"the same turn from two connections".to_vec();
        let barrier = Arc::new(Barrier::new(2));

        let outcomes = std::thread::scope(|scope| {
            let handles: Vec<_> = [&first, &second]
                .into_iter()
                .map(|store| {
                    let barrier = Arc::clone(&barrier);
                    let body = body.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        store.evict_turn(turn, &body)
                    })
                })
                .collect();
            handles.into_iter().map(|handle| handle.join().expect("thread")).collect::<Vec<_>>()
        });

        for outcome in &outcomes {
            assert!(
                outcome.is_ok(),
                "a concurrent eviction of identical bytes must succeed, got {outcome:?}"
            );
        }
        assert_eq!(first.evicted_turn_count().expect("count"), 1, "one row, not two");
        assert_eq!(first.recall_turn(turn).expect("recall"), body);

        drop(first);
        drop(second);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_wrong_width_digest_is_reported_as_malformed_not_padded() {
        // The schema's CHECK makes a wrong width unreachable through SQLite, so this path is
        // defensive - for a file edited around the database, or written by a schema that
        // predates the constraint. A mutation that padded instead of reporting survived the
        // whole suite, because nothing exercised it.
        //
        // Padding would not break I4: a padded digest never matches, so recall would still
        // withhold the bytes. What it would lose is the *distinction*. `Malformed` says the
        // file was edited around SQLite; `Corrupt` says the bytes changed under a valid
        // digest. Those are different causes with different remedies, and a reader who is
        // told the wrong one looks in the wrong place.
        let turn = TurnId::generate();

        let exact = vec![0_u8; ContentHash::LEN];
        assert!(decode_digest(&exact, turn).is_ok(), "the right width must be accepted");

        for width in [0, 1, ContentHash::LEN - 1, ContentHash::LEN + 1, 64] {
            let wrong = vec![0_u8; width];
            match decode_digest(&wrong, turn) {
                Err(StoreError::Malformed { detail }) => {
                    assert!(
                        detail.contains(&format!("{width}-byte digest")),
                        "the message must name the width it found: {detail}"
                    );
                }
                other => panic!("a {width}-byte digest must be Malformed, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_body_of_one_byte_differs_from_a_body_of_two() {
        // Guards the length prefix from the other direction: adjacent sizes must not
        // collide.
        assert_ne!(body_digest(b"a"), body_digest(b"aa"));
        assert_ne!(body_digest(b""), body_digest(b"\0"));
    }
}
