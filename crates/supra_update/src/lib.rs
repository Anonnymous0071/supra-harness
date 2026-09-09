//! Signed release verification and application. **T30** of the stage sequence.
//!
//! The release pipeline (`release.yml`) builds `dist/*` and signs it with
//! minisign; this crate verifies that signature on the operator's machine
//! before any bytes are applied. No network is opened here - fetching is
//! the CLI's job, verification is this crate's.

#![deny(missing_docs)]
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic))]

use thiserror::Error;

/// Why an update could not be verified or applied.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum UpdateError {
    /// The signature does not verify.
    #[error("signature verification failed: {0}")]
    BadSignature(String),
    /// The version string is not semver.
    #[error("invalid version {version:?}: {source}")]
    BadVersion {
        /// The raw version text.
        version: String,
        /// The parse failure.
        source: semver::Error,
    },
    /// I/O while reading or writing artefacts.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Verify a minisign signature over `data`.
///
/// `public_key` is the base64 minisign public key string, `signature` is the
/// `.minisig` file content (untrusted). Returns `Ok(())` when the signature
/// is valid for `data`.
///
/// # Errors
///
/// [`UpdateError::BadSignature`] when the key, signature, or verification
/// fails.
pub fn verify(data: &[u8], public_key: &str, signature: &str) -> Result<(), UpdateError> {
    let key = minisign_verify::PublicKey::from_base64(public_key)
        .map_err(|e| UpdateError::BadSignature(e.to_string()))?;
    let sig = minisign_verify::Signature::decode(signature.trim())
        .map_err(|e| UpdateError::BadSignature(e.to_string()))?;
    key.verify(data, &sig, false).map_err(|e| UpdateError::BadSignature(e.to_string()))
}

/// Parse a version string as semver, stripping a leading `v`.
///
/// # Errors
///
/// [`UpdateError::BadVersion`] when the version is not valid semver.
pub fn parse_version(raw: &str) -> Result<semver::Version, UpdateError> {
    let stripped = raw.strip_prefix('v').unwrap_or(raw);
    stripped.parse().map_err(|source| UpdateError::BadVersion { version: raw.to_owned(), source })
}
