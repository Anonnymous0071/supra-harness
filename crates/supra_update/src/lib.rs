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
    /// The manifest names a different version than the operator asked
    /// to install.
    #[error("the manifest offers {offered} where {expected} was expected")]
    VersionMismatch {
        /// The version the manifest carries.
        offered: String,
        /// The version the caller pinned.
        expected: String,
    },
    /// The manifest names a different target platform.
    #[error("the manifest offers target {offered} where {expected} was expected")]
    TargetMismatch {
        /// The target the manifest carries.
        offered: String,
        /// The target the caller pinned.
        expected: String,
    },
    /// The manifest's digest does not match the artefact bytes.
    #[error("the artefact digest is {computed} where the manifest pinned {pinned}")]
    DigestMismatch {
        /// The digest of the bytes on disk.
        computed: String,
        /// The digest the manifest carries.
        pinned: String,
    },
    /// The manifest lacks a required field or carries a malformed one.
    #[error("malformed manifest: {0}")]
    MalformedManifest(String),
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

/// Verify one update against a signed manifest: the signature
/// authenticates the manifest, the manifest pins what the artefact is.
///
/// A signature alone proves the bytes are unchanged since the signer
/// signed them; it says nothing about *which* bytes they are. A signed
/// manifest for version A on target B, applied where version C on
/// target D was expected, is a perfectly valid signature over the wrong
/// update - a rollback or a cross-platform substitution. This checks
/// version, target, and digest against the caller's expectations before
/// the signature is even asked about.
///
/// # Errors
///
/// [`UpdateError::MalformedManifest`] when a required field is absent;
/// [`UpdateError::VersionMismatch`], [`UpdateError::TargetMismatch`],
/// and [`UpdateError::DigestMismatch`] when the manifest names
/// something other than what was expected; [`UpdateError::BadVersion`]
/// when the manifest's version is not semver; [`UpdateError::BadSignature`]
/// when the signature over the manifest does not verify.
pub fn verify_manifest(
    manifest: &str,
    artefact: &[u8],
    expected_version: &str,
    expected_target: &str,
    public_key: &str,
    signature: &str,
) -> Result<(), UpdateError> {
    let parsed: serde_json::Value =
        serde_json::from_str(manifest).map_err(|error| UpdateError::MalformedManifest(error.to_string()))?;
    let offered_version = parsed
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| UpdateError::MalformedManifest("no version field".to_owned()))?;
    let offered_target = parsed
        .get("target")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| UpdateError::MalformedManifest("no target field".to_owned()))?;
    let pinned = parsed
        .get("sha256")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| UpdateError::MalformedManifest("no sha256 field".to_owned()))?;

    parse_version(offered_version)?;
    if offered_version != expected_version {
        return Err(UpdateError::VersionMismatch {
            offered: offered_version.to_owned(),
            expected: expected_version.to_owned(),
        });
    }
    if offered_target != expected_target {
        return Err(UpdateError::TargetMismatch {
            offered: offered_target.to_owned(),
            expected: expected_target.to_owned(),
        });
    }

    let computed = digest_sha256(artefact);
    if computed != pinned {
        return Err(UpdateError::DigestMismatch { computed, pinned: pinned.to_owned() });
    }

    verify(manifest.as_bytes(), public_key, signature)
}

/// The lowercase hex SHA-256 of `data`.
fn digest_sha256(data: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_with_a_leading_v_parses() {
        let parsed = parse_version("v0.1.0").expect("strip v");
        assert_eq!(parsed, semver::Version::new(0, 1, 0));
        let bare = parse_version("1.2.3").expect("bare");
        assert_eq!(bare, semver::Version::new(1, 2, 3));
        assert!(parse_version("not-a-version").is_err());
    }

    #[test]
    fn a_tampered_payload_fails_verification() {
        let key = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
        let result = verify(b"tampered", key, "untrusted comment: signature\ninvalid-base64");
        assert!(matches!(result, Err(UpdateError::BadSignature(_))));
    }

    #[test]
    fn a_garbage_key_is_a_signature_error_not_a_panic() {
        let result = verify(b"data", "not-a-key", "not-a-signature");
        assert!(matches!(result, Err(UpdateError::BadSignature(_))));
    }

    fn manifest(version: &str, target: &str, sha256: &str) -> String {
        serde_json::json!({"version": version, "target": target, "sha256": sha256}).to_string()
    }

    #[test]
    fn a_manifest_for_another_version_is_refused_before_its_signature() {
        let key = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
        let offered = manifest("0.1.0", "x86_64-unknown-linux-gnu", "00");
        let error = verify_manifest(&offered, b"artefact", "0.2.0", "x86_64-unknown-linux-gnu", key, "x")
            .expect_err("wrong version");
        let UpdateError::VersionMismatch { offered, expected } = error else {
            panic!("expected VersionMismatch, got {error:?}");
        };
        assert_eq!(offered, "0.1.0");
        assert_eq!(expected, "0.2.0");
    }

    #[test]
    fn a_manifest_for_another_target_is_refused_before_its_signature() {
        let key = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
        let offered = manifest("0.1.0", "aarch64-apple-darwin", "00");
        let error = verify_manifest(&offered, b"artefact", "0.1.0", "x86_64-unknown-linux-gnu", key, "x")
            .expect_err("wrong target");
        assert!(matches!(error, UpdateError::TargetMismatch { .. }), "{error}");
    }

    #[test]
    fn an_artefact_that_does_not_match_the_pinned_digest_is_refused() {
        let key = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
        let pinned = digest_sha256(b"the real artefact");
        let offered = manifest("0.1.0", "x86_64-unknown-linux-gnu", &pinned);
        let error =
            verify_manifest(&offered, b"a tampered artefact", "0.1.0", "x86_64-unknown-linux-gnu", key, "x")
                .expect_err("wrong bytes");
        assert!(matches!(error, UpdateError::DigestMismatch { .. }), "{error}");
    }

    #[test]
    fn a_manifest_missing_a_field_is_malformed_not_a_crash() {
        let key = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
        for text in ["{}", "{\"version\":\"0.1.0\"}", "not json"] {
            let error = verify_manifest(text, b"a", "0.1.0", "t", key, "x").expect_err("malformed");
            assert!(matches!(error, UpdateError::MalformedManifest(_)), "{error}");
        }
    }
}
