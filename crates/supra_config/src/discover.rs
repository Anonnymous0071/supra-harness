//! Finding the files, and reading them safely.
//!
//! # Where files live
//!
//! | Layer | Path |
//! | ----- | ---- |
//! | user | `$XDG_CONFIG_HOME/supra/config.toml`, else `$HOME/.config/supra/config.toml` |
//! | project | the nearest `.supra/config.toml` at or above the working directory |
//!
//! Discovery is explicit rather than delegated to a crate, because the rules are
//! four lines long and a dependency here would be one more thing whose behaviour
//! has to be verified on three platforms.
//!
//! # The permission check is on the handle, not the path
//!
//! Checking a path's mode and then opening it is a time-of-check-to-time-of-use
//! race: between the two, the file can be replaced. [`read_private`] opens first and
//! inspects the **open handle**, so the mode it reports and the bytes it reads
//! belong to the same file. That is not a hypothetical distinction for a file whose
//! whole reason for being checked is that it may hold something sensitive.
//!
//! Symlinks are followed deliberately. `File::open` resolves the link and the
//! handle's metadata describes the *target*, which is exactly the object whose
//! permissions matter; refusing links would break the common case of a config
//! directory kept in a dotfiles repository.
//!
//! # Why a regular-file check, and why it happens before the open
//!
//! Opening a FIFO read-only **blocks until a writer appears**. So the file type
//! cannot be checked on the handle the way the mode is: by the time there is a
//! handle, a FIFO has already hung the process. Without this check, pointing the
//! user config at a pipe turns a misconfiguration into a hang with no diagnosis -
//! the worst possible failure for something that runs before the UI exists.
//!
//! The type is therefore checked with a `stat` on the path *before* opening, which
//! does not block, and then re-checked on the handle afterwards. Those two checks
//! answer different questions and both are needed:
//!
//! - the pre-flight `stat` is about **availability**: do not open something that
//!   will not return;
//! - the handle check is about **correctness**: the bytes read and the metadata
//!   inspected describe the same object.
//!
//! A race remains between the two, and it is worth being precise about how much it
//! matters: exploiting it means replacing the file between the `stat` and the
//! `open`, which requires write access to the configuration directory - and anyone
//! with that can simply write whatever configuration they like. The window does not
//! widen the threat. The **permission** check, which is the security-relevant one,
//! is on the handle regardless.

use std::fs::{File, Metadata};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use crate::error::ConfigError;
use crate::source::ConfigSource;

/// Directory name used under the config root and inside a project.
pub const CONFIG_DIR: &str = "supra";

/// File name for every layer that lives on disk.
pub const CONFIG_FILE: &str = "config.toml";

/// Directory a project keeps its configuration in.
pub const PROJECT_DIR: &str = ".supra";

/// Path to the user's configuration file, whether or not it exists.
///
/// `$XDG_CONFIG_HOME` when set and absolute, otherwise `$HOME/.config`. A relative
/// `XDG_CONFIG_HOME` is ignored rather than resolved: the specification requires an
/// absolute path, and resolving it against the working directory would make the
/// user's configuration depend on where supra was started.
#[must_use]
pub fn user_config_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(value) if !value.is_empty() && Path::new(&value).is_absolute() => PathBuf::from(value),
        _ => {
            let home = std::env::var_os("HOME")?;
            if home.is_empty() {
                return None;
            }
            PathBuf::from(home).join(".config")
        }
    };
    Some(base.join(CONFIG_DIR).join(CONFIG_FILE))
}

/// Path to the nearest project configuration file at or above `start`.
///
/// Walks upward and stops at the first hit, so the innermost project wins - the same
/// rule git uses for its own configuration, and the one a reader already expects.
/// Returns `None` when the walk reaches the filesystem root without finding one.
#[must_use]
pub fn project_config_path(start: &Path) -> Option<PathBuf> {
    let mut directory = Some(start);
    while let Some(current) = directory {
        let candidate = current.join(PROJECT_DIR).join(CONFIG_FILE);
        if candidate.exists() {
            return Some(candidate);
        }
        directory = current.parent();
    }
    None
}

/// Open a path that must be a regular file, without risking a block.
///
/// Returns `Ok(None)` when the file does not exist, because an absent layer is not
/// an error - it is silence, and silence is what per-field precedence is built on.
fn open_regular(layer: ConfigSource, path: &Path) -> Result<Option<(File, Metadata)>, ConfigError> {
    // Pre-flight: `stat` does not open, so it returns even for a FIFO with no writer.
    match std::fs::metadata(path) {
        Ok(metadata) if !metadata.is_file() => {
            return Err(ConfigError::NotARegularFile { layer, path: path.to_path_buf() });
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ConfigError::Unreadable { layer, path: path.to_path_buf(), source });
        }
    }

    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ConfigError::Unreadable { layer, path: path.to_path_buf(), source });
        }
    };

    // Re-check on the handle. The bytes about to be read and the metadata about to be
    // inspected must describe the same object.
    let metadata = file.metadata().map_err(|source| ConfigError::Unreadable {
        layer,
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(ConfigError::NotARegularFile { layer, path: path.to_path_buf() });
    }

    Ok(Some((file, metadata)))
}

/// Read a file that must not be readable by anyone but its owner.
///
/// Returns `Ok(None)` when the file does not exist.
///
/// # Errors
///
/// [`ConfigError::NotARegularFile`] for anything that is not a regular file,
/// [`ConfigError::TooPermissive`] when group or other can read it,
/// [`ConfigError::Unreadable`] for any other I/O failure.
pub fn read_private(layer: ConfigSource, path: &Path) -> Result<Option<String>, ConfigError> {
    let Some((file, metadata)) = open_regular(layer, path)? else {
        return Ok(None);
    };
    check_private(layer, path, &metadata)?;
    read_to_string(layer, path, file).map(Some)
}

/// Read a file whose contents are not sensitive.
///
/// Used for the project layer, which is normally committed and therefore cannot be
/// held to `0600`. Its safety comes from the schema instead: the project layer may
/// not name a provider, an endpoint, or a credential source, so a world-readable
/// project file exposes nothing worth hiding.
///
/// # Errors
///
/// As [`read_private`], minus the permission check.
pub fn read_shared(layer: ConfigSource, path: &Path) -> Result<Option<String>, ConfigError> {
    let Some((file, _)) = open_regular(layer, path)? else {
        return Ok(None);
    };
    read_to_string(layer, path, file).map(Some)
}

fn read_to_string(layer: ConfigSource, path: &Path, mut file: File) -> Result<String, ConfigError> {
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(|source| ConfigError::Unreadable {
        layer,
        path: path.to_path_buf(),
        source,
    })?;
    Ok(text)
}

/// Refuse a file that group or other can read.
///
/// Unix only. On other platforms the check is a no-op and says so rather than
/// pretending: Windows access control is an ACL rather than a mode, so a mode
/// comparison there would be theatre. T12's keyring backing is what carries the
/// weight on those platforms.
#[cfg(unix)]
fn check_private(layer: ConfigSource, path: &Path, metadata: &Metadata) -> Result<(), ConfigError> {
    use std::os::unix::fs::MetadataExt as _;

    let mode = metadata.mode() & 0o777;
    // Only the read, write, and execute bits of group and other matter. The owner's
    // bits are their business, and the setuid family cannot make a file readable.
    //
    // Written as a mask rather than clippy's suggested `trailing_zeros() >= 6`: that
    // is the same test spelled in a way that no longer looks like a permission check.
    // `0o077` is the octal for "group and other", which is what the sentence above
    // says.
    #[allow(clippy::verbose_bit_mask, reason = "the mask names the permission bits it tests")]
    if mode & 0o077 == 0 {
        return Ok(());
    }
    Err(ConfigError::TooPermissive { layer, path: path.to_path_buf(), mode })
}

#[cfg(not(unix))]
fn check_private(_layer: ConfigSource, _path: &Path, _metadata: &Metadata) -> Result<(), ConfigError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A scratch directory that removes itself.
    ///
    /// Hand-rolled rather than a dependency: it is six lines, and T4 recorded what
    /// happens when a test leaves state behind - a workspace-write test passed for
    /// the wrong reason because a file persisted between runs.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("supra-config-{name}"));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("scratch directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
    }

    #[test]
    fn an_absent_file_is_silence_not_an_error() {
        let scratch = Scratch::new("absent");
        let missing = scratch.path().join("nope.toml");
        assert_eq!(read_private(ConfigSource::User, &missing).expect("absent is fine"), None);
        assert_eq!(read_shared(ConfigSource::Project, &missing).expect("absent is fine"), None);
    }

    #[test]
    #[cfg(unix)]
    fn a_private_file_is_read() {
        let scratch = Scratch::new("private");
        let path = scratch.path().join("config.toml");
        fs::write(&path, "[cohort]\nlimit = 4\n").expect("write");
        set_mode(&path, 0o600);

        let text = read_private(ConfigSource::User, &path).expect("readable").expect("present");
        assert!(text.contains("limit = 4"));
    }

    #[test]
    #[cfg(unix)]
    fn a_group_readable_file_is_refused_with_the_chmod_to_run() {
        let scratch = Scratch::new("group-readable");
        let path = scratch.path().join("config.toml");
        fs::write(&path, "[cohort]\nlimit = 4\n").expect("write");
        set_mode(&path, 0o640);

        let error = read_private(ConfigSource::User, &path).expect_err("too permissive");
        let text = error.to_string();
        assert!(text.contains("0640"), "the actual mode: {text}");
        assert!(text.contains("chmod 600"), "the remedy: {text}");
    }

    #[test]
    #[cfg(unix)]
    fn every_group_or_other_read_bit_is_refused() {
        // Exhaustive over the nine low bits: any bit outside the owner's three makes
        // the file someone else's business.
        let scratch = Scratch::new("mode-sweep");
        let path = scratch.path().join("config.toml");
        fs::write(&path, "").expect("write");

        for mode in 0o600..=0o777 {
            set_mode(&path, mode);
            let result = read_private(ConfigSource::User, &path);
            let permissive = mode & 0o077 != 0;
            assert_eq!(
                result.is_err(),
                permissive,
                "mode {mode:04o} should {} be refused",
                if permissive { "" } else { "not" }
            );
        }

        // Leave it readable so the scratch directory can be removed.
        set_mode(&path, 0o600);
    }

    #[test]
    #[cfg(unix)]
    fn owner_only_execute_and_write_bits_do_not_matter() {
        let scratch = Scratch::new("owner-bits");
        let path = scratch.path().join("config.toml");
        fs::write(&path, "").expect("write");
        for mode in [0o400, 0o600, 0o700] {
            set_mode(&path, mode);
            assert!(
                read_private(ConfigSource::User, &path).is_ok(),
                "mode {mode:04o} is the owner's business"
            );
        }
        set_mode(&path, 0o600);
    }

    #[test]
    fn a_directory_is_not_a_config_file() {
        let scratch = Scratch::new("directory");
        let error =
            read_private(ConfigSource::User, scratch.path()).expect_err("a directory is not a regular file");
        assert!(error.to_string().contains("not a regular file"), "{error}");
    }

    #[test]
    #[cfg(unix)]
    fn a_fifo_is_refused_rather_than_hanging() {
        // The failure this check exists for. Opening a FIFO read-only blocks until a
        // writer appears, so a handle-based type check is too late: it would hang
        // startup before any UI exists to explain why. If this test ever hangs rather
        // than failing, the pre-flight `stat` has been removed.
        let scratch = Scratch::new("fifo");
        let path = scratch.path().join("config.toml");
        let made =
            std::process::Command::new("mkfifo").arg(&path).status().is_ok_and(|status| status.success());
        if !made {
            eprintln!("skipped: mkfifo unavailable");
            return;
        }
        set_mode(&path, 0o600);

        let error = read_private(ConfigSource::User, &path).expect_err("a FIFO is not a file");
        assert!(error.to_string().contains("not a regular file"), "{error}");

        // And the shared path must refuse it too, since the project layer reads
        // through the same door.
        let error = read_shared(ConfigSource::Project, &path).expect_err("a FIFO is not a file");
        assert!(error.to_string().contains("not a regular file"), "{error}");
    }

    #[test]
    fn the_project_walk_finds_the_nearest_file() {
        let scratch = Scratch::new("project-walk");
        let outer = scratch.path().join("outer");
        let inner = outer.join("middle").join("inner");
        fs::create_dir_all(&inner).expect("tree");

        fs::create_dir_all(outer.join(PROJECT_DIR)).expect("outer config dir");
        fs::write(outer.join(PROJECT_DIR).join(CONFIG_FILE), "").expect("outer config");

        // From the leaf, the only file above is the outer one.
        assert_eq!(project_config_path(&inner), Some(outer.join(PROJECT_DIR).join(CONFIG_FILE)));

        // Add a nearer one; it must win.
        let middle = outer.join("middle");
        fs::create_dir_all(middle.join(PROJECT_DIR)).expect("middle config dir");
        fs::write(middle.join(PROJECT_DIR).join(CONFIG_FILE), "").expect("middle config");
        assert_eq!(project_config_path(&inner), Some(middle.join(PROJECT_DIR).join(CONFIG_FILE)));
    }

    #[test]
    fn the_project_walk_gives_up_at_the_root() {
        let scratch = Scratch::new("project-none");
        let deep = scratch.path().join("a").join("b");
        fs::create_dir_all(&deep).expect("tree");
        // The walk reaches / without finding anything, unless the machine running the
        // test has a stray .supra at some ancestor - which would be a real finding
        // rather than a flaky test, so it is not defended against.
        assert_eq!(project_config_path(&deep), None);
    }

    #[test]
    fn a_directory_named_like_the_config_file_is_still_refused() {
        // `exists()` is true for a directory, so discovery can hand back a path that
        // is not a file. The reader has to cope, and it does.
        let scratch = Scratch::new("config-is-dir");
        let project = scratch.path().join(PROJECT_DIR);
        fs::create_dir_all(project.join(CONFIG_FILE)).expect("directory shaped like a file");

        let found = project_config_path(scratch.path()).expect("discovery finds it");
        let error = read_shared(ConfigSource::Project, &found).expect_err("but reading refuses");
        assert!(error.to_string().contains("not a regular file"), "{error}");
    }
}
