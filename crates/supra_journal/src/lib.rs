//! Write-ahead file snapshots and atomic undo.
//!
//! **T16.6** of the stage sequence. The permission engine's R1 class is
//! "automatically recoverable", and the architecture is explicit about why
//! `auto` may be the default mode at all: "`auto` is the default only
//! because T16.6 `supra_journal` exists. Without an undo stack, 'auto'
//! would be a hope rather than an engineering decision."
//!
//! Two bindings shaped this crate, both from the architecture document:
//!
//! - **Snapshots before the bytes land.** T15.7's rename returns rewritten
//!   files without touching the filesystem, and its README names this stage
//!   as the owner of the moment before they do. `snapshot` reads the file,
//!   digests it, and commits the row; only after that `Ok` does the caller
//!   perform its edit - T14's "store before drop", restated for files.
//! - **One schema history per file.** `user_version` is one slot and T10's
//!   core schema owns it, so the journal's tables record their version in
//!   `schema_component` under `"journal"` - the second-owner pattern T11
//!   set, decided in T11 and written down there.
//!
//! # The shape of an undo
//!
//! `undo` spans one `Immediate` store transaction across read, verify,
//! write, flush, and mark. The store's lock is the only arbiter two
//! concurrent undoes must both pass, so the file write happens inside it;
//! the loser refuses with `AlreadyUndone`. A crash between write and mark
//! leaves the bytes restored and the row unmarked, and undoing again
//! rewrites the *same* bytes - idempotent, not destructive, which is the
//! failure mode a crash must leave behind.
//!
//! A damaged row restores nothing: the stored digest is compared against
//! the stored bytes before anything is written, and a mismatch is
//! [`JournalError::Corrupt`] with **no bytes offered** - I4's reasoning,
//! verbatim, because an undo that restores *approximately* the original is
//! worse than no undo.
//!
//! # Usage
//!
//! ```no_run
//! use std::sync::Arc;
//! use supra_journal::Journal;
//! use supra_store::Store;
//!
//! let store = Arc::new(Store::open("session.db").expect("open"));
//! let journal = Journal::open(store).expect("journal");
//!
//! let id = journal.snapshot("src/main.rs").expect("snapshot");
//! // ... the edit proceeds only after that Ok ...
//! journal.undo(id).expect("undo");
//! # Ok::<(), supra_journal::JournalError>(())
//! ```

#![deny(missing_docs)]
// Tests assert with `.expect()` and `panic!`. Scoped to `cfg(test)` so no
// allow reaches a shipped path.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

pub mod error;
pub mod journal;
pub mod schema;

pub use error::JournalError;
pub use journal::{Journal, snapshot_digest};
pub use schema::{COMPONENT, MIGRATIONS, SnapshotRow};

/// The journal sits on the turn loop (snapshot before every R1 edit, undo
/// on the user's word), so this is a requirement rather than an
/// observation.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Journal>();
    assert_send_sync::<SnapshotRow>();
};
