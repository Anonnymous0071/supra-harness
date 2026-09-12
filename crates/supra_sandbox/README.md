# supra_sandbox

Process-isolation policy layer. **T16** of the stage sequence.

## What this stage is for

T16 is the host-side glue between the C++ sandbox (T4) and a turn-loop call
site. Three pieces live in one crate because the order between them is the
product:

- **The fd audit** ([`fd_audit`]) runs before the C side does. The T4 note
  is the binding: "descriptors inherited across `exec` remain usable. T16
  and T16.5 must close descriptors they do not intend to pass." A
  filesystem policy that does not also audit descriptors is a policy the
  child can defeat by reading the host's pipes.
- **The host-side spawn** ([`spawn`]) composes the audit, the guard
  (T12.5), and the FFI in that order. A refusal at any step returns
  without touching the next.
- **The process-tree budget** ([`tree::TreeBudget`]) catches what the
  guard cannot. The T12.5 note is the binding: "the env-marker guard
  layer can be stripped by a command that deliberately clears the
  environment, and the process-tree budget catches the consequence
  rather than the intent."

## What this stage is **not** for

T16.5 takes the persistent-PTY side; T16.6 takes the write-ahead
journal; T16.7 takes the reversibility classifier. This crate composes
them by reference - it does not own their shapes. A change to the
T16.7 matrix would land in `supra_types` and propagate; a change to
the C-side Landlock ABI would land in `supra_ffi` and propagate. T16
is the host-side glue, not the policy.

## Modules

| Module | Owns |
|---|---|
| `error` | `SandboxError` - the five refusal shapes the operator sees |
| `fd_audit` | `/proc/self/fd` walk + `FD_CLOEXEC` flag check |
| `policy` | `SandboxPolicy` over `supra_ffi::sandbox::Policy`, with allow-list |
| `spawn` | the three-step host-side spawn (audit → guard → FFI) |
| `tree` | `TreeBudget` - per-host child counter |

## Authority vs consent

A refusal is one of two shapes:

| Variant | Shape | Where to look |
| ------- | ----- | ------------- |
| `Authority` | the guard's seven layers refused the spawn | `supra_guard` |
| `Consent` | the permission gate said no | T16.7 |
| `LeakyDescriptor` | the audit found an fd without `FD_CLOEXEC` | the harness, not the user |
| `Unsupported` | the policy requested a feature the platform lacks | sandboxing tier |
| `Ffi` | the C side refused; the message names the step | `libsupra_sandbox` |

`yolo` skips consent; nothing here skips authority. A `LeakyDescriptor`
returned in `yolo` is still a refusal, because leaking a file descriptor
is a property of the host, not of consent.

## Mutation results

Seven mutations. Six caught, one was a control.

| Mutation | Verdict |
|---|---|
| M1: drop `tree.record_child()` | CAUGHT |
| M2: drop `audit_descriptors()` call | CAUGHT |
| M3: drop `tree.record_child()` (re-tested) | CAUGHT |
| M4: bypass `supra_guard::judge` | CAUGHT |
| M5: drop `fd > 2` filter in audit | CAUGHT |
| M6: drop `burst.fetch_add(1)` | CAUGHT |
| M7: drop `/dev/tty` from default allow list | CAUGHT |

The audit's *unit* tests check the helper directly; the test that runs
through `spawn` itself is what closes M2 and M1, because the helper
could change shape and the integration test still relies on the same
behaviour. The pattern recurs: a helper test pins the mechanism, an
end-to-end test pins the composition.

## Guard checks

Three structural checks, each probed:

- **fd > 2 filter is in the leak predicate.** The audit ignores fds
  0/1/2 even when the kernel reports them without CLOEXEC, because
  stdio is what the child needs to talk to the user. A probe that
  drops the filter and runs through `spawn` (a fake `/bin/true`) trips
  on `pipe:[N]` under CI and on `/dev/pts/N` under a terminal.
- **`audit_descriptors` is called from `spawn`.** A probe that
  comments out the call is closed by the end-to-end leak test, which
  creates a non-CLOEXEC descriptor and asserts the call refuses.
- **`tree.record_child` is the only counter site.** A probe that drops
  the call is closed by the end-to-end spawn test, which records
  `started + 1` post-spawn.

## Obligations left to later stages

- **T16.5** (`supra_shell`) takes the persistent-PTY side; this crate
  hands the underlying FFI a normal `Command` for that case.
- **T16.6** (`supra_journal`) snapshots before the byte lands; this
  crate's `rename` does not touch the filesystem today.
- **T16.7** owns the reversibility classifier; this crate carries the
  classification through `SpawnRequest::reversibility` and refuses
  nothing itself on that axis.
- **Executable integration** is pending: a future turn driver must call `spawn`
  for each peer command and share its `TreeBudget` with the TUI. The current
  `supra run` path does not execute tools or consume this crate.
