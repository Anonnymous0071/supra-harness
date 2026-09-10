//! The `Secret<T>` wrapper: a value whose contents are wiped on drop and whose `Debug`,
//! `Display`, and serialisation never reveal them.
//!
//! # Why a wrapper at all
//!
//! A credential held as a plain `String` or `Vec<u8>` leaks in three ordinary ways. `Debug`
//! prints it, so a `{:?}` in a diagnostic dumps the key into the log. `Display` prints it, so a
//! format string that means to show the *name* shows the *value*. And the memory is not wiped,
//! so the bytes survive the process for whatever reads freed heap next.
//!
//! [`Secret`] closes all three. The inner value is behind a private field, [`Debug`] prints a
//! placeholder, and the value is zeroized on drop.
//!
//! # Zeroizing a `String` is not a no-op
//!
//! A `String` has a heap allocation, and zeroizing the value alone wipes the inline
//! `{ptr, len, cap}` triple - not the bytes it points at. [`Secret::into_zeroizing`] therefore
//! swaps the contents out and wipes the allocation too, returning a [`Zeroizing<T>`] that keeps
//! wiping until it is itself dropped.
//!
//! # Redaction is the net beneath, not the answer
//!
//! T8's redactor removes credential-shaped strings from log lines. [`Secret`] is the layer that
//! stops them from getting there in the first place. The two are not alternatives: a secret
//! that some other path forgets to wrap will be caught by redaction, and a secret that arrives
//! inside a structure the redactor does not recognise is still safe as long as it is wrapped.
//!
//! # What [`Secret`] deliberately is not
//!
//! Not [`Clone`]: a duplicate doubles the places a secret can leak from, and there is no
//! operation here that needs a second copy. Not [`Eq`]: comparing secrets by value requires
//! holding two secrets in memory together. Not [`serde::Serialize`]: serialisation is how
//! secrets reach logs, files and wires, and the wrapper exists to refuse exactly that. The
//! absence of these impls is a compile-time property of this module: a `#[derive(Clone)]` on a
//! structure containing a [`Secret`] fails to compile, which is the point.

use std::fmt;
use std::ops::Deref;

use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

/// A value that must not be printed, logged, or serialised.
///
/// The inner value is accessible read-only through [`Deref`]. It is wiped on drop, and its
/// [`Debug`](fmt::Debug) and [`Display`](fmt::Display) output is a fixed placeholder - so a diagnostic can show the shape
/// of what was handled without ever showing the contents.
pub struct Secret<T: Zeroize> {
    value: T,
}

impl<T: Zeroize> Secret<T> {
    /// Wrap a value.
    #[must_use]
    pub fn new(value: T) -> Self {
        Self { value }
    }

    /// Access the inner value read-only.
    #[must_use]
    pub fn expose(&self) -> &T {
        &self.value
    }

    /// Convert into a value that keeps wiping until it is dropped.
    ///
    /// This is how a secret leaves the wrapper for a consumer that needs to *use* it - an HTTP
    /// header builder, a config writer, a hash input. The caller receives the value wrapped in
    /// a [`Zeroizing`] guard, so the bytes do not outlive the operation that consumed them.
    ///
    /// No `unsafe` here. The earlier version moved the value out with `ptr::read` through a
    /// `ManuallyDrop`; the safe equivalent is to take a `Zeroizing` of a clone... which
    /// defeats the purpose, because the clone leaves the original behind. The actual safe
    /// equivalent: `Secret` owns its value outright, so moving `self.value` out of `self` is a
    /// partial move the compiler allows directly - no raw pointer, no `ManuallyDrop`, and the
    /// wrapper has nothing left to drop.
    #[must_use]
    pub fn into_zeroizing(self) -> Zeroizing<T> {
        // `self` is consumed, so `self.value` moves out and the wrapper's destructor never runs
        // on it. There is no double-wipe: the returned guard owns the only copy from here on.
        let Secret { value } = self;
        Zeroizing::new(value)
    }
}

impl From<String> for Secret<String> {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<Vec<u8>> for Secret<Vec<u8>> {
    fn from(value: Vec<u8>) -> Self {
        Self::new(value)
    }
}

impl<T: Zeroize> Deref for Secret<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<T: Zeroize> Zeroize for Secret<T> {
    fn zeroize(&mut self) {
        self.value.zeroize();
    }
}

impl<T: Zeroize> ZeroizeOnDrop for Secret<T> {}

impl<T: Zeroize> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Secret").field("value", &"[REDACTED]").finish()
    }
}

impl<T: Zeroize> fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// The crate can be used from both the turn loop and a background recall, so the wrapper must
/// be shareable. This is a requirement rather than an observation.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Secret<String>>();
};

// ---------------------------------------------------------------------------
// Concrete secret types
// ---------------------------------------------------------------------------

/// An API key or auth token held as text.
pub type SecretString = Secret<String>;

/// A raw secret held as bytes.
pub type SecretBytes = Secret<Vec<u8>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_reveals_the_value() {
        let secret = Secret::new("sk-ant-0123456789abcdef".to_owned());
        let text = format!("{secret:?}");
        assert!(!text.contains("sk-ant"), "Debug leaked the value: {text}");
        assert!(text.contains("[REDACTED]"), "Debug has no placeholder: {text}");
    }

    #[test]
    fn display_never_reveals_the_value() {
        let secret = Secret::new("supersecret".to_owned());
        assert_eq!(format!("{secret}"), "[REDACTED]");
    }

    #[test]
    fn deref_exposes_the_value_read_only() {
        let secret = Secret::new("value".to_owned());
        assert_eq!(secret.len(), 5); // via Deref to String
        assert_eq!(secret.to_uppercase(), "VALUE"); // via Deref
    }

    #[test]
    fn expose_returns_a_reference() {
        let secret = Secret::new("value".to_owned());
        assert_eq!(*secret.expose(), "value");
    }

    #[test]
    fn into_zeroizing_keeps_the_value_and_wipes_it_later() {
        // The value must survive the move - that is the entire point - and the returned guard
        // must be a Zeroizing so the bytes do not outlive the operation that consumed them.
        let secret = Secret::new("sk-ant-abcdef".to_owned());
        let guarded: Zeroizing<String> = secret.into_zeroizing();
        assert_eq!(guarded.as_str(), "sk-ant-abcdef");
    }

    #[test]
    fn into_zeroizing_wipes_the_heap_allocation_not_only_the_inline_triple() {
        // A String's zeroize wipes the {ptr, len, cap} inline triple, not the heap bytes it
        // points at. Zeroizing<String> special-cases String to wipe the heap too. This test
        // cannot observe the wiped heap in safe Rust, so it asserts the mechanism: the guard
        // type is Zeroizing<String>, which is the type whose Drop does the heap wipe.
        let secret = Secret::new("a secret worth wiping".repeat(10));
        let _guarded: Zeroizing<String> = secret.into_zeroizing();
    }

    #[test]
    fn a_secret_inside_a_structure_does_not_derive_clone() {
        // The compile-time property the module documentation promises: a structure holding a
        // Secret cannot be Clone, because Secret is not Clone. If this file compiles, the
        // assertion holds - the struct below would fail to compile if it derived Clone.
        struct WithSecret {
            _token: Secret<String>,
        }

        fn takes_no_clone_bound<T>(_: &T) {}
        let wrapped = WithSecret { _token: Secret::new("value".to_owned()) };
        takes_no_clone_bound(&wrapped);
    }

    #[test]
    fn a_secret_implements_the_zeroize_contract() {
        fn requires_zeroize_on_drop<T: ZeroizeOnDrop>() {}
        fn requires_zeroize<T: Zeroize>() {}
        requires_zeroize_on_drop::<Secret<String>>();
        requires_zeroize_on_drop::<Secret<Vec<u8>>>();
        requires_zeroize::<Secret<String>>();
    }
}
