//! The secret manager: OS keyring first, encrypted file second.
//!
//! # The ladder
//!
//! ```text
//! OS keyring (desktop)  ->  encrypted file (headless)  ->  a clear error
//! ```
//!
//! The ladder is decided at [`SecretManager::open`] by probing the OS keyring, never assumed.
//! `keyring` v3 creates entries lazily, so the probe reads a dedicated sentinel: a value or
//! [`keyring::Error::NoEntry`] proves the platform store answered, while every backend error
//! selects the encrypted-file fallback. The probe never creates, changes, or deletes a secret.
//! If the probe fails, the encrypted file becomes the primary store. If that cannot work either
//! (no passphrase source and no terminal), the error says exactly what to set rather than
//! pretending a store exists.
//!
//! # Why "primary" and "fallback", not a merged namespace
//!
//! A credential lives in exactly one store, not in both. `get` searches the stores in order -
//! so a secret written while the keyring was down is still found when it comes back - but `set`
//! writes to the primary only. Merging namespaces would make "where does my key live?" depend
//! on the history of which store was down when, which is not a question a user should have to
//! answer.

use std::path::PathBuf;

use crate::error::{SecretsError, is_no_entry};
use crate::file_store::FileStore;

/// The default directory for the encrypted fallback store.
///
/// `$XDG_CONFIG_HOME` or `~/.config` on Unix, `%APPDATA%` on Windows, `~/Library/Application
/// Support` on macOS. The file itself is `supra/secrets.enc`.
fn default_config_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(app_data) = std::env::var_os("APPDATA") {
            return PathBuf::from(app_data);
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join("Library").join("Application Support");
        }
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".config");
    }
    PathBuf::from(".")
}

/// The default path of the encrypted fallback store.
#[must_use]
pub fn default_file_store_path() -> PathBuf {
    default_config_dir().join("supra").join("secrets.enc")
}

/// Which store a credential lives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// The OS keyring: macOS Keychain, Windows Credential Manager, or a Linux Secret Service.
    OsKeyring,
    /// The encrypted file fallback.
    EncryptedFile,
}

/// How secrets are read and written.
#[derive(Debug)]
pub struct SecretManager {
    /// Whether the OS keyring answered the probe.
    keyring_available: bool,
    /// The encrypted file store. Always constructed; only used when the keyring is not
    /// available, or as the second place `get` looks.
    file: FileStore,
    /// How the stores were chosen, for diagnostics.
    keyring_probe_error: Option<String>,
}

impl SecretManager {
    /// Open the manager: probe the OS keyring, fall back to the encrypted file.
    ///
    /// The keyring probe is the only thing that runs here. The file store's passphrase is not
    /// read until a secret actually is, so an interactive prompt cannot fire during startup on
    /// a machine that will never need the fallback.
    ///
    /// # Errors
    ///
    /// Never, in the current design: if neither store can work, the failure surfaces on the
    /// first `get` or `set` with a message naming the remedy. A manager that exists but cannot
    /// store anything is a better outcome than a startup that refuses to run because the
    /// environment is headless - the caller may only want to *read* a credential that the OS
    /// keyring does hold.
    #[must_use]
    pub fn open() -> Self {
        Self::open_with_store(FileStore::new(default_file_store_path()))
    }

    /// Open with a specific file store path. For tests, and for an explicit `--secrets-file`.
    #[must_use]
    pub fn open_with_file_store(path: PathBuf) -> Self {
        Self::open_with_store(FileStore::new(path))
    }

    /// Supply the fallback vault's passphrase for this session.
    ///
    /// Checked before the environment on every read and write, so the interactive CLI prompts
    /// once and every later secret resolves without asking again.
    pub fn unlock_file_store(&self, passphrase: &str) {
        self.file.unlock(passphrase);
    }

    /// Forget the fallback passphrase. The environment variable, if set, still applies.
    pub fn lock_file_store(&self) {
        self.file.lock();
    }

    /// Register a fallback that supplies the vault passphrase.
    ///
    /// The interactive CLI registers its terminal prompt here. Nothing else does, so the
    /// library never blocks on input it was never told how to ask for.
    pub fn set_passphrase_provider(&self, provider: crate::file_store::PassphraseProvider) {
        self.file.set_passphrase_provider(provider);
    }

    fn open_with_store(file: FileStore) -> Self {
        // Entry construction validates only the service/account shape in keyring v3; the read is
        // what connects to the native store. NoEntry is therefore a successful availability
        // probe. A dedicated namespace keeps an accidental pre-existing value harmless, and the
        // probe never writes or deletes it.
        let probe = keyring::Entry::new("supra-harness-keyring-probe-v1", "availability")
            .and_then(|entry| entry.get_password());
        let (keyring_available, keyring_probe_error) = match probe {
            Ok(_) | Err(keyring::Error::NoEntry) => (true, None),
            Err(error) => (false, Some(error.to_string())),
        };

        Self { keyring_available, file, keyring_probe_error }
    }

    /// Which backend is primary: the OS keyring when it answered the probe, else the file.
    #[must_use]
    pub fn primary_backend(&self) -> Backend {
        if self.keyring_available { Backend::OsKeyring } else { Backend::EncryptedFile }
    }

    /// Which store a credential lives in, for diagnostics.
    #[must_use]
    pub const fn keyring_available(&self) -> bool {
        self.keyring_available
    }

    /// Why the keyring probe failed, when it did.
    #[must_use]
    pub const fn keyring_probe_error(&self) -> Option<&String> {
        self.keyring_probe_error.as_ref()
    }

    /// Read a secret, searching the available stores in order.
    ///
    /// # Errors
    ///
    /// [`SecretsError::NotFound`] when no store holds the entry,
    /// [`SecretsError::Crypto`] when the file store cannot decrypt,
    /// [`SecretsError::NoStore`] when no store is usable at all.
    pub fn get(&self, service: &str, account: &str) -> Result<String, SecretsError> {
        if self.keyring_available {
            match self.keyring_get(service, account) {
                Ok(value) => return Ok(value),
                // The keyring has no such entry. The file store is the second place a secret
                // may live (written while this machine was headless), so look there before
                // reporting not-found.
                //
                // Coverage note: no test on a keyring-less machine can take the `Ok` branch
                // above or prove this arm runs - the keyring never answers here. A mutation
                // turning this arm into an early return survives the suite for that reason.
                // The arm stays because the alternative (a headless-written secret vanishing
                // when the keyring comes back) is silent data loss, not because a test pins
                // it. Do not "simplify" it away.
                Err(error) if is_no_entry(&error) => {}
                Err(error) => {
                    return Err(SecretsError::Keyring(error.to_string()));
                }
            }
        }
        // The file store is the primary on a headless machine and the second place otherwise.
        // A NotFound from it becomes a message that says where the secret was looked for, and
        // when nothing can store at all, what to do about it.
        match self.file.get(service, account) {
            Ok(value) => Ok(value),
            Err(SecretsError::NotFound { .. }) => self.not_found(service, account),
            Err(other) => Err(other),
        }
    }

    /// The "nowhere has it" error, with the layout of the search in the message.
    ///
    /// # Errors
    ///
    /// Always. [`SecretsError::NotFound`] when a store exists but lacks the entry,
    /// [`SecretsError::NoStore`] when no store is configured at all.
    fn not_found(&self, service: &str, account: &str) -> Result<String, SecretsError> {
        let searched = if self.keyring_available {
            format!("searched the OS keyring and the fallback file at {}", self.file.path().display())
        } else {
            match &self.keyring_probe_error {
                Some(probe) => format!(
                    "searched the fallback file at {} (no OS keyring: {probe})",
                    self.file.path().display()
                ),
                None => format!("searched the fallback file at {}", self.file.path().display()),
            }
        };

        if !self.keyring_available && !self.file.exists() {
            // Neither store can hold the entry: the keyring is down and the file has never been
            // written. The remedy is not "look again" but "set up a store".
            return Err(SecretsError::NoStore {
                reason: format!(
                    "{searched} and nothing is configured to hold credentials yet - set {} or \
                     run in a terminal and one will be created on first write",
                    crate::file_store::MASTER_KEY_ENV
                ),
            });
        }
        // A store exists but lacks the entry; say where the search went.
        Err(SecretsError::NotFound { service: service.to_owned(), account: account.to_owned(), searched })
    }

    /// Write a secret to the primary store.
    ///
    /// # Errors
    ///
    /// As the store that is primary.
    pub fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), SecretsError> {
        if self.keyring_available {
            self.keyring_set(service, account, secret)
        } else {
            self.file.set(service, account, secret)
        }
    }

    /// Remove a secret from wherever it lives. Returns whether it was there.
    ///
    /// # Errors
    ///
    /// Any error from either store. Deletion checks both stores when the keyring is available so
    /// a fallback copy cannot silently survive a reported success.
    pub fn delete(&self, service: &str, account: &str) -> Result<bool, SecretsError> {
        if self.keyring_available {
            let removed = self.keyring_delete(service, account)?;
            // Also remove from the file, in case a previous headless session wrote it there.
            // A corrupt or inaccessible fallback must remain visible: reporting success while a
            // credential copy survives would violate the deletion contract.
            combine_delete_results(removed, self.file.delete(service, account))
        } else {
            self.file.delete(service, account)
        }
    }

    /// The file store path, for a diagnostic.
    #[must_use]
    pub fn file_store_path(&self) -> &std::path::Path {
        self.file.path()
    }

    /// The `self` here is the manager's choice of backend, not dead weight: the keyring
    /// path is taken only when the probe answered, and these helpers exist so `get` reads
    /// like the ladder it is. The lint is right that they do not touch fields; the structure
    /// is worth more than the warning.
    #[allow(clippy::unused_self, reason = "the helpers encode the backend ladder, not field access")]
    fn keyring_get(&self, service: &str, account: &str) -> Result<String, keyring::Error> {
        let entry = keyring::Entry::new(service, account)?;
        entry.get_password()
    }

    #[allow(clippy::unused_self, reason = "see keyring_get")]
    fn keyring_set(&self, service: &str, account: &str, secret: &str) -> Result<(), SecretsError> {
        let entry = keyring::Entry::new(service, account)
            .map_err(|error| SecretsError::Keyring(format!("could not open the keyring entry: {error}")))?;
        entry.set_password(secret).map_err(|error| SecretsError::Keyring(error.to_string()))
    }

    #[allow(clippy::unused_self, reason = "see keyring_get")]
    fn keyring_delete(&self, service: &str, account: &str) -> Result<bool, SecretsError> {
        let entry = keyring::Entry::new(service, account)
            .map_err(|error| SecretsError::Keyring(format!("could not open the keyring entry: {error}")))?;
        match entry.delete_credential() {
            Ok(()) => Ok(true),
            Err(error) if is_no_entry(&error) => Ok(false),
            Err(error) => Err(SecretsError::Keyring(error.to_string())),
        }
    }
}

fn combine_delete_results(
    primary_removed: bool,
    fallback_removed: Result<bool, SecretsError>,
) -> Result<bool, SecretsError> {
    fallback_removed.map(|removed| primary_removed || removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_delete_errors_are_not_suppressed_after_primary_success() {
        let fallback_error = SecretsError::Crypto { detail: "fallback vault could not be opened".to_owned() };
        let error =
            combine_delete_results(true, Err(fallback_error)).expect_err("fallback error must propagate");
        assert!(matches!(error, SecretsError::Crypto { .. }));
    }

    #[test]
    fn deletion_reports_a_record_found_in_either_store() {
        assert!(combine_delete_results(true, Ok(false)).expect("primary result"));
        assert!(combine_delete_results(false, Ok(true)).expect("fallback result"));
        assert!(!combine_delete_results(false, Ok(false)).expect("miss result"));
    }
}
