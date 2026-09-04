# supra_config

Layered configuration. **T7** of the stage sequence: discovery, per-field precedence,
provenance, and fail-fast validation.

## Modules

| Module | Owns |
|---|---|
| `source` | the precedence ladder, and which sources the operator controls |
| `layer` | the file schema, per-layer validation, and error text that quotes no file content |
| `resolve` | per-field precedence, provenance, and the tightening asymmetry |
| `discover` | file locations, and reading them without hanging or leaking |
| `error` | one variant per failure, each carrying the remedy |

## The three ways configuration goes wrong

**A whole file overwriting another.** Precedence is per *field*. `ConfigLayer` is
all-`Option` and says only what its file said; `Config` is fully resolved. Two types,
because collapsing them is what makes layered configuration surprising — a project
file setting `[cohort] limit` would otherwise erase the user's `[thinking] budget`.

**A typo silently ignored.** `deny_unknown_fields` on every shape, and unrecognised
`SUPRA_CONFIG_*` variables refused too. The `toml` parser already names the offending
key and the keys expected instead, so refusing costs nothing in diagnosability.

**A value that fails much later.** Every bound is checked at load. `[cohort] limit = 0`
fails before the first request, not at cohort spawn. The one exception is named rather
than hidden: the per-model minimum reasoning budget is a provider fact T13 owns, and
T13 must check it at startup too or this setting stops being fail-fast.

## Two properties

**Resolution cannot fail.** Layers are validated individually, so combining them is a
total function — which also means a precedence bug cannot hide behind an error path.

**The result is immutable.** `Config` has no setter and no `&mut` accessor. That is
what "the thinking budget is frozen per session" amounts to in practice: not a rule to
remember, but the absence of a way to break it. `scripts/check-invariants.sh` fails the
build if a setter appears.

## The trust boundary

Exactly one source is not controlled by the person running supra: **the project
layer**. A repository is cloned from anywhere. So:

- a project file may not name a provider, an endpoint, or a credential source;
- a project file's permission mode may only make the session **stricter**.

Under plain precedence, `project` outranks `user`, so a cloned repository shipping
`[permission] mode = "yolo"` would get it. The mode therefore resolves in two passes:
ordinary precedence over operator-controlled layers only, then a pass in which a
non-operator-controlled layer may narrow the result.

The order of those passes is load-bearing and was wrong in the first implementation.
Applying the project's mode in the ordinary pass and narrowing afterwards leaves a
loosened value in place, because a pass that can only tighten cannot undo a loosening.
The exhaustive test missed it too, because it used `Cli` as the operator layer — which
outranks `Project` — so the bug only appeared when the operator layer sat *below* the
project. The test now walks every operator-controlled source.

**The cost, stated plainly:** a project pinning `ask` beats an explicit `--yolo`. That
follows the same rule as a project `Deny` surviving a session `Allow`. An operator who
wants to overrule the repository needs a flag that says so — `--ignore-project-config`,
which belongs to T30 with its own confirmation, exactly as `--sandbox off` does.

## Credentials cannot be in a config file

The schema has `api_key_env` and `api_key_keyring` — the *name* of an environment
variable or of a keyring entry — and no field that takes a key. A file that cannot hold
a secret cannot leak one, whether by being committed, pasted into a bug report, or read
by another user.

Three supporting decisions:

- `api_key` and `auth_token` are *declared*, as fields whose only behaviour is to be
  refused with the explanation of where the credential should go. `deny_unknown_fields`
  would refuse them anyway, but "unknown field" does not tell the reader the remedy.
- `api_key_env` must be shaped like an environment variable name. Without the check,
  pasting the key there would make supra look up a variable literally named `sk-...`,
  report a missing credential, and send the reader looking in the wrong place while the
  real key sat in a file.
- A rejected value is redacted to its first four characters and a length.

### The leak that the tests caught

`ConfigError::Invalid` originally carried the `toml` crate's `Display` verbatim, on the
reasoning that its message is already excellent — it names the line, the column, and
prints the offending source line with a caret under it.

That last part is the problem. The message refusing a pasted credential *quoted the
credential*:

```
2 | api_key = "sk-would-be-a-real-key"
  |           ^^^^^^^^^^^^^^^^^^^^^^^^
supra never reads a literal credential from a configuration file...
```

Into stderr, into the T8 log, and into the next bug report. The leak the schema exists
to prevent, reintroduced by the message announcing it. `describe_toml_error` now
reports the location and the cause and never the content.

One residual is named rather than glossed: for a mistyped *scalar*, the parser's cause
text includes the value (`invalid type: string "four", expected usize`). That surface
needs a value in a field of the wrong type, and removing it would leave "expected
usize" with nothing to compare against. The large, unconditional surface was the source
excerpt, and that is gone.

## Two defects found by a later audit

**IPv6 loopback was rejected.** The loopback exemption split the authority on `:`, which
yields `[` for `[::1]:8080` — so `http://[::1]:8080`, an ordinary local proxy, was refused
with a message about sending credentials in clear. The authority is now parsed properly and
loopback is decided by `IpAddr::is_loopback`, which covers the whole of `127.0.0.0/8` and
`::1`.

**And the first fix for it was itself a bypass.** Unwrapping the brackets and ignoring
whatever followed made `http://[::1].evil.example` read as loopback — an attacker-controlled
host served plaintext. Its own adversarial test caught it before it shipped. After the
closing bracket the only thing permitted is a numeric port.

The same reasoning is why loopback is *parsed* rather than prefix-matched:
`starts_with("127.")` would have accepted `127.evil.example`.

**Redaction echoed short multibyte values in full.** The guard was on `len()` — bytes —
while the truncation took characters, so a two-character CJK value was six bytes, passed the
guard, and was reproduced whole by the message whose entire purpose is not to reproduce it.
Both now count characters.

## Reading files

**The permission check is on the open handle.** Checking a path's mode and then opening
it is a time-of-check-to-time-of-use race. `read_private` opens first and inspects the
handle, so the mode reported and the bytes read belong to the same file.

**The type check is before the open.** Opening a FIFO read-only blocks until a writer
appears, so the type cannot be checked on the handle the way the mode is — by the time
there is a handle, a FIFO has already hung the process. The two checks answer different
questions: the pre-flight `stat` is about availability, the handle check about
correctness. The remaining race between them needs write access to the config
directory, and anyone with that can write whatever configuration they like.

**Only the user layer is held to `0600`.** A project file is normally committed and
cannot be. Its safety comes from the schema instead: it may not name a credential
source, so a world-readable project file exposes nothing worth hiding.

## Why `SUPRA_CONFIG_` and not `SUPRA_`

Fail-fast requires refusing unrecognised variables. But `SUPRA_CXX`,
`SUPRA_CPP_BUILD_DIR`, and `SUPRA_CMAKE_BUILD_TYPE` are real build variables, and T4
uses `SUPRA_SANDBOX_*`. Refusing unknown `SUPRA_*` would break a developer's own shell.
A reserved sub-prefix keeps both properties.

## Mutation results

Twelve mutations, all caught.

| Mutation | Verdict |
|---|---|
| M1 project layer joins ordinary mode precedence | CAUGHT |
| M2 the extra pass loosens instead of tightening | CAUGHT |
| M3 the project layer becomes operator-controlled | CAUGHT |
| M4 the mode check stops covering group | CAUGHT |
| M5 providers merge field-by-field | CAUGHT |
| M6 an unknown `SUPRA_CONFIG_*` variable is skipped | CAUGHT |
| M7 `api_key_env` accepts anything | CAUGHT |
| M8 plaintext `http` accepted anywhere | CAUGHT |
| M9 the pre-flight `stat` is removed | CAUGHT (as a hang) |
| M10 the env layer skips validation | CAUGHT |
| M11 resolution takes arrival order | CAUGHT |
| M12 the parse error echoes the file again | CAUGHT |
| FIX-C IPv6 loopback endpoints rejected | CAUGHT |
| FIX-C2 trailing junk after `]` treated as loopback | CAUGHT |
| FIX-C3 loopback prefix-matched instead of parsed | CAUGHT |
| FIX-D redaction guards on bytes, truncates characters | CAUGHT |

M9 is caught as a **hang** rather than a failed assertion — removing the pre-flight
`stat` makes the FIFO test block instead of fail, which is the behaviour the check
exists to prevent. `scripts/mutate.sh` gained a `timeout` around its Rust branch for
exactly this: an unbounded hang stalls the harness instead of reporting a verdict.

M1 and M5 are the two worth reading. M1 is the bug that was actually present. M5 is the
one whose consequence is not obvious: merging a provider field-by-field across layers
pairs one layer's credential with another layer's endpoint, which is how a key gets
sent to the wrong host.

## Obligations left to later stages

- **T12** owns the default credential lookup for a named provider, so a provider entry
  with neither `api_key_env` nor `api_key_keyring` is valid here.
- **T13** owns the per-model minimum reasoning budget and must check it at startup, and
  extends the `[providers.*]` schema with its `CachePolicy` fields.
- **T28.5** owns the terminal ambiguous-width setting. It is deliberately absent from
  this schema: the canonical type lives in `supra_ffi`, and T7 has no reason to depend
  on the C++ build to hold one enum.
- **T30** owns `--ignore-project-config`, the explicit escape hatch from the tightening
  rule, with its own confirmation.
