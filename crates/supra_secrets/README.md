# supra_secrets

Secret storage. **T12** of the stage sequence: the OS keyring as the primary store and an
encrypted file as the automatic fallback, with a `Secret<T>` wrapper that never leaks.

## Modules

| Module | Owns |
|---|---|
| `secret` | the wrapper: wiped on drop, `Debug`/`Display` reveal nothing, no `Clone`/`Eq`/`Serialize` |
| `file_store` | the encrypted fallback: AES-256-GCM vault, PBKDF2-HMAC-SHA256 key, atomic writes |
| `manager` | the ladder: keyring first, file second, with the probe outcome in the errors |
| `error` | one variant per failure, each carrying the remedy |

## What this stage is for

> A credential lives in exactly one store, and no diagnostic ever shows it.

Everything here serves the second half of that sentence. `Secret<T>` closes the three
ordinary leaks - `Debug` printing it, `Display` printing it, memory keeping it - and T8's
redactor is the net beneath for the paths this crate does not control.

## The ladder, and why it is probed rather than assumed

```text
OS keyring  ->  encrypted file  ->  a clear error, never a hang
```

`keyring` v4 reports `NoDefaultStore` within milliseconds when no Secret Service provider is
running - measured at ~10 ms on a machine with a session bus but no keyring daemon - so the
probe in `SecretManager::open` is cheap and cannot hang startup. A *blocking* probe would have
needed T7/T8's pre-flight treatment; it does not, because the failure is fast.

`get` searches the stores in order, but `set` writes to the primary only. A credential lives
in exactly one store, not in both: merging namespaces would make "where does my key live?"
depend on the history of which store was down when.

## The threat model, stated plainly

The encrypted file (AES-256-GCM, key from PBKDF2-HMAC-SHA256 over the stored salt) protects
the secret at rest: another user, a backup, a sync, or an accidental `cat` sees ciphertext.
It does **not** protect against an attacker who is already this user on this machine - the
passphrase has to come from somewhere, and on a headless box that somewhere is an environment
variable, which is as readable as the file by the same user.

This crate says so rather than implying otherwise.

## Decisions with measurements behind them

**100 000 PBKDF2 rounds, not OWASP's 600 000.** OWASP's floor assumes a login server that
derives one key per authentication; this store derives one key per process - the CLI prompts
once and holds the passphrase. Measured release builds on this machine: 100 000 rounds cost
~74 ms, 600 000 cost ~447 ms, and neither protects a weak passphrase. What would change the
number is a memory-hard KDF (Argon2, scrypt), which prices parallel guessing hardware out of
the game in a way iteration count cannot. Neither is pinned in the workspace; pulling one in
for a fallback store is a T30 dependency decision, not a constant to guess here.

The production count is asserted at compile time against a `cfg(not(test))`-only floor, and
the test suite runs at 1 000 rounds under `cfg(test)`. No `test-kdf` feature: a feature is one
`cargo add --features` away from any build that copies the line, while `cfg(test)` is set by
the compiler and by nothing else.

**The vault is 0600 before the first byte lands.** The temp file is restricted at creation,
not after the write: the process umask may be permissive, and a vault that is world-readable
between creation and chmod is a leak with a window. Rename preserves the mode; the destination
is re-asserted anyway, because a reviewer reading only `save` should not have to know that.

**No interactive prompt inside the library.** An earlier version prompted on `/dev/tty` when
no passphrase source existed. That hung the test runner - stdin was a terminal with nobody
behind it - and it would hang every daemon and tool harness the same way. A library that
blocks on input without an explicit opt-in hangs every non-interactive caller behind it. The
CLI registers its prompt through `set_passphrase_provider`; the library resolves memory,
environment, provider, in that order.

**A missing file is a state; an unreadable file is a fault.** `get` against a never-written
path returns `NotFound`, but `Permission denied` stays an `Io` error. Widening the match to
all `Io` turns "cannot read" into "no such record" and sends the user to re-enter a
credential the store holds but cannot read.

**No `StoreTooNew` without a version to compare.** The vault format is four fields with no
version slot, so the variant could never fire - and dead error variants are misleading API.

## Two test-isolation lessons, both about locks

**`static` items in different functions are different statics.** Two helpers each declaring
their own `GUARD` guard nothing against each other. One module-level lock, shared by every
helper, is what actually serialises `set_var`/`remove_var` across parallel tests.

**A lock held for part of a test serialises part of a test.** An earlier version seeded the
vault under the lock, then cleared and read outside it; parallel tests interleaved their
mutations and the suite failed intermittently. Everything - seed, clear, read - now happens
inside one helper body.

## Mutation results

Ten mutations against the suite. Two survived first, both for documented reasons.

| Mutation | Verdict |
|---|---|
| M1 decrypt ignores the derived key (zero key) | CAUGHT |
| M2 decrypt error loses the remedy | CAUGHT |
| M3 magic check disabled | CAUGHT |
| M4 every I/O error reads as empty | CAUGHT *(survived first)* |
| M5 replacement writes empty | CAUGHT |
| M6 tamper test tampers nothing (control: `^= 0x00`) | CAUGHT |
| M7 Debug prints the value | BUILD_FAIL *(survived as designed)* |
| M8 Display stops redacting | CAUGHT |
| M9 keyring miss never reaches the file | SURVIVED *(by construction)* |
| M10 empty env credential accepted | CAUGHT |

**M4** survived because no test distinguished "missing" from "unreadable". Closed with a
`0o000`-mode test asserting `Permission denied` stays an `Io` fault.

**M7** does not compile: `T` has no `Debug` bound, so printing `self.value` fails the build
rather than the suite. That is the mechanism working - the generic cannot leak what it
cannot even name - and BUILD_FAIL is the correct verdict, not a gap.

**M9** survives because no test on a keyring-less machine can make the keyring answer. The
arm stays anyway: without it a headless-written secret vanishes when the keyring comes
back, which is silent data loss. The comment on the arm says so; uncovered-by-construction
is not untested-by-neglect.

## Credential resolution (`supra_config::Config::provider_secret`)

T7's schema names exactly one source per provider; T12 resolves it. `api_key_env` is read
from the process environment, `api_key_keyring` through the manager's ladder, and the
result is a `SecretString` - never a bare `String`. Eight tests cover the ladder end to
end: env resolution, unset and empty variables, keyring-through-file, missing entries, no
source named, unknown providers, and the `None` attribution of `MissingCredential` (the
failure is about the world, not any one file).

## Obligations left to later stages

- **T13** opens one `SecretManager` per session and threads it into `provider_secret`. The
  manager probe is I/O, which is why resolution takes it as a parameter rather than
  constructing one: `resolve` stays a pure function over already-read layers.
- **T30** registers the terminal prompt via `set_passphrase_provider` and owns
  `--secrets-file` if it wants one. `SecretManager::open_with_file_store` is the seam.
- **T29** shows `primary_backend()` and `keyring_probe_error()` in diagnostics: "no OS
  keyring" is a state the user should see, not infer.
