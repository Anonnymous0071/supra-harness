# libsupra_sandbox

OS-level process isolation behind one C ABI. **T4** of the supra-harness stage
sequence.

Enables `auto` permission mode in T16.7: reversible mutations run without asking,
which is only defensible if the blast radius is bounded.

## Linux is native, not a bubblewrap wrapper

The plan called for bubblewrap. Measurement rejected it.

The composition supra needs is **host applies policy, then execs the child**.
Applying a Landlock ruleset and then exec'ing `bwrap` fails:

```
bwrap: Failed to make / slave: Operation not permitted
```

Three follow-ups isolate the cause:

| Probe | Result |
|---|---|
| Maximally permissive ruleset (write granted everywhere) | **still fails** — not a too-tight policy |
| `no_new_privs` alone, then bwrap | works — not the `prctl` |
| bwrap outer, Landlock inner | works, but inverts the trust boundary |

The last ordering is the only one bwrap accepts, and it makes the confined
program responsible for confining itself. So the Linux backend is
`unshare(NEWUSER|NEWNET|NEWPID|NEWIPC|NEWUTS)` plus Landlock, with **no mount
operations at all** — no `pivot_root`, no `/proc` remount, nothing that trips the
conflict.

It also enforces more precisely. Landlock `NET_CONNECT_TCP` is **per-port**;
a network namespace is all-or-nothing. A blocked connection reports `EACCES`
rather than `ENETUNREACH`.

The shipped backend does not invoke bwrap. Its presence is informational only;
there is no bwrap fallback when Landlock is missing.

## Tiers are reported, never assumed

```c
supra_sandbox_capabilities caps;
supra_sandbox_probe(&caps);
// caps.tier: NONE | NAMESPACES | LANDLOCK | SBPL | APPCONTAINER
```

Capability probing is backend-specific and fail-closed. Linux applies a
throwaway Landlock ruleset in a forked child and confirms a denial; macOS
performs a behavioral `sandbox_init` denial probe; Windows verifies that an
AppContainer profile can be created. Unsupported platforms report `NONE`.
Linux and macOS use behavioral denials rather than trusting reported API or
kernel availability; if those probes fail, the backend does not claim
enforcement.

A filesystem policy on a platform that cannot enforce it **refuses to run**. A
sandbox reporting success while enforcing nothing is worse than no sandbox,
because the caller stops looking.

## Linux verified escape attempts

The Linux escape suite forks and execs real payloads and inspects their effects.
macOS and Windows run corresponding native probe, enforcement, refusal, timeout,
and process-tree cleanup checks through `sandbox_native_test`; Windows launches
payloads with native process creation. Asserting that a policy struct was
populated proves nothing about enforcement.

| Attempt | Outcome |
|---|---|
| read outside allowlist | denied |
| write outside allowlist | denied, and the file does not appear |
| `../../etc/passwd` traversal | denied — Landlock is inode-based, so traversal is meaningless to it |
| TCP connect | denied |
| `chroot` then reach out | denied — chroot cannot widen an inode policy |
| grandchild via `sh -c "sh -c ..."` | denied — the domain is inherited |
| install a wider ruleset | impossible — rulesets are monotonic |
| host processes visible | `/proc` unreadable, payload is pid 1 |
| host environment | not inherited; a canary variable does not leak |

## Contract

Policy is a **struct, never an argv string**. That is a security property:
building a command line from user-supplied paths is the injection surface T12.5
exists to remove. Commands are argv vectors for the same reason.

Zero-initialising a policy yields the most restrictive setting, so a caller who
forgets a field gets *less* access rather than more.

Linux and macOS report child setup failures through a **CLOEXEC report pipe**.
Windows reports AppContainer, ACL, explicit-handle, process-creation, and
job-object setup failures directly through the process error buffer. Each path
identifies the failed step rather than reporting only that startup failed.

### What this does not defend against

Stated plainly, because an undocumented boundary gets trusted for things it never
covered:

- **Not a syscall filter.** No seccomp-bpf. A confined process can invoke any
  syscall its uid permits; it simply cannot reach files outside its allowlist or
  open sockets.
- **Not a resource guarantee.** `rlimit` is scheduling pressure, not cgroup
  accounting. A busy loop still burns CPU.
- **Not protection against a kernel bug.**
- **Not isolation from inherited descriptors or handles.** On POSIX,
  descriptors left open and inheritable remain usable after `exec`, so callers
  must close them or mark unintended descriptors `CLOEXEC`. Windows passes only
  the handles explicitly selected for inheritance.

## Layout

```
include/supra/sandbox.h  public ABI
src/policy.cpp           construction and validation, all platforms
src/landlock.cpp         Linux Landlock wrapper, ABI 1-7
src/linux_spawn.cpp      Linux namespaces, uid mapping, rlimits, exec
src/macos_spawn.cpp      macOS sandbox_init/SBPL backend
src/windows_spawn.cpp    Windows AppContainer, explicit handles, job object
src/self_identity.cpp    platform file identity for T12.5 guard layer L4
src/unsupported.cpp      other platforms: refuse, never run unconfined
tests/                   platform-specific CTest suites
```

## Two bugs found by testing, not by reading

**`spawn` blocked until the payload exited.** Measured 5004 ms for a 5000 ms
`sleep`. The PID-namespace intermediate never execs, so `CLOEXEC` never closed
its copy of the report pipe and the parent's `read()` waited for the whole tree.
This also made `kill` untestable: by the time `spawn` returned, the process was
always already gone.

**`readlink` truncated silently.** A short buffer returned a partial path, and a
truncated path can resolve to a *different file* — which would make guard layer
L4 compare against the wrong inode. It now fails instead.

## Mutation testing

`scripts/mutate.sh` exists because my first inline harness **lied**. It ignored
the build exit code, so a mutation rejected by `-Werror` left the previous correct
binary in place, ctest passed, and it reported `SURVIVED`. Three mutations were
recorded as test gaps having never been compiled.

The script distinguishes three outcomes, not two:

| Verdict | Meaning |
|---|---|
| `CAUGHT` | built, suite failed → invariant is tested |
| `SURVIVED` | built, suite passed → real gap |
| `BUILD_FAIL` | did not compile → **not a verdict** |

### Results

| Mutation | Verdict |
|---|---|
| treat all targets as directories | CAUGHT |
| treat all targets as regular files | CAUGHT |
| skip `restrict_self` | CAUGHT |
| skip `no_new_privs` | CAUGHT |
| ignore `stat` failure on a policy path | CAUGHT |
| skip `setsid` | CAUGHT |
| inherit host environment | CAUGHT |
| allow unenforceable filesystem policy | CAUGHT |
| allow unenforceable port policy | CAUGHT |
| run at tier `NONE` | CAUGHT |
| ignore `required_tier` | CAUGHT |
| ignore `add_rule` failure | **survives — see below** |

Two gaps were real and both are now closed:

**Directory-bit masking.** The write test did `echo > .../file` on a path that
**persisted between runs**. Truncating an existing file needs only `WRITE_FILE`,
so `MAKE_REG`, `MAKE_DIR`, `REMOVE_FILE`, and `REMOVE_DIR` were never exercised.
The suite now creates a fresh file, creates a directory, writes inside it, and
deletes both, having cleaned the workspace first.

**Fail-closed refusals were unreachable.** On a kernel that supports Landlock,
the branch rejecting an unenforceable policy never executes — so the refusal
protecting users on *older* kernels was untested on the only kernel available.
`supra_sandbox_force_tier_for_testing` pins the tier downward to reach it.

### The one that still survives, and why

`ignore add_rule failure` survives, and that is a property of the code rather
than a hole in the suite. Probed directly: with correctly masked bits,
`landlock_add_rule` returns 0 for directories, regular files, device nodes,
`/proc`, `/sys`, and for 20 000 consecutive rules. No input was found that fails.
It rejects only mismatched bits, which masking prevents.

So the check is defence in depth against a *future* masking regression, and no
single-fault test can reach it. Reaching it requires two simultaneous defects.
The comment at the site records this, so the next person does not mistake it for
untested code.

## Building

```sh
just build-cpp
just test-cpp        # skips escape attempts below the Landlock tier, loudly
just tidy
```

Also runs under ASan+UBSan in CI. The escape suite **skips loudly** rather than
passing vacuously when the platform cannot reach the Landlock tier: a green suite
that verified nothing is the worst possible outcome for a security boundary.
