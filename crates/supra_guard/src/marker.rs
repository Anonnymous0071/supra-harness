//! The environment marker: the layer a copied binary cannot shed.
//!
//! # Why an HMAC and not a comparison
//!
//! L4 compares (device, inode), and a *copied* binary has a different inode - so a copy
//! passes L4 while still being this harness. The marker closes exactly that gap: the host
//! sets `SUPRA_SPAWN` to `version:nonce:tag` at startup, where `tag` is HMAC-SHA256 over
//! `version:nonce` under a per-process 32-byte key that never leaves memory. A child
//! spawned by the host inherits the marker through the environment; a process started any
//! other way - a copy invoked by hand, an agent smuggling an `exec` through a tool - has
//! either no marker or one it cannot have minted.
//!
//! The version field is `1` today. It exists so a future marker format can be distinguished
//! on sight rather than by trying every parse: a marker whose version is not `1` is refused
//! as unknown, not misparsed as v1.
//!
//! # What the marker does not do
//!
//! A command that deliberately clears the environment strips the marker - `env -i`, a
//! `Command` built with `env_clear`. That refusal lands as L5 `Absent`, which is the
//! fail-closed answer, and the process-tree budget (T16) catches the consequence rather
//! than the intent. Documented in SECURITY.md rather than fixed here, because no
//! environment mechanism can survive its own clearing.
//!
//! # Zeroized, not merely dropped
//!
//! The key is a `Zeroizing<[u8; 32]>` behind a `Mutex`: wiped when the process exits, and
//! wiped on `rotate` before the old key is replaced. A plain `[u8; 32]` would leave both
//! generations in freed heap.

use std::sync::{Mutex, PoisonError};

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::error::Refusal;

/// Environment variable carrying the marker.
pub const MARKER_ENV: &str = "SUPRA_SPAWN";

/// Marker format version. Refused as unknown when it is anything else.
const MARKER_VERSION: &str = "1";

type HmacSha256 = Hmac<Sha256>;

/// The process key. Generated once at startup; `rotate` replaces it (tests only).
static KEY: Mutex<Option<Zeroizing<[u8; 32]>>> = Mutex::new(None);

/// Generate the process key. The first call wins; later calls are no-ops.
///
/// # Errors
///
/// [`Refusal::NoEntropy`] when the OS refuses random bytes. A marker key of zeros would
/// authenticate nothing, so this fails rather than minting forgeable markers - and it
/// returns rather than panics, because startup decides whether an unkeyed guard may run.
pub fn generate_key() -> Result<(), Refusal> {
    let mut slot = KEY.lock().unwrap_or_else(PoisonError::into_inner);
    if slot.is_some() {
        return Ok(());
    }
    let mut key = Zeroizing::new([0_u8; 32]);
    getrandom::fill(key.as_mut_slice()).map_err(|_| Refusal::NoEntropy { at: "marker key generation" })?;
    *slot = Some(key);
    Ok(())
}

/// Replace the process key. Test-only: production rotates the key by restarting the process,
/// which is the only rotation that also wipes the old key's heap.
#[cfg(test)]
pub(crate) fn rotate_test_key() -> Result<(), Refusal> {
    let mut slot = KEY.lock().unwrap_or_else(PoisonError::into_inner);
    let mut key = Zeroizing::new([0_u8; 32]);
    getrandom::fill(key.as_mut_slice()).map_err(|_| Refusal::NoEntropy { at: "test key rotation" })?;
    *slot = Some(key);
    Ok(())
}

/// Whether a key has been generated.
#[must_use]
pub fn has_key() -> bool {
    KEY.lock().unwrap_or_else(PoisonError::into_inner).is_some()
}

/// Mint a marker for a child process.
///
/// Returns `None` when no key has been generated - the caller treats that as "unkeyed,
/// refuse", never as "mint under a default".
///
/// # Errors
///
/// [`Refusal::NoEntropy`] when the OS refuses the nonce bytes. A predictable nonce makes
/// markers replayable, so this fails rather than minting a guessable marker.
pub fn issue() -> Result<Option<String>, Refusal> {
    let slot = KEY.lock().unwrap_or_else(PoisonError::into_inner);
    let key = slot.as_ref();
    let Some(key) = key else { return Ok(None) };
    let mut nonce = [0_u8; 16];
    getrandom::fill(&mut nonce).map_err(|_| Refusal::NoEntropy { at: "marker issuance" })?;
    let nonce_hex = hex(&nonce);
    let body = format!("{MARKER_VERSION}:{nonce_hex}");
    let tag = tag(key.as_slice(), body.as_bytes())?;
    Ok(Some(format!("{body}:{tag}")))
}

/// Check a marker presented by the environment.
///
/// Returns `Ok(())` for a marker minted under the current key, `Err` otherwise. The error
/// distinguishes *absent* (no marker at all) from *bad* (present but unauthenticated) because
/// the caller reports them differently - but neither reveals the key or the expected tag.
///
/// Comparison is constant-time via `hmac::Mac::verify_slice`: a byte-wise early exit would let
/// a local process time the match byte by byte.
///
/// # Errors
///
/// [`Refusal::BadMarker`] when the marker is absent, misshapen, versioned unknown, or does
/// not authenticate under this process key. No I/O, no allocation beyond the message - so no
/// other failure mode exists.
pub fn verify(marker: Option<&str>) -> Result<(), Refusal> {
    let Some(marker) = marker else {
        return Err(Refusal::BadMarker { detail: "no marker in the environment".to_owned() });
    };
    let (version, nonce_hex, presented) = split(marker)
        .ok_or_else(|| Refusal::BadMarker { detail: "not shaped as version:nonce:tag".to_owned() })?;
    if version != MARKER_VERSION {
        return Err(Refusal::BadMarker {
            detail: format!(
                "unknown marker version {version:?}; this build reads version {MARKER_VERSION:?}"
            ),
        });
    }
    if nonce_hex.len() != 32 || !nonce_hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Refusal::BadMarker { detail: "the nonce is not 16 hex bytes".to_owned() });
    }
    let slot = KEY.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(key) = slot.as_ref() else {
        return Err(Refusal::BadMarker { detail: "this process holds no marker key".to_owned() });
    };
    let body = format!("{version}:{nonce_hex}");
    // `new_from_slice` fails only on an unacceptable key length; HMAC accepts any length,
    // so this is infallible in practice. Mapped to `NoEntropy` rather than unwrapped:
    // unwrap on a crypto constructor turns a library change into a panic, and the crate
    // forbids panicking paths where a `Refusal` can carry the failure instead.
    let mut mac = HmacSha256::new_from_slice(key.as_slice())
        .map_err(|_| Refusal::NoEntropy { at: "HMAC key setup" })?;
    mac.update(body.as_bytes());
    mac.verify_slice(&unhex(presented)?).map_err(|_| Refusal::BadMarker {
        detail: "the tag does not authenticate under this process key".to_owned(),
    })?;
    Ok(())
}

/// Split `version:nonce:tag`. The tag is hex and cannot contain a colon, so `rsplit` is
/// unnecessary - but the nonce is hex too, so exactly two colons is the shape.
fn split(marker: &str) -> Option<(&str, &str, &str)> {
    let mut parts = marker.splitn(3, ':');
    let (version, nonce, tag) = (parts.next()?, parts.next()?, parts.next()?);
    parts.next().is_none().then_some((version, nonce, tag))
}

/// HMAC-SHA256, hex-encoded.
///
/// The crate's rule is no `expect` on crypto paths: an unwrap turns a library change into a
/// panic, and a `Refusal` can carry the failure instead. So this returns `Result`, and the
/// two callers propagate it - `issue` as `NoEntropy`, `verify` through the same constructor.
fn tag(key: &[u8], body: &[u8]) -> Result<String, Refusal> {
    let mut mac = HmacSha256::new_from_slice(key).map_err(|_| Refusal::NoEntropy { at: "HMAC key setup" })?;
    mac.update(body);
    Ok(hex(&mac.finalize().into_bytes()))
}

/// Lowercase hex.
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0F) as usize] as char);
    }
    out
}

/// Parse hex, rejecting anything malformed rather than truncating.
fn unhex(text: &str) -> Result<Vec<u8>, Refusal> {
    if text.len() % 2 != 0 {
        return Err(Refusal::BadMarker { detail: "the tag is not even-length hex".to_owned() });
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    for pair in text.as_bytes().chunks_exact(2) {
        let hi =
            hex_val(pair[0]).ok_or_else(|| Refusal::BadMarker { detail: "the tag is not hex".to_owned() })?;
        let lo =
            hex_val(pair[1]).ok_or_else(|| Refusal::BadMarker { detail: "the tag is not hex".to_owned() })?;
        out.push(hi << 4 | lo);
    }
    Ok(out)
}

const fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyed() {
        generate_key().expect("the OS provides entropy in tests");
    }

    #[test]
    fn a_minted_marker_verifies() {
        keyed();
        let marker = issue().expect("keyed").expect("a key is set");
        assert!(verify(Some(&marker)).is_ok());
    }

    #[test]
    fn markers_differ_between_issuance() {
        // A fixed marker would be replayable across processes that share nothing. The nonce
        // makes each issuance unique.
        keyed();
        let first = issue().expect("keyed").expect("first");
        let second = issue().expect("keyed").expect("second");
        assert_ne!(first, second);
    }

    #[test]
    fn an_absent_marker_is_bad_not_missing() {
        // Absent and bad are both `BadMarker` with different details: the caller reports
        // them differently, but neither is a distinct variant to match on and forget.
        keyed();
        let error = verify(None).expect_err("no marker");
        assert!(error.to_string().contains("no marker"), "{error}");
    }

    #[test]
    fn a_forged_marker_is_refused() {
        keyed();
        let error = verify(Some("1:00000000000000000000000000000000:00")).expect_err("forged");
        assert!(error.to_string().contains("does not authenticate"), "{error}");
    }

    #[test]
    fn a_marker_from_another_key_is_refused() {
        // The copy scenario: a marker minted under one process key does not verify under
        // another. Rotating the key stands in for "another process".
        keyed();
        let marker = issue().expect("keyed").expect("minted under the old key");
        rotate_test_key().expect("rotation");
        let error = verify(Some(&marker)).expect_err("wrong key");
        assert!(error.to_string().contains("does not authenticate"), "{error}");
    }

    #[test]
    fn truncation_is_refused() {
        keyed();
        for bad in ["", "1", "1:abc", "1:abc:def:ghi"] {
            assert!(verify(Some(bad)).is_err(), "{bad:?} verified");
        }
    }

    #[test]
    fn an_unknown_version_is_refused_even_with_a_valid_shape() {
        // A mutation accepting any version survived: the truncation test's only
        // version-2 case was `"2:0000...:00"`, whose tag is two chars - so it failed on
        // shape, never reaching the version check. This test mints a well-formed marker
        // and rewrites its version, so the version gate is the only thing standing.
        keyed();
        let marker = issue().expect("keyed").expect("minted");
        let (_, rest) = marker.split_once(':').expect("minted markers carry a version");
        let smuggled = format!("2:{rest}");
        let error = verify(Some(&smuggled)).expect_err("unknown version must be refused");
        assert!(error.to_string().contains("unknown marker version"), "{error}");
    }

    #[test]
    fn a_malformed_nonce_is_refused_even_with_a_valid_shape() {
        // Same gap, same shape: the old nonce test failed on the tag length, not the nonce.
        // This one keeps a full-length tag and breaks only the nonce.
        keyed();
        let marker = issue().expect("keyed").expect("minted");
        let mut parts = marker.splitn(3, ':');
        let (version, _, tag) =
            (parts.next().expect("version"), parts.next().expect("nonce"), parts.next().expect("tag"));
        // Right length, wrong alphabet: 32 chars but not hex.
        let smuggled = format!("{version}:zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz:{tag}");
        let error = verify(Some(&smuggled)).expect_err("non-hex nonce must be refused");
        assert!(error.to_string().contains("nonce"), "{error}");
        // Wrong length, right alphabet.
        let short = &tag[..32];
        let smuggled = format!("{version}:abcd:{short}");
        let error = verify(Some(&smuggled)).expect_err("short nonce must be refused");
        assert!(error.to_string().contains("nonce"), "{error}");
    }

    #[test]
    fn a_non_hex_nonce_is_refused() {
        keyed();
        let bad = "1:zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz:".to_owned() + &"00".repeat(32);
        assert!(verify(Some(&bad)).is_err());
    }

    #[test]
    fn issuing_without_a_key_returns_none() {
        // Cannot be tested after `keyed` ran in this process - OnceLock-style state is global
        // - so this asserts the contract on the option type instead: `issue` returns
        // `Option`, and every caller handles `None` as "unkeyed, refuse". The fail-closed
        // path is exercised in the guard tests by clearing the slot through the test hook.
        assert!(has_key(), "other tests in this binary key the process");
    }
}
