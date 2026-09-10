//! The tables this crate owns inside the store's file, and the configuration it freezes.
//!
//! # One file, a second ledger
//!
//! These tables live in T10's database rather than a second file, so a turn and the vector
//! that points at it can be written in one transaction. `user_version` is a single slot and
//! T10's core schema owns it, so this crate records its own version through
//! [`supra_store::Store::migrate_component`] under [`COMPONENT`].
//!
//! # What is frozen, and why it has to be
//!
//! [`Meta`] is written once and never updated. It holds the embedding model's identity, the
//! width, and the **binarisation threshold**.
//!
//! The threshold is the part that is easy to get wrong. A binary code records, per dimension,
//! whether the value was above a threshold. If the threshold moves - say it is recomputed as
//! the corpus mean each time the corpus grows - then every code written before the move
//! describes a different question from every code written after, and the scan silently ranks
//! them against each other. Nothing fails; the answers just quietly stop being right.
//!
//! So the threshold is chosen once, stored, and used unchanged for the life of the index.
//! Changing it means re-binarising every entry, which is a rewrite, not a migration.
//!
//! The same argument covers the model identity, and it is the rule 0xPony's `mem_meta` already
//! followed: embeddings from two models share no space, so a cosine between them is a number
//! with no meaning - and it would still rank. A mismatch is reported
//! ([`crate::VectorError::ModelMismatch`]) rather than scored.
//!
//! # `CHECK` over trust
//!
//! Following T10: an invariant the code depends on goes where no code path can bypass it.
//! `STRICT` is not enough on its own - a STRICT `TEXT` column accepts an integer and converts
//! it - so the widths are `CHECK`ed.
//!
//! One of them is worth pointing at: `length(embedding) = length(code) * 32`. A row's exact
//! vector is `dims * 4` bytes and its code is `dims / 8` bytes, so the ratio is fixed at 32
//! whatever `dims` is. A CHECK cannot reach into [`Meta`] to compare against `dims`, but it
//! can tie the two blobs in the row to each other - which catches the case that actually
//! matters: a code and a vector that describe different widths.

use supra_store::ComponentMigration;

/// The name this crate records its schema version under.
pub const COMPONENT: &str = "vector";

/// Largest width this crate accepts.
///
/// Not a limit of the format. At 4096 dimensions a code is 512 bytes, so the resident tier for
/// 100k entries is 51 MB and the scan is measured at about 21 ms - past the 5 ms retrieval
/// budget. A wider model would need a different first stage, so it is refused here rather than
/// accepted into a configuration that cannot meet its own budget.
pub const MAX_DIMS: usize = 4096;

/// Bytes per stored `f32`.
pub const EMBEDDING_SCALAR_BYTES: usize = 4;

/// Ratio between a row's exact vector and its code: `dims * 4` against `dims / 8`.
pub const EMBEDDING_TO_CODE_RATIO: usize = 32;

/// This crate's schema steps, in order.
pub const MIGRATIONS: &[ComponentMigration] = &[
    ComponentMigration {
        version: 1,
        sql: "
    -- Exactly one row, pinned by `CHECK (id = 1)`: a second configuration row would mean two
    -- answers to 'what threshold were these codes written with', and the reader would take
    -- whichever came back first.
    CREATE TABLE vector_meta (
        id         INTEGER PRIMARY KEY
                   CHECK (id = 1),
        -- Multiple of 8 because a code is byte-packed, which is also what makes the
        -- embedding-to-code ratio on `vector_entry` exact.
        dims       INTEGER NOT NULL
                   CHECK (dims > 0 AND dims % 8 = 0 AND dims <= 4096),
        -- Model identity, free-form and opaque to this crate. Compared, never parsed.
        model      TEXT    NOT NULL
                   CHECK (length(model) > 0),
        -- The frozen binarisation threshold, one f32 per dimension.
        threshold  BLOB    NOT NULL
                   CHECK (length(threshold) = dims * 4),
        created_at INTEGER NOT NULL
                   CHECK (created_at >= 0)
    ) STRICT;

    CREATE TABLE vector_entry (
        -- Dense and assigned here, because the resident code tier is a flat array indexed by
        -- this value. `INTEGER PRIMARY KEY` is the rowid, which is also what the FTS5 table
        -- joins on - so one number identifies a row in both lanes.
        slot       INTEGER PRIMARY KEY,
        -- The caller's name for the entry. This crate never parses it: T15 decides whether it
        -- is a symbol path, a line range, or a turn id.
        locator    TEXT    NOT NULL
                   CHECK (length(locator) > 0),
        -- The exact vector, f32 little-endian. Read only for the rerank stage.
        embedding  BLOB    NOT NULL,
        -- The binary code, one bit per dimension. Read on every query.
        code       BLOB    NOT NULL
                   CHECK (length(code) > 0),
        updated_at INTEGER NOT NULL
                   CHECK (updated_at >= 0),
        -- The two blobs must describe the same width. A CHECK cannot reach `vector_meta`, but
        -- `dims * 4` against `dims / 8` is a fixed ratio of 32 for every `dims`.
        CHECK (length(embedding) = length(code) * 32)
    ) STRICT;

    CREATE UNIQUE INDEX vector_entry_locator ON vector_entry (locator);

    -- Contentless: the text is indexed but not stored. The corpus is the source of truth for
    -- its own text - the digest is rebuilt from the working tree by a file watcher - so a
    -- second copy here would double the file for nothing. `contentless_delete=1` is what makes
    -- a contentless table support DELETE at all, which an incrementally maintained index
    -- needs; without it a removed symbol could only be dropped by rebuilding every row.
    CREATE VIRTUAL TABLE vector_text USING fts5(
        body,
        content='',
        contentless_delete=1
    );
",
    },
    ComponentMigration {
        version: 2,
        sql: "
    -- The cross-handle revision: every upsert and remove bumps it, and an
    -- index handle that has resident state built from an older revision
    -- reloads before answering a query. Two handles on one file otherwise
    -- diverge silently - one holds codes and cached vectors the other has
    -- already replaced.
    ALTER TABLE vector_meta ADD COLUMN revision INTEGER NOT NULL DEFAULT 0;
",
    },
];

/// A gap makes "apply everything above the current version" ambiguous and an out-of-order
/// entry silently never runs. Both are compile errors rather than test failures.
const _: () = supra_store::schema::assert_dense(MIGRATIONS);

/// The frozen configuration of an index.
#[derive(Clone, Debug, PartialEq)]
pub struct Meta {
    /// Vector width.
    pub dims: usize,
    /// Embedding model identity, as the caller supplied it.
    pub model: String,
    /// The binarisation threshold, one value per dimension.
    pub threshold: Vec<f32>,
    /// When the index was initialised, in milliseconds since the Unix epoch.
    pub created_at: i64,
}

impl Meta {
    /// Bytes an exact vector occupies at this width.
    #[must_use]
    pub const fn embedding_bytes(&self) -> usize {
        self.dims * EMBEDDING_SCALAR_BYTES
    }

    /// Bytes a code occupies at this width.
    #[must_use]
    pub const fn code_bytes(&self) -> usize {
        self.dims / 8
    }
}
