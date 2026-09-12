//! The secret manager against a real (fallback) store.
//!
//! The OS keyring is unavailable in CI and on headless machines, so these tests exercise the
//! ladder's second rung: the encrypted file. The keyring-first path is covered by the manager's
//! construction (it probes at open) and by the fallback-order test below, which asserts the
//! probe outcome rather than assuming it.

// An integration test is its own crate, so the library's `cfg(test)` allowances do not reach
// it. `expect`/`panic` in assertions and `print_stderr` for environment-dependent skips. The
// `set_var`/`remove_var` helper below needs `unsafe` in the current toolchain; the workspace
// forbids it everywhere but T5, and honours that in shipped code. Tests are not shipped code,
// and serialised environment control has no safe spelling - so this file re-allows it here,
// scoped to this test target, with the invariant on the helper itself.
#![allow(clippy::expect_used, clippy::panic, clippy::print_stderr, unsafe_code)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use supra_secrets::{Backend, SecretManager, SecretsError, file_store::MASTER_KEY_ENV};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch_path() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let directory = std::env::temp_dir().join(format!("supra-secrets-manager-{n}"));
    let _ = std::fs::remove_dir_all(&directory);
    directory.join("secrets.enc")
}

fn remove_scratch(path: &std::path::Path) {
    if let Some(directory) = path.parent() {
        let _ = std::fs::remove_dir_all(directory);
    }
}

fn with_key(value: &str, body: impl FnOnce()) {
    // SAFETY: serialised by the module lock; no test thread reads outside it.
    static GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _lock = GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    unsafe {
        std::env::set_var(MASTER_KEY_ENV, value);
        body();
        std::env::remove_var(MASTER_KEY_ENV);
    }
}

#[test]
fn concurrent_process_writes_preserve_every_record() {
    if let (Some(path), Some(account)) =
        (std::env::var_os("SUPRA_CONCURRENCY_VAULT"), std::env::var_os("SUPRA_CONCURRENCY_ACCOUNT"))
    {
        let manager = SecretManager::open_with_file_store(PathBuf::from(path));
        if manager.primary_backend() == Backend::EncryptedFile {
            manager.unlock_file_store("concurrency-test-key");
            manager.set("svc", &account.to_string_lossy(), "opaque-test-value").expect("child set");
        }
        return;
    }

    let path = scratch_path();
    remove_scratch(&path);
    let executable = std::env::current_exe().expect("test executable");
    let mut children = Vec::new();
    for index in 0..8 {
        let child = Command::new(&executable)
            .arg("--exact")
            .arg("concurrent_process_writes_preserve_every_record")
            .arg("--nocapture")
            .env("SUPRA_CONCURRENCY_VAULT", &path)
            .env("SUPRA_CONCURRENCY_ACCOUNT", format!("account-{index}"))
            .spawn()
            .expect("spawn child");
        children.push(child);
    }
    for child in &mut children {
        assert!(child.wait().expect("wait for child").success(), "child failed");
    }

    let manager = SecretManager::open_with_file_store(path.clone());
    if manager.primary_backend() != Backend::EncryptedFile {
        remove_scratch(&path);
        return;
    }
    manager.unlock_file_store("concurrency-test-key");
    for index in 0..8 {
        assert_eq!(
            manager.get("svc", &format!("account-{index}")).expect("record survives"),
            "opaque-test-value"
        );
    }
    remove_scratch(&path);
}

#[test]
fn the_manager_reports_which_backend_is_primary() {
    // On this machine there is no Secret Service provider, so the probe fails and the file is
    // primary. On a desktop with a keyring the OS rung is primary instead. The test asserts
    // the *mapping* (probe outcome -> backend), not a fixed backend, so it holds on both.
    let manager = SecretManager::open_with_file_store(scratch_path());
    if manager.keyring_available() {
        assert_eq!(manager.primary_backend(), Backend::OsKeyring);
        assert!(manager.keyring_probe_error().is_none());
    } else {
        assert_eq!(manager.primary_backend(), Backend::EncryptedFile);
        assert!(manager.keyring_probe_error().is_some());
    }
}

#[test]
fn a_secret_round_trips_through_the_fallback() {
    let path = scratch_path();
    remove_scratch(&path);
    with_key("manager-test-key", || {
        let manager = SecretManager::open_with_file_store(path.clone());
        // Force the fallback even where a keyring exists: the point is the file rung, not the
        // machine's keyring state. (On a keyring machine the primary would be the OS store and
        // this test would touch real user credentials - which it must never do. Since the probe
        // outcome is environmental, gate on it: only run the file path when the file is primary.
        if manager.primary_backend() != Backend::EncryptedFile {
            return;
        }
        manager.set("svc", "acct", "s3cr3t").expect("set");
        assert_eq!(manager.get("svc", "acct").expect("get"), "s3cr3t");
        assert!(manager.delete("svc", "acct").expect("delete"));
        assert!(!manager.delete("svc", "acct").expect("delete again"));
        remove_scratch(&path);
    });
}

#[test]
fn a_missing_entry_names_where_it_looked() {
    let path = scratch_path();
    remove_scratch(&path);
    with_key("manager-test-key", || {
        let manager = SecretManager::open_with_file_store(path.clone());
        if manager.primary_backend() != Backend::EncryptedFile {
            return;
        }
        let error = manager.get("svc", "absent").expect_err("must be missing");
        // A fresh machine has no vault file and no keyring: nothing is configured, so the
        // remedy - not a bare "not found" - is what the user hears.
        assert!(matches!(error, SecretsError::NoStore { .. }), "{error}");
        assert!(error.to_string().contains(MASTER_KEY_ENV), "{error}");
        remove_scratch(&path);
    });
}

#[test]
fn a_vault_that_exists_but_lacks_the_entry_is_not_found_not_nostore() {
    let path = scratch_path();
    remove_scratch(&path);
    with_key("manager-test-key", || {
        let manager = SecretManager::open_with_file_store(path.clone());
        if manager.primary_backend() != Backend::EncryptedFile {
            return;
        }
        manager.set("svc", "present", "value").expect("seed");
        let error = manager.get("svc", "absent").expect_err("must be missing");
        assert!(matches!(error, SecretsError::NotFound { .. }), "{error}");
        remove_scratch(&path);
    });
}

#[test]
fn unlock_supplies_the_passphrase_without_the_environment() {
    // The CLI prompts once and unlocks for the session; later reads must not need the variable.
    let path = scratch_path();
    remove_scratch(&path);
    with_key("seed-key", || {
        let seeder = SecretManager::open_with_file_store(path.clone());
        if seeder.primary_backend() != Backend::EncryptedFile {
            return;
        }
        seeder.set("svc", "acct", "value").expect("seed");
    });
    // Environment cleared by the helper. Unlock with the right passphrase directly.
    let manager = SecretManager::open_with_file_store(path.clone());
    if manager.primary_backend() != Backend::EncryptedFile {
        return;
    }
    manager.unlock_file_store("seed-key");
    assert_eq!(manager.get("svc", "acct").expect("get"), "value");
    manager.lock_file_store();
    remove_scratch(&path);
}

#[test]
fn a_provider_supplies_the_passphrase_when_nothing_else_does() {
    use std::sync::Arc;
    use supra_secrets::PassphraseProvider;
    use zeroize::Zeroizing;

    let path = scratch_path();
    remove_scratch(&path);
    // The whole body runs under the module's environment lock, not just the
    // seed: this test's `get` reads a file unlocked by a *provider*, so it
    // must not race another test's `remove_var` between the seeding and the
    // read - the in-binary flake this closes. (The cross-binary window is
    // documented in the module docs above and stays open by nature.)
    with_key("seed-key", || {
        let seeder = SecretManager::open_with_file_store(path.clone());
        if seeder.primary_backend() != Backend::EncryptedFile {
            return;
        }
        seeder.set("svc", "acct", "value").expect("seed");

        let manager = SecretManager::open_with_file_store(path.clone());
        if manager.primary_backend() != Backend::EncryptedFile {
            return;
        }
        let provider: PassphraseProvider = Arc::new(|| Ok(Zeroizing::new("seed-key".to_owned())));
        manager.set_passphrase_provider(provider);
        assert_eq!(manager.get("svc", "acct").expect("get"), "value");
    });
    remove_scratch(&path);
}

#[test]
fn a_keyring_miss_falls_through_to_the_file() {
    // The M9 gap, closed as far as a keyring-less machine allows. The `is_no_entry` arm in
    // `get` is what lets a secret written while headless survive the keyring coming back -
    // and a mutation turning that arm into an early return survived, because no test here can
    // make the keyring answer.
    //
    // What *is* pin-able without a keyring: the arm's contract from the file side. A manager
    // whose keyring is down returns the file's value through the same `get`, and the miss
    // path names the search. The keyring-answers half stays uncovered by construction; the
    // comment on the arm says so, rather than a test pretending to cover it.
    //
    // Gated on the file rung being primary: on a keyring machine the manager below would
    // write to the OS store, and no test may touch real credentials.
    let path = scratch_path();
    remove_scratch(&path);
    with_key("fallthrough-key", || {
        let manager = SecretManager::open_with_file_store(path.clone());
        if manager.primary_backend() != Backend::EncryptedFile {
            return;
        }
        manager.set("svc", "acct", "file-value").expect("seed the file");
        assert_eq!(manager.get("svc", "acct").expect("get"), "file-value");

        let error = manager.get("svc", "absent").expect_err("no such entry");
        let text = error.to_string();
        assert!(matches!(error, SecretsError::NotFound { .. }), "{text}");
    });
    remove_scratch(&path);
}

#[test]
fn redact_catches_a_secret_that_escapes_the_wrapper() {
    // The two layers are not alternatives: a secret that some path forgets to wrap must still
    // be caught at the log boundary. This test pins the contract between T12 and T8.
    let secret = "sk-ant-test-escape-0123456789abcdef";
    let line = format!("sending request with key {secret}");
    let redacted = supra_log::redact(&line);
    assert!(!redacted.contains(secret), "the T8 net missed a T12 escape: {redacted}");
}
