//! Secret storage for supra-harness.
//!
//! **T12** of the stage sequence. The OS keyring is the primary store and an encrypted file is
//! the automatic fallback, so a credential is available on a desktop (macOS Keychain, Windows
//! Credential Manager, a Linux Secret Service) and on a headless box where none of those exist.
//!
//! # The backend ladder
//!
//! ```text
//! OS keyring  ->  encrypted file  ->  no store (a clear error, never a hang)
//! ```
//!
//! The ladder is probed at first use, not assumed. `keyring` v4 returns `NoDefaultStore` within
//! milliseconds when no Secret Service provider is running - measured on this machine, which has
//! a session bus but no gnome-keyring or kwallet - so the fallback engages without a timeout.
//! A *blocking* probe would have needed the T7/T8 pre-flight treatment; it does not, because the
//! failure is fast.
//!
//! # What the encrypted file does and does not do
//!
//! The file (AES-256-GCM, key derived from a passphrase by PBKDF2-HMAC-SHA256) protects the
//! secret at rest: another user, a backup, a sync, or an accidental `cat` sees ciphertext. It
//! does **not** protect against an attacker who is already this user on this machine - the key
//! has to come from somewhere, and that somewhere is as readable as the file. This crate says so
//! rather than implying otherwise.
//!
//! # Redaction is the net beneath
//!
//! T8's redactor is what catches a secret that escapes through a path this crate does not
//! control. [`Secret`] stops the escapes this crate does control. Both are required.
//!
//! # Usage
//!
//! ```no_run
//! use supra_secrets::SecretManager;
//!
//! // The manager tries the OS keyring, then the encrypted file, then reports.
//! let manager = SecretManager::open();
//!
//! manager.set("openai", "primary", "sk-...")?;
//! let secret = manager.get("openai", "primary")?;
//! # Ok::<(), supra_secrets::SecretsError>(())
//! ```

#![deny(missing_docs)]
// The crate is otherwise `#![forbid(unsafe_code)]` everywhere else, but the test helpers reach
// for `set_var`/`remove_var`, which are `unsafe` in the current toolchain. T5 confines `unsafe`
// to `supra_ffi`, and this crate honours that in shipped code: the `forbid` applies to the
// library, and the test module re-allows `unsafe_code` for its serialised environment control.
#![cfg_attr(not(test), forbid(unsafe_code))]
#![cfg_attr(test, allow(unsafe_code))]
// Tests assert with `.expect()` and `panic!`, and reach for `set_var`/`remove_var` under a
// serialising lock to control the passphrase environment. Scoped to `cfg(test)` so no allow
// reaches a shipped path.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::print_stderr))]

pub mod error;
pub mod file_store;
pub mod manager;
pub mod secret;

pub use error::{SecretsError, is_no_entry};
pub use file_store::{FileStore, MASTER_KEY_ENV, PassphraseProvider};
pub use manager::{Backend, SecretManager};
pub use secret::{Secret, SecretBytes, SecretString};
