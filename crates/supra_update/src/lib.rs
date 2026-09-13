//! Signed, local-only release verification and application.
//!
//! A signature authenticates a strict manifest. The manifest in turn binds one
//! exact archive, target, version, byte length, digest, and executable member.
//! No network is opened here.

#![deny(missing_docs)]
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::unwrap_used))]

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use fs2::FileExt as _;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use thiserror::Error;

/// Maximum accepted compressed bundle size (128 MiB).
pub const MAX_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
/// Maximum accepted executable size (128 MiB).
pub const MAX_EXECUTABLE_BYTES: u64 = 128 * 1024 * 1024;
/// Maximum number of archive headers inspected before refusing the bundle.
pub const MAX_ARCHIVE_ENTRIES: usize = 64;

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
    /// The manifest names a different version than the caller expects.
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
    /// The manifest names a different archive filename.
    #[error("the manifest offers archive {offered:?} where {expected:?} was expected")]
    ArchiveMismatch {
        /// The archive filename the manifest carries.
        offered: String,
        /// The actual archive filename.
        expected: String,
    },
    /// The archive or executable digest does not match the manifest.
    #[error("{subject} digest is {computed} where the manifest pinned {pinned}")]
    DigestMismatch {
        /// Which object failed validation.
        subject: &'static str,
        /// The digest of the bytes on disk.
        computed: String,
        /// The digest the manifest carries.
        pinned: String,
    },
    /// A byte length differs from the signed value.
    #[error("{subject} size is {actual} where the manifest pinned {pinned}")]
    SizeMismatch {
        /// Which object failed validation.
        subject: &'static str,
        /// The measured byte length.
        actual: u64,
        /// The signed byte length.
        pinned: u64,
    },
    /// The manifest is not the strict supported schema.
    #[error("malformed manifest: {0}")]
    MalformedManifest(String),
    /// The archive violates the single-regular-executable contract.
    #[error("unsafe update archive: {0}")]
    UnsafeArchive(String),
    /// The destination changed type or identity during application.
    #[error("unsafe install destination: {0}")]
    UnsafeDestination(String),
    /// Applying updates is not supported on this platform.
    #[error("update apply is not supported on {0}")]
    UnsupportedPlatform(&'static str),
    /// I/O while reading or writing artefacts.
    #[error("I/O error at {path}: {source}")]
    Io {
        /// The path being accessed.
        path: PathBuf,
        /// The underlying failure.
        source: io::Error,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestWire {
    schema: u32,
    version: String,
    target: String,
    archive: String,
    archive_size: u64,
    archive_sha256: String,
    executable: ExecutableWire,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutableWire {
    path: String,
    size: u64,
    sha256: String,
}

/// A verified update manifest whose archive and executable bindings are trusted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedManifest {
    version: semver::Version,
    target: String,
    archive: String,
    archive_size: u64,
    archive_sha256: String,
    executable_path: String,
    executable_size: u64,
    executable_sha256: String,
}

impl VerifiedManifest {
    /// The release version authenticated by the signed manifest.
    #[must_use]
    pub const fn version(&self) -> &semver::Version {
        &self.version
    }

    /// The target triple authenticated by the signed manifest.
    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }

    /// The exact archive filename authenticated by the signed manifest.
    #[must_use]
    pub fn archive(&self) -> &str {
        &self.archive
    }

    /// The exact executable member path authenticated by the signed manifest.
    #[must_use]
    pub fn executable_path(&self) -> &str {
        &self.executable_path
    }
}

/// A complete set of local update inputs.
#[derive(Clone, Copy, Debug)]
pub struct LocalUpdate<'a> {
    /// Path to the local release archive.
    pub archive: &'a Path,
    /// Path to the canonical signed JSON manifest.
    pub manifest: &'a Path,
    /// Path to the manifest's Minisign signature.
    pub signature: &'a Path,
    /// Path to a Minisign public-key file.
    pub public_key: &'a Path,
    /// Target triple the caller expects.
    pub expected_target: &'a str,
}

/// Result of applying an update on Unix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplyOutcome {
    /// Installed release version.
    pub version: semver::Version,
    /// Destination atomically replaced.
    pub install_path: PathBuf,
}

/// Verify a Minisign signature over `data`.
///
/// # Errors
///
/// [`UpdateError::BadSignature`] when the key, signature, or verification fails.
pub fn verify(data: &[u8], public_key: &str, signature: &str) -> Result<(), UpdateError> {
    let key_text = public_key
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("RW"))
        .ok_or_else(|| UpdateError::BadSignature("public key file has no key line".to_owned()))?;
    let key = minisign_verify::PublicKey::from_base64(key_text)
        .map_err(|error| UpdateError::BadSignature(error.to_string()))?;
    let sig = minisign_verify::Signature::decode(signature.trim())
        .map_err(|error| UpdateError::BadSignature(error.to_string()))?;
    key.verify(data, &sig, false).map_err(|error| UpdateError::BadSignature(error.to_string()))
}

/// Parse a version string as semver, stripping one leading `v`.
///
/// # Errors
///
/// [`UpdateError::BadVersion`] when the version is not valid semver.
pub fn parse_version(raw: &str) -> Result<semver::Version, UpdateError> {
    raw.strip_prefix('v')
        .unwrap_or(raw)
        .parse()
        .map_err(|source| UpdateError::BadVersion { version: raw.to_owned(), source })
}

/// Return the current compiler target triple used by release artefacts.
#[must_use]
pub const fn current_target() -> &'static str {
    if cfg!(all(target_arch = "x86_64", target_os = "linux", target_env = "musl")) {
        "x86_64-unknown-linux-musl"
    } else if cfg!(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_arch = "aarch64", target_os = "linux", target_env = "gnu")) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(target_arch = "x86_64", target_os = "macos")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_arch = "aarch64", target_os = "macos")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_arch = "x86_64", target_os = "windows", target_env = "msvc")) {
        "x86_64-pc-windows-msvc"
    } else {
        "unsupported-target"
    }
}

/// Verify a signed, bounded local update bundle without extracting it.
///
/// # Errors
///
/// Returns an [`UpdateError`] for every malformed, mismatched, unsigned, unsafe,
/// oversized, or unreadable input.
pub fn check_local(input: LocalUpdate<'_>) -> Result<VerifiedManifest, UpdateError> {
    let manifest_bytes = read_bounded(input.manifest, 64 * 1024)?;
    let signature = read_text_bounded(input.signature, 64 * 1024)?;
    let public_key = read_text_bounded(input.public_key, 16 * 1024)?;
    verify(&manifest_bytes, &public_key, &signature)?;
    let manifest = parse_manifest(&manifest_bytes)?;
    if manifest.target != input.expected_target {
        return Err(UpdateError::TargetMismatch {
            offered: manifest.target.clone(),
            expected: input.expected_target.to_owned(),
        });
    }
    let actual_name = regular_filename(input.archive)?;
    if manifest.archive != actual_name {
        return Err(UpdateError::ArchiveMismatch {
            offered: manifest.archive.clone(),
            expected: actual_name,
        });
    }
    verify_file_binding(
        input.archive,
        manifest.archive_size,
        &manifest.archive_sha256,
        MAX_ARCHIVE_BYTES,
        "archive",
    )?;
    inspect_archive(input.archive, &manifest, None)?;
    Ok(manifest)
}

/// Verify and atomically apply a signed local update on supported platforms.
///
/// On Unix, the destination is locked, type and identity checked, staged in the
/// same directory with `create_new`, synced, renamed atomically, then its parent
/// directory is synced. Windows deliberately fails closed until an equivalent
/// replacement primitive is implemented.
///
/// # Errors
///
/// Returns an [`UpdateError`] if verification or safe replacement fails.
pub fn apply_local(input: LocalUpdate<'_>, install_path: &Path) -> Result<ApplyOutcome, UpdateError> {
    let manifest = check_local(input)?;
    apply_verified(input.archive, &manifest, install_path)?;
    Ok(ApplyOutcome { version: manifest.version.clone(), install_path: install_path.to_owned() })
}

fn parse_manifest(bytes: &[u8]) -> Result<VerifiedManifest, UpdateError> {
    let wire: ManifestWire =
        serde_json::from_slice(bytes).map_err(|error| UpdateError::MalformedManifest(error.to_string()))?;
    if wire.schema != 1 {
        return Err(UpdateError::MalformedManifest(format!("unsupported schema {}", wire.schema)));
    }
    let version = parse_version(&wire.version)?;
    if wire.version != version.to_string() {
        return Err(UpdateError::MalformedManifest(
            "version must be canonical semver without a v prefix".to_owned(),
        ));
    }
    validate_filename(&wire.archive, "archive")?;
    validate_member_path(&wire.executable.path)?;
    validate_digest(&wire.archive_sha256, "archive_sha256")?;
    validate_digest(&wire.executable.sha256, "executable.sha256")?;
    if wire.archive_size == 0 || wire.archive_size > MAX_ARCHIVE_BYTES {
        return Err(UpdateError::MalformedManifest("archive_size is outside the supported bound".to_owned()));
    }
    if wire.executable.size == 0 || wire.executable.size > MAX_EXECUTABLE_BYTES {
        return Err(UpdateError::MalformedManifest(
            "executable.size is outside the supported bound".to_owned(),
        ));
    }
    if wire.target.is_empty() || wire.target.len() > 128 || !wire.target.bytes().all(is_target_byte) {
        return Err(UpdateError::MalformedManifest("target is not a target triple".to_owned()));
    }
    Ok(VerifiedManifest {
        version,
        target: wire.target,
        archive: wire.archive,
        archive_size: wire.archive_size,
        archive_sha256: wire.archive_sha256,
        executable_path: wire.executable.path,
        executable_size: wire.executable.size,
        executable_sha256: wire.executable.sha256,
    })
}

fn is_target_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
}

fn validate_filename(value: &str, field: &str) -> Result<(), UpdateError> {
    let path = Path::new(value);
    if value.is_empty()
        || value.len() > 255
        || path.file_name().and_then(|name| name.to_str()) != Some(value)
        || path.components().count() != 1
    {
        return Err(UpdateError::MalformedManifest(format!("{field} must be one UTF-8 filename")));
    }
    Ok(())
}

fn validate_member_path(value: &str) -> Result<(), UpdateError> {
    if value.is_empty() || value.len() > 512 || value.contains('\\') || value.starts_with('/') {
        return Err(UpdateError::MalformedManifest(
            "executable.path is not a relative portable path".to_owned(),
        ));
    }
    let path = Path::new(value);
    if path.components().any(|component| !matches!(component, Component::Normal(_))) {
        return Err(UpdateError::MalformedManifest(
            "executable.path contains traversal or a prefix".to_owned(),
        ));
    }
    Ok(())
}

fn validate_digest(value: &str, field: &str) -> Result<(), UpdateError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(UpdateError::MalformedManifest(format!(
            "{field} must be 64 lowercase hexadecimal digits"
        )));
    }
    Ok(())
}

fn regular_filename(path: &Path) -> Result<String, UpdateError> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| UpdateError::MalformedManifest("archive path has no UTF-8 filename".to_owned()))
}

fn io_at(path: &Path, source: io::Error) -> UpdateError {
    UpdateError::Io { path: path.to_owned(), source }
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, UpdateError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| io_at(path, error))?;
    if !metadata.file_type().is_file() {
        return Err(UpdateError::UnsafeDestination(format!("{} is not a regular file", path.display())));
    }
    if metadata.len() > limit {
        return Err(UpdateError::SizeMismatch { subject: "input", actual: metadata.len(), pinned: limit });
    }
    let file = File::open(path).map_err(|error| io_at(path, error))?;
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(limit + 1).read_to_end(&mut bytes).map_err(|error| io_at(path, error))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        return Err(UpdateError::SizeMismatch {
            subject: "input",
            actual: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            pinned: limit,
        });
    }
    Ok(bytes)
}

fn read_text_bounded(path: &Path, limit: u64) -> Result<String, UpdateError> {
    String::from_utf8(read_bounded(path, limit)?)
        .map_err(|error| UpdateError::MalformedManifest(format!("{} is not UTF-8: {error}", path.display())))
}

fn verify_file_binding(
    path: &Path,
    pinned_size: u64,
    pinned_digest: &str,
    limit: u64,
    subject: &'static str,
) -> Result<(), UpdateError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| io_at(path, error))?;
    if !metadata.file_type().is_file() {
        return Err(UpdateError::UnsafeDestination(format!("{} is not a regular file", path.display())));
    }
    if metadata.len() != pinned_size {
        return Err(UpdateError::SizeMismatch { subject, actual: metadata.len(), pinned: pinned_size });
    }
    if metadata.len() > limit {
        return Err(UpdateError::SizeMismatch { subject, actual: metadata.len(), pinned: limit });
    }
    let bytes = read_bounded(path, limit)?;
    let computed = digest_sha256(&bytes);
    if computed != pinned_digest {
        return Err(UpdateError::DigestMismatch { subject, computed, pinned: pinned_digest.to_owned() });
    }
    Ok(())
}

fn inspect_archive(
    archive_path: &Path,
    manifest: &VerifiedManifest,
    mut output: Option<&mut File>,
) -> Result<(), UpdateError> {
    let file = File::open(archive_path).map_err(|error| io_at(archive_path, error))?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let entries = archive.entries().map_err(|error| io_at(archive_path, error))?;
    let mut seen = HashSet::new();
    let mut found = false;
    for (index, item) in entries.enumerate() {
        if index >= MAX_ARCHIVE_ENTRIES {
            return Err(UpdateError::UnsafeArchive(format!("more than {MAX_ARCHIVE_ENTRIES} entries")));
        }
        let mut entry = item.map_err(|error| io_at(archive_path, error))?;
        let path = entry.path().map_err(|error| io_at(archive_path, error))?;
        let path_text = path
            .to_str()
            .ok_or_else(|| UpdateError::UnsafeArchive("member path is not UTF-8".to_owned()))?
            .to_owned();
        validate_archive_path(&path_text)?;
        if !seen.insert(path_text.clone()) {
            return Err(UpdateError::UnsafeArchive(format!("duplicate member {path_text:?}")));
        }
        if !entry.header().entry_type().is_file() {
            return Err(UpdateError::UnsafeArchive(format!("non-regular member {path_text:?}")));
        }
        if path_text != manifest.executable_path {
            return Err(UpdateError::UnsafeArchive(format!("unsigned extra member {path_text:?}")));
        }
        if found {
            return Err(UpdateError::UnsafeArchive("executable appears more than once".to_owned()));
        }
        let header_size = entry.size();
        if header_size != manifest.executable_size {
            return Err(UpdateError::SizeMismatch {
                subject: "executable",
                actual: header_size,
                pinned: manifest.executable_size,
            });
        }
        let mut hasher = Sha256::new();
        let mut copied = 0_u64;
        let mut buffer = vec![0_u8; 32 * 1024].into_boxed_slice();
        loop {
            let read = entry.read(&mut buffer).map_err(|error| io_at(archive_path, error))?;
            if read == 0 {
                break;
            }
            copied = copied.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
            if copied > manifest.executable_size || copied > MAX_EXECUTABLE_BYTES {
                return Err(UpdateError::UnsafeArchive("executable exceeds its signed size".to_owned()));
            }
            hasher.update(&buffer[..read]);
            if let Some(destination) = output.as_deref_mut() {
                destination.write_all(&buffer[..read]).map_err(|error| io_at(archive_path, error))?;
            }
        }
        if copied != manifest.executable_size {
            return Err(UpdateError::SizeMismatch {
                subject: "executable",
                actual: copied,
                pinned: manifest.executable_size,
            });
        }
        let computed = lower_hex(hasher.finalize().as_slice());
        if computed != manifest.executable_sha256 {
            return Err(UpdateError::DigestMismatch {
                subject: "executable",
                computed,
                pinned: manifest.executable_sha256.clone(),
            });
        }
        found = true;
    }
    if !found {
        return Err(UpdateError::UnsafeArchive("signed executable member is absent".to_owned()));
    }
    Ok(())
}

fn validate_archive_path(value: &str) -> Result<(), UpdateError> {
    if value.contains('\\') || value.starts_with('/') {
        return Err(UpdateError::UnsafeArchive(format!("non-portable member path {value:?}")));
    }
    if Path::new(value).components().any(|component| !matches!(component, Component::Normal(_))) {
        return Err(UpdateError::UnsafeArchive(format!("traversal or prefixed member path {value:?}")));
    }
    Ok(())
}

fn digest_sha256(data: &[u8]) -> String {
    lower_hex(Sha256::digest(data).as_slice())
}

fn lower_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(unix)]
fn apply_verified(
    archive: &Path,
    manifest: &VerifiedManifest,
    install_path: &Path,
) -> Result<(), UpdateError> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

    let parent = install_path.parent().filter(|path| !path.as_os_str().is_empty()).ok_or_else(|| {
        UpdateError::UnsafeDestination("install path must have a parent directory".to_owned())
    })?;
    let parent_metadata = fs::symlink_metadata(parent).map_err(|error| io_at(parent, error))?;
    if !parent_metadata.file_type().is_dir() {
        return Err(UpdateError::UnsafeDestination(format!("{} is not a directory", parent.display())));
    }
    let name = install_path.file_name().and_then(|value| value.to_str()).ok_or_else(|| {
        UpdateError::UnsafeDestination("install path must end in a UTF-8 filename".to_owned())
    })?;
    let lock_path = parent.join(format!(".{name}.update.lock"));
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path)
        .map_err(|error| io_at(&lock_path, error))?;
    lock.lock_exclusive().map_err(|error| io_at(&lock_path, error))?;

    let before = destination_identity(install_path)?;
    let staged_path = create_staging_path(parent, name)?;
    let mut staged = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&staged_path)
        .map_err(|error| io_at(&staged_path, error))?;
    let result = (|| {
        inspect_archive(archive, manifest, Some(&mut staged))?;
        staged
            .set_permissions(fs::Permissions::from_mode(0o755))
            .map_err(|error| io_at(&staged_path, error))?;
        staged.sync_all().map_err(|error| io_at(&staged_path, error))?;
        let after = destination_identity(install_path)?;
        if before != after {
            return Err(UpdateError::UnsafeDestination(
                "destination identity changed while locked".to_owned(),
            ));
        }
        fs::rename(&staged_path, install_path).map_err(|error| io_at(install_path, error))?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| io_at(parent, error))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&staged_path);
    }
    result
}

#[cfg(unix)]
fn destination_identity(path: &Path) -> Result<Option<(u64, u64)>, UpdateError> {
    use std::os::unix::fs::MetadataExt as _;

    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(Some((metadata.dev(), metadata.ino()))),
        Ok(_) => Err(UpdateError::UnsafeDestination(format!("{} is not a regular file", path.display()))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_at(path, error)),
    }
}

#[cfg(unix)]
fn create_staging_path(parent: &Path, name: &str) -> Result<PathBuf, UpdateError> {
    for nonce in 0..128_u32 {
        let path = parent.join(format!(".{name}.update.{}.{}", std::process::id(), nonce));
        if !path.exists() {
            return Ok(path);
        }
    }
    Err(UpdateError::UnsafeDestination("could not allocate a same-directory staging name".to_owned()))
}

#[cfg(windows)]
fn apply_verified(
    _archive: &Path,
    _manifest: &VerifiedManifest,
    _install_path: &Path,
) -> Result<(), UpdateError> {
    Err(UpdateError::UnsupportedPlatform("Windows"))
}

#[cfg(not(any(unix, windows)))]
fn apply_verified(
    _archive: &Path,
    _manifest: &VerifiedManifest,
    _install_path: &Path,
) -> Result<(), UpdateError> {
    Err(UpdateError::UnsupportedPlatform(std::env::consts::OS))
}

#[cfg(test)]
mod tests;
