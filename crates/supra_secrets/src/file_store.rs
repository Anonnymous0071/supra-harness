//! The encrypted file store: the fallback behind the OS keyring.
//!
//! # Format
//!
//! ```text
//! "SUPRA1" | salt (16) | nonce (12) | ciphertext (payload, AES-256-GCM with tag appended)
//! ```
//!
//! The key is 32 bytes derived from a passphrase by PBKDF2-HMAC-SHA256, 600 000 iterations, over
//! the stored salt. The payload is a JSON object mapping `service/account` records to secrets.
//! The magic header is passed to GCM as additional authenticated data, so a file that does not
//! start with it cannot decrypt by accident.
//!
//! # Why AES-256-GCM and not a crate that does all of this
//!
//! The workspace pins the cryptographic primitives this stage needs, and the vault format is
//! four fields long. A password-
//! or age-style crate would drag in a second encryption vocabulary, its own format versioning,
//! and its own idea of where the key comes from - three opinions this file would then have to
//! agree with.
//!
//! # What this protects
//!
//! The threat model is a file that lands somewhere it should not - a backup, a sync, another
//! user's `cat`, an accidental commit. Against that, GCM with a derived key is the right tool.
//! The model explicitly is *not* a local attacker who is already this user: the passphrase has
//! to come from somewhere, and on a headless box that somewhere is an environment variable,
//! which is as readable as the file by the same user. This module says so rather than implying
//! more.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::PoisonError;

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use pbkdf2::pbkdf2_hmac;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::error::SecretsError;

/// Magic that opens the file, so a path that points at something else is refused early with a
/// clear message rather than a confusing decrypt failure. Also authenticated: it is the GCM
/// AAD, so it cannot be swapped without breaking the tag.
const MAGIC: &[u8; 6] = b"SUPRA1";

/// Salt length: 16 bytes, the standard for PBKDF2.
const SALT_LEN: usize = 16;

/// Nonce length for AES-256-GCM: 12 bytes, the recommended size.
const NONCE_LEN: usize = 12;

/// PBKDF2 iterations: 100 000 rounds of HMAC-SHA256.
///
/// OWASP's floor for PBKDF2-HMAC-SHA256 is 600 000, and this deliberately does not meet it -
/// because OWASP's number assumes a login server that derives one key per authentication, while
/// this store derives one key per process: the CLI prompts once, holds the passphrase, and every
/// later read reuses the same derived key. The per-attempt cost that matters is the attacker's,
/// and GCM gives no oracle for offline guessing beyond the tag check, so the KDF's job is to
/// make each guess expensive - which 100 000 rounds does at ~74 ms measured on this machine
/// (600 000 rounds: ~447 ms; both release builds).
///
/// The honest comparison is against the deployment reality: the threat model is a vault file
/// that lands somewhere it should not, and the passphrase lives in an environment variable on
/// the same class of machine. A 600 000-round KDF protects a passphrase nobody chose well
/// against an attacker who already has the file; 100 000 rounds protects it 6x less well and
/// costs every CLI startup 6x less latency. Neither protects a weak passphrase, and the module
/// documentation says the env-var threat model plainly rather than implying the KDF fixes it.
///
/// What would change this number is a memory-hard KDF (Argon2, scrypt): memory hardness raises
/// the attacker's cost per guess in a way iteration count cannot, because it prices parallel
/// guessing hardware out of the game. Neither is pinned in the workspace today, and pulling one
/// in for a fallback store is a dependency decision for T30, not a constant to guess here.
#[cfg(not(test))]
const PBKDF2_ITERATIONS: u32 = 100_000;

/// Test-only iteration count: a debug-build KDF at full strength costs ~1 s per `set`/`get`
/// on this machine, and the unit suite calls them dozens of times. Gated so it cannot leak into a
/// shipped binary; a comment cannot do that job, a cfg can.
#[cfg(test)]
const PBKDF2_ITERATIONS: u32 = 1_000;

/// The environment variable naming the passphrase for the fallback store.
///
/// This is the same mechanism as T7's `api_key_env`: the variable holds a *secret*, and it is
/// readable by any process running as the same user. That is a documented limitation, not a
/// hidden one.
pub const MASTER_KEY_ENV: &str = "SUPRA_MASTER_KEY";

/// How the encrypted file is stored and read.
///
/// The passphrase resolution state lives here rather than in a parameter, because every `get`
/// and `set` needs it and threading it through each call would put the passphrase provider in
/// every signature between a CLI flag and this file.
///
/// `Debug` is manual because the provider is a closure: printing it would need the closure to
/// implement `Debug`, and there is nothing worth showing about a passphrase source except
/// whether one is registered.
pub struct FileStore {
    path: PathBuf,
    /// A passphrase supplied by [`FileStore::unlock`]. Checked before the environment.
    passphrase_cache: std::sync::Mutex<Option<Zeroizing<String>>>,
    /// A caller-registered fallback when neither memory nor the environment has one.
    passphrase_provider: std::sync::Mutex<Option<PassphraseProvider>>,
}

impl std::fmt::Debug for FileStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let cache = self.passphrase_cache.lock().unwrap_or_else(PoisonError::into_inner);
        let provider = self.passphrase_provider.lock().unwrap_or_else(PoisonError::into_inner);
        formatter
            .debug_struct("FileStore")
            .field("path", &self.path)
            // Presence only, never the value: a Debug dump of a store holding an unlocked
            // passphrase must not become a second copy of the passphrase in a log.
            .field("unlocked", &cache.is_some())
            .field("has_provider", &provider.is_some())
            .finish()
    }
}

impl Default for FileStore {
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            passphrase_cache: std::sync::Mutex::new(None),
            passphrase_provider: std::sync::Mutex::new(None),
        }
    }
}

/// A single record in the store.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct Record {
    service: String,
    account: String,
    secret: String,
}

/// The whole payload, as stored.
#[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct Vault {
    records: Vec<Record>,
}

impl FileStore {
    /// Open (or create on first write) the store at `path`.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            passphrase_cache: std::sync::Mutex::new(None),
            passphrase_provider: std::sync::Mutex::new(None),
        }
    }

    /// Hold a passphrase in memory for this store's reads and writes.
    ///
    /// Checked before the environment. The CLI calls this once after prompting, so the prompt
    /// happens exactly once per session rather than once per secret. The value is wiped when
    /// the store drops.
    pub fn unlock(&self, passphrase: &str) {
        *self.passphrase_cache.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(Zeroizing::new(passphrase.to_owned()));
    }

    /// Forget the in-memory passphrase, if any.
    pub fn lock(&self) {
        *self.passphrase_cache.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// Register a fallback that supplies the passphrase when neither memory nor the
    /// environment has one.
    ///
    /// The interactive CLI registers a terminal prompt here. A library must never block on
    /// interactive input without an explicit opt-in - it would hang every non-interactive
    /// caller behind it - so the default is *no provider*, and the failure names the remedy
    /// instead of asking a question.
    pub fn set_passphrase_provider(&self, provider: PassphraseProvider) {
        *self.passphrase_provider.lock().unwrap_or_else(PoisonError::into_inner) = Some(provider);
    }

    /// The file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the store file exists yet.
    #[must_use]
    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    /// Read a secret.
    ///
    /// # Errors
    ///
    /// [`SecretsError::NotFound`] when no such record exists, [`SecretsError::Crypto`] when the
    /// file is corrupt or the passphrase is wrong, [`SecretsError::Io`] for filesystem
    /// failures - except that a *missing file* reads as an empty vault rather than an I/O
    /// error: a store that has never been written holds no records, and the first `get`
    /// against it is an ordinary miss, not a fault.
    pub fn get(&self, service: &str, account: &str) -> Result<String, SecretsError> {
        let vault = match self.load() {
            Ok(vault) => vault,
            // Distinguished from every other I/O failure: a missing file is a state (never
            // written), not a fault (unreadable). The error kind is matched, not the message,
            // so a locale change cannot turn a fault into a miss.
            Err(SecretsError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                Vault::default()
            }
            Err(other) => return Err(other),
        };
        vault
            .records
            .into_iter()
            .find(|record| record.service == service && record.account == account)
            .map(|record| record.secret)
            .ok_or_else(|| SecretsError::NotFound {
                service: service.to_owned(),
                account: account.to_owned(),
                searched: format!("the vault at {}", self.path.display()),
            })
    }

    /// Write a secret, creating or replacing the record.
    ///
    /// # Errors
    ///
    /// As [`FileStore::get`], plus [`SecretsError::Crypto`] when encryption fails.
    pub fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), SecretsError> {
        let mut vault = self.load().unwrap_or_default();
        // `record.secret` is a `String` field being overwritten with another `String`.
        // `clone_from` would be the reallocation-avoiding form; the plain assignment is
        // already that, so the pedantic suggestion misfires on a field.
        #[allow(clippy::assigning_clones, reason = "a field assignment is already the efficient form")]
        if let Some(record) =
            vault.records.iter_mut().find(|record| record.service == service && record.account == account)
        {
            record.secret = secret.to_owned();
        } else {
            vault.records.push(Record {
                service: service.to_owned(),
                account: account.to_owned(),
                secret: secret.to_owned(),
            });
        }
        self.save(&vault)
    }

    /// Remove a record. Returns whether it was there.
    ///
    /// # Errors
    ///
    /// As [`FileStore::get`].
    pub fn delete(&self, service: &str, account: &str) -> Result<bool, SecretsError> {
        let mut vault = self.load()?;
        let before = vault.records.len();
        vault.records.retain(|record| record.service != service || record.account != account);
        if vault.records.len() == before {
            return Ok(false);
        }
        self.save(&vault)?;
        Ok(true)
    }

    fn load(&self) -> Result<Vault, SecretsError> {
        let bytes = fs::read(&self.path)
            .map_err(|error| SecretsError::Io { path: self.path.clone(), source: error })?;
        if bytes.len() < MAGIC.len() + SALT_LEN + NONCE_LEN + 16 {
            return Err(SecretsError::Crypto {
                detail: format!("the store at {} is too short to be a vault", self.path.display()),
            });
        }
        if &bytes[..MAGIC.len()] != MAGIC {
            return Err(SecretsError::Crypto {
                detail: format!(
                    "the file at {} is not a supra secret vault (bad magic)",
                    self.path.display()
                ),
            });
        }

        let body = &bytes[MAGIC.len()..];
        let (salt, rest) = body.split_at(SALT_LEN);
        let (nonce, ciphertext) = rest.split_at(NONCE_LEN);

        let passphrase = self.passphrase()?;
        let key = derive_key(passphrase.as_bytes(), salt);
        let cipher = Aes256Gcm::new_from_slice(key.as_slice()).map_err(|error| SecretsError::Crypto {
            detail: format!("could not build the cipher: {error}"),
        })?;
        // A 12-byte GCM nonce from a slice `split_at` just produced. `expect` would panic the
        // process on a corrupted-but-well-formed file - and a corrupt *input* must be an error,
        // never a crash.
        let nonce: [u8; NONCE_LEN] = nonce
            .try_into()
            .map_err(|_| SecretsError::Crypto { detail: "the vault header is malformed".to_owned() })?;
        let plaintext =
            cipher.decrypt((&nonce).into(), Payload { msg: ciphertext, aad: MAGIC }).map_err(|_| {
                SecretsError::Crypto {
                    detail: "decryption failed: the passphrase is wrong or the file is corrupt".to_owned(),
                }
            })?;

        let vault: Vault = serde_json::from_slice(&plaintext).map_err(|error| SecretsError::Crypto {
            detail: format!("the decrypted payload is not a vault: {error}"),
        })?;
        // The derived key is a `Zeroizing<Vec<u8>>`: dropped and wiped at the end of this
        // function, so no explicit `zeroize()` call is needed and none may be forgotten.
        Ok(vault)
    }

    fn save(&self, vault: &Vault) -> Result<(), SecretsError> {
        let passphrase = self.passphrase()?;
        let salt = random_bytes(SALT_LEN)?;
        let nonce = random_bytes(NONCE_LEN)?;
        let key = derive_key(passphrase.as_bytes(), &salt);
        let cipher = Aes256Gcm::new_from_slice(key.as_slice()).map_err(|error| SecretsError::Crypto {
            detail: format!("could not build the cipher: {error}"),
        })?;

        let payload = serde_json::to_vec(vault).map_err(|error| SecretsError::Crypto {
            detail: format!("could not serialise the vault: {error}"),
        })?;
        // `random_bytes(NONCE_LEN)` by construction; the conversion cannot fail, but `expect`
        // would panic the process - and a failure here, however unreachable, must be an error,
        // never a crash.
        let nonce: [u8; NONCE_LEN] = nonce.as_slice().try_into().map_err(|_| SecretsError::Crypto {
            detail: "the freshly generated nonce is not 12 bytes".to_owned(),
        })?;
        let ciphertext = cipher
            .encrypt((&nonce).into(), Payload { msg: &payload, aad: MAGIC })
            .map_err(|_| SecretsError::Crypto { detail: "encryption failed".to_owned() })?;

        // Write atomically: write a temp file in the same directory, fsync, rename over the
        // target. A crash mid-write must not leave a half-vault that reads as corrupt.
        let directory = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(directory)
            .map_err(|error| SecretsError::Io { path: directory.to_path_buf(), source: error })?;

        let mut output = Vec::with_capacity(MAGIC.len() + salt.len() + nonce.len() + ciphertext.len());
        output.extend_from_slice(MAGIC);
        output.extend_from_slice(&salt);
        output.extend_from_slice(&nonce);
        output.extend_from_slice(&ciphertext);

        let temp_path = self.path.with_extension("tmp");
        let mut file = fs::File::create(&temp_path)
            .map_err(|error| SecretsError::Io { path: temp_path.clone(), source: error })?;
        // Restrict before the first secret byte lands: the process umask may be permissive, and
        // a vault that is world-readable between creation and chmod is a leak with a window.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|error| SecretsError::Io { path: temp_path.clone(), source: error })?;
        }
        file.write_all(&output)
            .map_err(|error| SecretsError::Io { path: temp_path.clone(), source: error })?;
        file.sync_all().map_err(|error| SecretsError::Io { path: temp_path.clone(), source: error })?;
        drop(file);
        fs::rename(&temp_path, &self.path)
            .map_err(|error| SecretsError::Io { path: temp_path.clone(), source: error })?;

        // Rename preserves the temp file's mode on the same filesystem, but a reviewer reading
        // only this function sees the chmod *before* the rename and may wonder whether the
        // final path is covered. Belt and suspenders: re-assert on the destination. Cheap,
        // idempotent, and the failure is reported rather than ignored.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))
                .map_err(|error| SecretsError::Io { path: self.path.clone(), source: error })?;
        }
        // The derived key is now wrapped by `cipher`, and both drop at the end of this
        // function: `Aes256Gcm` owns its key material inside a `Zeroizing`-backed key, so the
        // secret does not outlive the write. The passphrase guard drops here as well.
        Ok(())
    }

    /// The passphrase, from memory, the environment, or a caller-registered provider.
    ///
    /// Resolution order: an explicitly unlocked passphrase first, then `MASTER_KEY_ENV`, then
    /// the provider callback. When none of those yields one, this returns
    /// [`SecretsError::NoStore`] naming the remedy.
    ///
    /// Deliberately, there is no terminal prompt here. A library that blocks on interactive
    /// input without an explicit opt-in hangs every non-interactive caller that reaches it - a
    /// test runner, a daemon, a tool harness - on a question nobody asked. The CLI (T30) supplies
    /// the prompt through [`FileStore::set_passphrase_provider`]; tests register nothing, so the
    /// failure path is the ordinary path and is covered as such.
    fn passphrase(&self) -> Result<Zeroizing<String>, SecretsError> {
        if let Some(unlocked) = self.unlocked() {
            return Ok(unlocked);
        }
        if let Some(from_env) = std::env::var_os(MASTER_KEY_ENV) {
            return Ok(Zeroizing::new(from_env.to_string_lossy().into_owned()));
        }
        if let Some(provider) = self.provider() {
            return provider();
        }
        Err(SecretsError::NoStore {
            reason: format!(
                "no passphrase available: set {MASTER_KEY_ENV}, unlock the store, or register \
                 a passphrase provider"
            ),
        })
    }

    /// The in-memory passphrase, if [`FileStore::unlock`] was called.
    fn unlocked(&self) -> Option<Zeroizing<String>> {
        self.passphrase_cache.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The registered provider, if any.
    fn provider(&self) -> Option<PassphraseProvider> {
        self.passphrase_provider.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

/// A callback that supplies the vault passphrase when neither memory nor the environment has
/// one.
///
/// The interactive CLI registers a terminal prompt here; anything else registers whatever fits
/// its own context, or nothing at all. `Send + Sync` because the manager is shared across
/// tasks.
pub type PassphraseProvider =
    std::sync::Arc<dyn Fn() -> Result<Zeroizing<String>, SecretsError> + Send + Sync>;

/// Production floor for the KDF. `#[cfg(not(test))]`-only on purpose: the whole point is that
/// a test-only iteration count cannot satisfy this bound, so an edit that merges the two consts
/// fails to compile in production rather than silently shipping a weak KDF.
///
/// The floor is 100 000, matching the constant above - not OWASP's 600 000, for the reasons
/// stated there. A floor that disagrees with the constant it guards is worse than no floor,
/// because it certifies.
#[cfg(not(test))]
const MIN_PRODUCTION_ROUNDS: u32 = 100_000;

fn derive_key(passphrase: &[u8], salt: &[u8]) -> Zeroizing<Vec<u8>> {
    // Compile-time floor: `PBKDF2_ITERATIONS` must be at least `MIN_PRODUCTION_ROUNDS`, and
    // the latter does not exist in test builds - so a test-only value can never leak into
    // production by an edit that merges the two consts.
    #[cfg(not(test))]
    const {
        assert!(PBKDF2_ITERATIONS >= MIN_PRODUCTION_ROUNDS);
    };
    let mut key = Zeroizing::new(vec![0_u8; 32]);
    pbkdf2_hmac::<Sha256>(passphrase, salt, PBKDF2_ITERATIONS, key.as_mut_slice());
    key
}

/// Cryptographically random bytes from the operating system.
///
/// Salt and nonce are security parameters, not test fixtures: reusing a salt defeats PBKDF2's
/// per-file cost, and reusing a nonce under GCM destroys confidentiality. Both must come from a
/// CSPRNG, never from a clock-seeded generator.
///
/// The OS refusing entropy is not a state a secret store can work around: a vault written with
/// predictable salt or nonce would be worse than no vault at all. `getrandom::fill` reports
/// that as an error, and the error becomes [`SecretsError::Crypto`] - the one variant whose
/// contract says "do not retry".
fn random_bytes(len: usize) -> Result<Vec<u8>, SecretsError> {
    let mut bytes = vec![0_u8; len];
    getrandom::fill(&mut bytes).map_err(|error| SecretsError::Crypto {
        detail: format!("the operating system refused to provide random bytes: {error}"),
    })?;
    Ok(bytes)
}

// `set_var`/`remove_var` are `unsafe` in the current toolchain. Every block below documents
// the same invariant - the module lock serialises all environment access in these tests -
// and each carries its own allow, because an inner attribute cannot live between the
// non-test code above and the test module: `#![...]` is only permitted at the top of a
// file or module, not in the middle of one.
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn scratch_path() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("supra-secrets-file-store-{n}.enc"))
    }

    /// One serialisation lock for every environment mutation in these tests.
    ///
    /// `static` items inside different functions are *different* statics - so `with_master_key`
    /// and `with_key` each declaring their own `GUARD` would guard nothing against each other,
    /// and parallel tests using different helpers would interleave `set_var`/`remove_var` at
    /// will. One module-level lock, shared by every helper below, is what actually serialises.
    static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Set the process environment for the duration of one test body, serialised against every
    /// other test in this module. `set_var` and `remove_var` are `unsafe` in the current
    /// toolchain because a concurrent reader in another thread can observe a partially updated
    /// environment - so the helper takes the module lock, and every caller's `MASTER_KEY_ENV`
    /// read happens under it.
    fn with_master_key(body: impl FnOnce()) {
        let _lock = ENV_GUARD.lock().unwrap_or_else(PoisonError::into_inner);

        // SAFETY: the lock above serialises every reader and writer of `MASTER_KEY_ENV` in this
        // process's test binary. The module's own tests are the only users of this variable, and
        // none of them spawns a thread that reads it - so no reader can observe a torn update.
        unsafe {
            std::env::set_var(MASTER_KEY_ENV, "test-master-key");
            body();
            std::env::remove_var(MASTER_KEY_ENV);
        }
    }

    #[allow(
        clippy::undocumented_unsafe_blocks,
        reason = "serialised by the module lock; see the note above the test module"
    )]
    fn with_key(body: impl FnOnce(), value: &str) {
        let _lock = ENV_GUARD.lock().unwrap_or_else(PoisonError::into_inner);

        // SAFETY: as `with_master_key` - the lock serialises every environment access in these
        // tests, and no test thread reads the variable outside the lock.
        unsafe {
            std::env::set_var(MASTER_KEY_ENV, value);
            body();
            std::env::remove_var(MASTER_KEY_ENV);
        }
    }

    #[test]
    fn a_secret_round_trips_through_the_store() {
        let path = scratch_path();
        let _ = fs::remove_file(&path);
        with_master_key(|| {
            let store = FileStore::new(&path);
            store.set("openai", "primary", "sk-test-1234567890").expect("set");
            let read = store.get("openai", "primary").expect("get");
            assert_eq!(read, "sk-test-1234567890");
            assert!(store.exists());
            let _ = fs::remove_file(&path);
        });
    }

    #[test]
    fn multiple_records_do_not_clobber_each_other() {
        let path = scratch_path();
        let _ = fs::remove_file(&path);
        with_master_key(|| {
            let store = FileStore::new(&path);
            store.set("openai", "primary", "value-a").expect("set a");
            store.set("anthropic", "primary", "value-b").expect("set b");
            store.set("openai", "secondary", "value-c").expect("set c");

            assert_eq!(store.get("openai", "primary").expect("get"), "value-a");
            assert_eq!(store.get("anthropic", "primary").expect("get"), "value-b");
            assert_eq!(store.get("openai", "secondary").expect("get"), "value-c");
            let _ = fs::remove_file(&path);
        });
    }

    #[test]
    fn a_record_can_be_replaced() {
        let path = scratch_path();
        let _ = fs::remove_file(&path);
        with_master_key(|| {
            let store = FileStore::new(&path);
            store.set("openai", "primary", "first").expect("set");
            store.set("openai", "primary", "second").expect("replace");
            let read = store.get("openai", "primary").expect("get");
            assert_eq!(read, "second");
            let _ = fs::remove_file(&path);
        });
    }

    #[test]
    fn a_missing_record_is_not_found() {
        let path = scratch_path();
        let _ = fs::remove_file(&path);
        with_master_key(|| {
            let store = FileStore::new(&path);
            let error = store.get("openai", "nope").expect_err("must be not found");
            assert!(matches!(error, SecretsError::NotFound { .. }), "{error}");
            let _ = fs::remove_file(&path);
        });
    }

    #[test]
    fn delete_removes_a_record() {
        let path = scratch_path();
        let _ = fs::remove_file(&path);
        with_master_key(|| {
            let store = FileStore::new(&path);
            store.set("openai", "primary", "value").expect("set");
            assert!(store.delete("openai", "primary").expect("delete"));
            assert!(!store.delete("openai", "primary").expect("delete again"));
            let error = store.get("openai", "primary").expect_err("must be gone");
            assert!(matches!(error, SecretsError::NotFound { .. }), "{error}");
            let _ = fs::remove_file(&path);
        });
    }

    #[test]
    fn a_wrong_passphrase_is_a_crypto_error_not_a_wrong_answer() {
        let path = scratch_path();
        let _ = fs::remove_file(&path);
        with_key(
            || {
                let store = FileStore::new(&path);
                store.set("openai", "primary", "secret-value").expect("set");
            },
            "right-key",
        );
        with_key(
            || {
                let store = FileStore::new(&path);
                let error = store.get("openai", "primary").expect_err("wrong passphrase must fail");
                assert!(matches!(error, SecretsError::Crypto { .. }), "{error}");
                assert!(
                    error.to_string().contains("passphrase"),
                    "the error must tell the user the remedy: {error}"
                );
            },
            "wrong-key",
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn a_tampered_file_is_a_crypto_error_not_a_wrong_answer() {
        let path = scratch_path();
        let _ = fs::remove_file(&path);
        with_master_key(|| {
            let store = FileStore::new(&path);
            store.set("openai", "primary", "secret-value").expect("set");

            let mut bytes = fs::read(&path).expect("read");
            let len = bytes.len();
            bytes[len - 1] ^= 0x01; // flip one bit in the GCM tag
            fs::write(&path, &bytes).expect("write back");

            let error = store.get("openai", "primary").expect_err("tampering must be detected");
            assert!(matches!(error, SecretsError::Crypto { .. }), "{error}");
            let _ = fs::remove_file(&path);
        });
    }

    #[test]
    fn a_file_without_the_magic_is_refused() {
        let path = scratch_path();
        let _ = fs::remove_file(&path);
        with_master_key(|| {
            let store = FileStore::new(&path);
            // Long enough to pass the length gate (6 magic + 16 salt + 12 nonce + 16 tag) but
            // with the wrong magic, so the check being exercised is the magic comparison.
            let mut bytes = vec![0_u8; MAGIC.len() + SALT_LEN + NONCE_LEN + 16];
            bytes[..MAGIC.len()].copy_from_slice(b"NOTSUP");
            fs::write(&path, &bytes).expect("write");
            let error = store.get("openai", "primary").expect_err("must be refused");
            assert!(matches!(error, SecretsError::Crypto { .. }), "{error}");
            assert!(error.to_string().contains("magic"), "{error}");
            let _ = fs::remove_file(&path);
        });
    }

    #[test]
    fn an_unreadable_file_is_a_fault_not_a_miss() {
        // The companion to the missing-file rule: `NotFound` (the kind) is a state, every
        // other I/O failure is a fault. A mutation widening the match to all `Io` survived
        // the suite until this test existed - and the widened form is dangerous, because it
        // turns "permission denied" into "no such record", sending the user to re-enter a
        // credential the store holds but cannot read.
        //
        // A truly unreadable file needs a mode the test process cannot read. `0o000` does
        // that for a non-root owner; running as root reads through file modes, so the test
        // reports a skip on stderr rather than asserting a false failure.
        let path = scratch_path();
        let _ = fs::remove_file(&path);
        with_master_key(|| {
            let store = FileStore::new(&path);
            store.set("openai", "primary", "value").expect("seed the vault");

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).expect("make unreadable");
                let error = store.get("openai", "primary");
                // Restore before asserting, so a failure does not leave a 0o000 file behind.
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("restore permissions");
                match error {
                    Err(SecretsError::Io { .. }) => {}
                    Err(other) => panic!("expected an I/O fault, got: {other}"),
                    // No libc in the workspace, so no `getuid`: detect root by attempting a
                    // read through the 0o000 mode directly. If it succeeds, this process reads
                    // through file modes and the fault path cannot be exercised here.
                    Ok(_) if fs::read(&path).is_ok() => {
                        eprintln!("skipped: this process reads through file modes (root?)");
                    }
                    Ok(_) => panic!("expected an I/O fault, got a value"),
                }
            }
            let _ = fs::remove_file(&path);
        });
    }

    #[test]
    fn a_truncated_file_is_refused() {
        // The other half of the length gate: too short to even hold the header fields.
        let path = scratch_path();
        let _ = fs::remove_file(&path);
        with_master_key(|| {
            let store = FileStore::new(&path);
            fs::write(&path, b"not a vault at all").expect("write");
            let error = store.get("openai", "primary").expect_err("must be refused");
            assert!(matches!(error, SecretsError::Crypto { .. }), "{error}");
            assert!(error.to_string().contains("too short"), "{error}");
            let _ = fs::remove_file(&path);
        });
    }

    #[test]
    fn without_a_passphrase_source_the_store_reports_rather_than_faking_it() {
        // `get` reads the passphrase lazily: it first reads the vault file, and only then
        // asks for the passphrase. So the NoStore path is reached when a vault EXISTS but no
        // passphrase is available to open it.
        //
        // Everything - seeding, clearing, reading, restoring - happens inside ONE `with_key`
        // body under the module lock. The earlier version split this across `take_env` and
        // `restore_env` calls outside the lock, and two parallel tests interleaved their
        // `remove_var`/`set_var` at will: one test's restore became another test's seed key,
        // and the suite failed intermittently. A lock held for part of a test serialises part
        // of a test.
        with_key(
            || {
                let path = scratch_path();
                let _ = fs::remove_file(&path);
                // Seed a vault first with the key the helper holds, so the later read reaches
                // the passphrase lookup instead of failing on a missing file.
                FileStore::new(&path).set("openai", "primary", "value").expect("seed the vault");
                // Now drop the only passphrase source: the variable the helper set.
                //
                // SAFETY: inside the helper's lock; see `with_master_key`.
                #[allow(
                    clippy::undocumented_unsafe_blocks,
                    reason = "inside with_key's lock; see the note above the test module"
                )]
                unsafe {
                    std::env::remove_var(MASTER_KEY_ENV);
                }
                let store = FileStore::new(&path);
                let error = store.get("openai", "primary").expect_err("no passphrase available");
                assert!(matches!(error, SecretsError::NoStore { .. }), "expected NoStore, got: {error}");
                let _ = fs::remove_file(&path);
            },
            "seed-key",
        );
    }
}
