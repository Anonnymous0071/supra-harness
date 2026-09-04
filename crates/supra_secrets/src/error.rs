//! One variant per way secret operations can fail.
//!
//! The distinctions that matter: *no backend available* is a configuration problem the user
//! must fix; *encryption failed* is a bug or corruption; *not found* is normal operation.

use std::path::PathBuf;

use thiserror::Error;

/// Everything this crate can fail at.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SecretsError {
    /// No credential store could be initialised.
    ///
    /// On Linux this means no Secret Service provider (gnome-keyring, kwallet, keepassxc) is
    /// running and the encrypted file fallback directory could not be created either. On macOS
    /// or Windows it means the platform keychain refused access.
    #[error("no credential store available: {reason}")]
    NoStore {
        /// What was tried and why each attempt failed.
        reason: String,
    },

    /// A secret could not be encrypted or decrypted.
    ///
    /// This is either a bug in the crypto code or file corruption. The caller cannot recover
    /// by retrying.
    #[error("crypto operation failed: {detail}")]
    Crypto {
        /// What went wrong. Never includes the secret value.
        detail: String,
    },

    /// The requested entry does not exist in any active store.
    ///
    /// Not an error in the exceptional sense — callers should handle this as "no credential
    /// stored yet" and prompt the user or fall back to another source.
    ///
    /// `searched` describes where the search went, because a bare "not found" on a machine
    /// whose stores are not obvious would send the user hunting.
    #[error("secret not found: service={service:?}, account={account:?} ({searched})")]
    NotFound {
        /// The service name that was looked up.
        service: String,
        /// The account name that was looked up.
        account: String,
        /// Where the search went, for the message.
        searched: String,
    },

    /// A file-system operation on the encrypted store failed.
    #[error("file store I/O error at {}: {source}", path.display())]
    Io {
        /// The path involved.
        path: PathBuf,
        /// The underlying OS error.
        source: std::io::Error,
    },

    /// An upstream keyring crate error that does not fit the other variants.
    #[error("keyring backend error: {0}")]
    Keyring(String),
}

/// Whether a keyring error means "no such entry".
///
/// [`keyring::Error::NoEntry`] carries no service or account, so the conversion to
/// [`SecretsError::NotFound`] has to happen where those are known - in the manager, not in
/// [`From`]. This method is what lets the manager recognise the case.
#[must_use]
pub fn is_no_entry(error: &keyring::Error) -> bool {
    matches!(error, keyring::Error::NoEntry)
}
