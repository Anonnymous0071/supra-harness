//! Who this process is, established once and never changed.
//!
//! # What "identity" means here
//!
//! Two facts, captured together at startup before any agent code runs:
//!
//! - the canonical path of the running executable ([`self_path`]);
//! - its (device, inode) pair ([`self_identity`]).
//!
//! The path is for L3's name comparison; the (device, inode) pair is for L4's file
//! comparison. Neither is sufficient alone - a path can be a symlink, an inode changes
//! when the binary is copied - and together they still do not cover a copy, which is why
//! L5's marker exists. Identity is what makes the name and file checks *about this
//! process* rather than about a string in a config file.
//!
//! # `OnceLock`, not a parameter
//!
//! The identity is process-global because the question it answers is process-global: "is
//! this command *me*?" Threading it through every call between a CLI flag and a spawn
//! decision would put process identity in every signature in the crate. `OnceLock` holds
//! exactly one initialisation, `establish` is the only writer, and everything else reads.
//!
//! # Fail-closed on absence
//!
//! [`current`] returns `None` until [`establish`] runs. Every layer that needs identity
//! refuses when it is absent (L1/L2 `NoIdentity`) rather than treating "unknown" as
//! "not me". An unidentified guard that allowed spawns would be a guard in name only.

use std::path::PathBuf;
use std::sync::OnceLock;

use supra_ffi::sandbox::{FileIdentity, file_identity, self_identity, self_path};

/// Identity established at startup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessIdentity {
    /// Canonical path of the running executable.
    path: PathBuf,
    /// (device, inode) of the running executable.
    file: FileIdentity,
}

/// The single process identity. `None` until [`establish`] runs.
static IDENTITY: OnceLock<ProcessIdentity> = OnceLock::new();

/// Establish the process identity. The first call wins; later calls return what the first
/// stored. Returns `None` when the platform cannot report either fact - in which case the
/// guard layers refuse everything, because an unidentified process cannot prove a command
/// is not itself.
pub fn establish() -> Option<ProcessIdentity> {
    let identity = ProcessIdentity { path: self_path()?, file: self_identity()? };
    let _ = IDENTITY.set(identity.clone());
    Some(identity)
}

/// The established identity, if any.
#[must_use]
pub fn current() -> Option<ProcessIdentity> {
    IDENTITY.get().cloned()
}

/// Whether identity has been established.
#[must_use]
pub fn is_established() -> bool {
    IDENTITY.get().is_some()
}

impl ProcessIdentity {
    /// Canonical path of the running executable.
    #[must_use]
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// (device, inode) of the running executable.
    #[must_use]
    pub const fn file(&self) -> FileIdentity {
        self.file
    }

    /// Whether `path` resolves to this process's own binary.
    ///
    /// Follows symlinks, like the spawn path will. A path that does not resolve is not this
    /// binary - absence is an answer, not an error - so only resolution *failures* (NUL bytes
    /// and the like) propagate.
    ///
    /// # Errors
    ///
    /// [`supra_ffi::sandbox::Error::InteriorNul`] when the path contains a NUL byte.
    pub fn is_own_file(&self, path: impl AsRef<std::path::Path>) -> Result<bool, supra_ffi::sandbox::Error> {
        Ok(file_identity(path)?.is_some_and(|found| found == self.file))
    }

    /// Whether `name` is this binary's file name.
    ///
    /// Compares only the final component: the spawn may invoke by bare name through `PATH`,
    /// by relative path, or by absolute path, and all three end in the same file name.
    #[must_use]
    pub fn is_own_name(&self, name: &str) -> bool {
        self.path.file_name().is_some_and(|own| own == std::ffi::OsStr::new(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn establishing_twice_keeps_the_first() {
        let first = establish().expect("identity must establish in tests");
        let second = establish().expect("second call returns the stored identity");
        assert_eq!(first, second);
        assert!(is_established());
        assert_eq!(current().expect("established"), first);
    }

    #[test]
    fn the_running_test_binary_is_its_own_file() {
        let identity = establish().expect("identity");
        let own = std::env::current_exe().expect("current exe");
        assert!(identity.is_own_file(&own).expect("resolvable"), "the test binary is itself");
    }

    #[test]
    fn another_file_is_not_own_file() {
        let identity = establish().expect("identity");
        assert!(!identity.is_own_file("/bin/sh").expect("resolvable"));
    }

    #[test]
    fn a_missing_path_is_not_own_file() {
        let identity = establish().expect("identity");
        assert!(!identity.is_own_file("/nonexistent/supra-guard-probe").expect("absence is false"));
    }

    #[test]
    fn own_name_matches_the_final_component_only() {
        let identity = establish().expect("identity");
        let file_name = identity.path().file_name().expect("a file name").to_string_lossy().into_owned();
        assert!(identity.is_own_name(&file_name));
        assert!(!identity.is_own_name(&format!("not-{file_name}")));
        // A full path is not a name.
        assert!(!identity.is_own_name(&identity.path().to_string_lossy()));
    }
}
