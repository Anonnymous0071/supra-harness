/// libsupra_sandbox - OS-level process isolation behind one C ABI.
///
/// ## Contract
///
/// Flat C ABI, `noexcept` by construction (`-fno-exceptions`): an exception
/// unwinding into Rust is undefined behaviour, so the machinery is removed
/// rather than merely unused.
///
/// Policy is a **struct**, never an argv string. That is a security property,
/// not ergonomics: building a command line from user-supplied paths is exactly
/// the injection surface T12.5 exists to remove.
///
/// **Fail-closed.** Every function that cannot apply the policy it was given
/// refuses to run the child. A sandbox that reports success while enforcing
/// nothing is worse than no sandbox, because the caller stops looking.
///
/// ## Backends
///
/// One ABI, several enforcement mechanisms, and the caller is always told which
/// one actually ran. `supra_sandbox_probe` reports the tier so a caller can
/// refuse to proceed rather than discover the gap later.
///
/// * **Linux, Landlock (preferred).** `unshare(NEWUSER|NEWNET|NEWPID|NEWIPC|
///   NEWUTS)` for process and network isolation, plus a Landlock ruleset for the
///   filesystem. No mount operations at all.
/// * **Linux, namespaces only.** Kernels without Landlock, or with the LSM
///   absent from the boot list. Process and network isolation hold; filesystem
///   policy degrades to what the invoking user's own permissions already allow,
///   and the tier says so.
/// * **macOS**, `sandbox_init` with a generated SBPL profile. Not yet
///   implemented.
/// * **Windows**, AppContainer plus a job object. Not yet implemented.
///
/// ### Why not bubblewrap
///
/// Measured, not assumed: applying a Landlock ruleset and then exec'ing `bwrap`
/// fails with `Failed to make / slave: Operation not permitted`, and it still
/// fails under a maximally permissive ruleset that grants write access
/// everywhere - so the cause is bwrap's mount setup, not a too-tight policy.
/// `no_new_privs` alone does not break it, ruling that out too.
///
/// The composition supra needs is *host applies policy, then execs the child*.
/// That direction does not work with bwrap under any ruleset. The reverse order
/// does work, but inverts the trust boundary: the confined program would be
/// responsible for confining itself.
///
/// The native path also enforces more precisely. Landlock
/// `LANDLOCK_ACCESS_NET_CONNECT_TCP` is per-port, where a network namespace is
/// all-or-nothing; a blocked connection reports `EACCES` rather than
/// `ENETUNREACH`.
///
/// bwrap remains available as an explicit fallback backend where Landlock is
/// missing and the caller prefers mount-based isolation.
///
/// ## What this does not defend against
///
/// Stated plainly, because a security boundary whose limits are undocumented
/// gets trusted for things it never covered:
///
/// * **Not a syscall filter.** No seccomp-bpf. A confined process can still
///   invoke any syscall its uid permits; it simply cannot reach files outside
///   its allowlist or open sockets.
/// * **Not a resource guarantee.** `rlimit` caps are advisory scheduling
///   pressure, not cgroup accounting. A busy loop still burns CPU.
/// * **Not protection against a kernel bug.** Landlock and namespaces are
///   kernel features; a kernel vulnerability defeats both.
/// * **Not isolation from already-open descriptors.** Anything inherited across
///   `exec` stays usable. The caller must close what it does not intend to pass.

#ifndef SUPRA_SANDBOX_H
#define SUPRA_SANDBOX_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ------------------------------------------------------------------------- */
/* Limits                                                                    */
/* ------------------------------------------------------------------------- */

/// Maximum filesystem rules in one policy.
#define SUPRA_SANDBOX_MAX_PATHS 64

/// Maximum permitted TCP ports in one policy.
#define SUPRA_SANDBOX_MAX_PORTS 16

/// Bytes of diagnostic text a failure can report.
#define SUPRA_SANDBOX_ERROR_LEN 256

/* ------------------------------------------------------------------------- */
/* Enforcement tier                                                          */
/* ------------------------------------------------------------------------- */

/// Which mechanism actually enforced the policy.
///
/// Reported rather than inferred. A caller that requires filesystem enforcement
/// must check this and refuse to proceed on a weaker tier; silently degrading is
/// how a sandbox becomes decorative.
typedef enum supra_sandbox_tier {
    /// No enforcement available. `supra_sandbox_spawn` refuses to run.
    SUPRA_SANDBOX_TIER_NONE = 0,
    /// Process and network isolation only; filesystem policy not enforced.
    SUPRA_SANDBOX_TIER_NAMESPACES = 1,
    /// Namespaces plus Landlock: filesystem and per-port network enforced.
    SUPRA_SANDBOX_TIER_LANDLOCK = 2,
    /// macOS `sandbox_init` with a generated SBPL profile.
    SUPRA_SANDBOX_TIER_SBPL = 3,
    /// Windows AppContainer plus a job object.
    SUPRA_SANDBOX_TIER_APPCONTAINER = 4
} supra_sandbox_tier;

/// Platform capabilities, resolved once at startup.
typedef struct supra_sandbox_capabilities {
    /// Best tier available here.
    uint8_t tier;
    /// Landlock ABI version, or 0 when unavailable. ABI 4 adds TCP rules; ABI 6
    /// adds abstract-socket and signal scoping.
    uint8_t landlock_abi;
    /// 1 when an unprivileged user namespace can be created.
    uint8_t user_namespaces;
    /// 1 when network isolation is available by any mechanism.
    uint8_t network_isolation;
    /// 1 when per-port TCP policy is available (Landlock ABI >= 4).
    uint8_t port_granular_network;
    /// 1 when `bwrap` was found on PATH.
    uint8_t bubblewrap_present;
    /// Human-readable explanation of any gap. Empty when the best tier is
    /// available.
    char detail[SUPRA_SANDBOX_ERROR_LEN];
} supra_sandbox_capabilities;

/// Probe the platform. Cheap and idempotent; the result is cached internally.
///
/// The Landlock check applies a throwaway ruleset in a forked child and
/// confirms a denial actually occurs. A reported ABI version only proves the LSM
/// is compiled in - not that it is enabled in the boot-time LSM list - and a
/// no-op sandbox that reports success is the failure mode this exists to
/// prevent.
void supra_sandbox_probe(supra_sandbox_capabilities* out);

/// Value that clears a testing tier override.
#define SUPRA_SANDBOX_TIER_UNSET 255

/// Force the reported tier. **Testing only.**
///
/// The fail-closed refusals cannot otherwise be exercised on a machine that does
/// support Landlock: the branch rejecting an unenforceable filesystem policy is
/// unreachable there, so a mutation deleting it survived every test. Rather than
/// leave a security-critical refusal permanently unverified, the tier can be
/// pinned to a weaker value.
///
/// Pass `SUPRA_SANDBOX_TIER_UNSET` to resume normal probing. Never call this from
/// production code: it makes the sandbox weaker on purpose.
void supra_sandbox_force_tier_for_testing(uint8_t tier);

/* ------------------------------------------------------------------------- */
/* Filesystem policy                                                         */
/* ------------------------------------------------------------------------- */

/// Access bits for one path rule.
///
/// Directory-only bits are rejected by the kernel when applied to a regular
/// file, so the implementation masks each rule to what the target's file type
/// accepts. A rule the kernel refuses is a *widened* sandbox, so such failures
/// abort the spawn rather than being ignored.
typedef enum supra_sandbox_access {
    SUPRA_SANDBOX_READ = 1u << 0,
    SUPRA_SANDBOX_WRITE = 1u << 1,
    SUPRA_SANDBOX_EXECUTE = 1u << 2,
    /// Create, delete, and rename beneath this path. Implies WRITE.
    SUPRA_SANDBOX_MANAGE = 1u << 3
} supra_sandbox_access;

typedef struct supra_sandbox_path_rule {
    /// Absolute path. A relative path is rejected: resolution would depend on
    /// the caller's working directory at an unpredictable moment.
    const char* path;
    /// Bitwise OR of supra_sandbox_access.
    uint32_t access;
} supra_sandbox_path_rule;

/* ------------------------------------------------------------------------- */
/* Policy                                                                    */
/* ------------------------------------------------------------------------- */

/// Network policy.
typedef enum supra_sandbox_network {
    /// No network at all. Default.
    SUPRA_SANDBOX_NET_NONE = 0,
    /// Only the ports in `allowed_ports`. Requires Landlock ABI >= 4; without
    /// it the spawn is refused rather than silently granting full access.
    SUPRA_SANDBOX_NET_PORTS = 1,
    /// Unrestricted. The host network namespace is retained.
    SUPRA_SANDBOX_NET_FULL = 2
} supra_sandbox_network;

/// Complete sandbox policy. Zero-initialise for the most restrictive setting:
/// no paths, no network, no process visibility.
typedef struct supra_sandbox_policy {
    supra_sandbox_path_rule paths[SUPRA_SANDBOX_MAX_PATHS];
    uint8_t path_count;

    /// supra_sandbox_network.
    uint8_t network;
    uint16_t allowed_ports[SUPRA_SANDBOX_MAX_PORTS];
    uint8_t port_count;

    /// 1 to isolate the PID namespace, hiding host processes.
    uint8_t isolate_processes;
    /// 1 to isolate IPC and UTS namespaces.
    uint8_t isolate_ipc;

    /// Maximum processes, 0 for no limit. Advisory scheduling pressure rather
    /// than an accounting guarantee.
    uint32_t max_processes;
    /// Address space cap in bytes, 0 for no limit.
    uint64_t max_address_space;
    /// CPU seconds, 0 for no limit.
    uint32_t max_cpu_seconds;
    /// Maximum file size in bytes, 0 for no limit.
    uint64_t max_file_size;

    /// Minimum acceptable tier. The spawn is refused when the platform cannot
    /// reach it. Set to SUPRA_SANDBOX_TIER_LANDLOCK to require filesystem
    /// enforcement; leave 0 to accept whatever is available.
    uint8_t required_tier;
} supra_sandbox_policy;

/// Initialise `policy` to the most restrictive setting.
void supra_sandbox_policy_init(supra_sandbox_policy* policy);

/// Append a filesystem rule.
///
/// @return 1 on success, 0 when the table is full or `path` is not absolute.
int supra_sandbox_policy_allow(supra_sandbox_policy* policy, const char* path, uint32_t access);

/// Permit one outbound TCP port. Switches `network` to
/// `SUPRA_SANDBOX_NET_PORTS`.
///
/// @return 1 on success, 0 when the table is full.
int supra_sandbox_policy_allow_port(supra_sandbox_policy* policy, uint16_t port);

/* ------------------------------------------------------------------------- */
/* Spawning                                                                  */
/* ------------------------------------------------------------------------- */

/// A running sandboxed process.
typedef struct supra_sandbox_process {
    /// Host-visible pid, or -1 when the spawn failed.
    int64_t pid;
    /// Tier that actually applied.
    uint8_t tier;
    /// Diagnostic text on failure. Empty on success.
    char error[SUPRA_SANDBOX_ERROR_LEN];
} supra_sandbox_process;

/// Command to run. Never a shell string: an argv vector cannot be
/// re-interpreted, and quoting bugs in a generated command line are a whole
/// vulnerability class this avoids by construction.
typedef struct supra_sandbox_command {
    /// Absolute path to the executable.
    const char* program;
    /// NULL-terminated argv. `argv[0]` is conventionally `program`.
    const char* const* argv;
    /// NULL-terminated `KEY=VALUE` list, or NULL to pass nothing. The
    /// environment is *not* inherited by default: it routinely carries
    /// credentials, and inheriting it silently would leak them into every
    /// sandboxed command.
    const char* const* envp;
    /// Working directory, or NULL for the current one. Must be readable under
    /// the policy or the child cannot start.
    const char* working_dir;
    /// Descriptors for the child. -1 means `/dev/null`.
    int stdin_fd;
    int stdout_fd;
    int stderr_fd;
} supra_sandbox_command;

/// Apply `policy` and execute `command`.
///
/// Fails closed. When the policy cannot be applied - a rule the kernel refuses,
/// a tier below `required_tier`, a `NET_PORTS` policy on a kernel without
/// per-port support - no process is started and `out->error` explains why.
///
/// Setup failures inside the forked child are reported through a CLOEXEC pipe,
/// so the caller learns *which* step failed rather than only that the child
/// exited. Diagnosing a sandbox that "just fails" is otherwise guesswork.
///
/// @return 1 when the child started, 0 on failure.
int supra_sandbox_spawn(const supra_sandbox_policy* policy, const supra_sandbox_command* command,
                        supra_sandbox_process* out);

/// Wait for a sandboxed process.
///
/// @param timeout_ms Milliseconds, or 0 to block indefinitely.
/// @param out_status Receives the exit code, or 128+signal when signalled.
/// @return 1 when it exited, 0 on timeout, -1 on error.
int supra_sandbox_wait(const supra_sandbox_process* process, uint32_t timeout_ms, int* out_status);

/// Terminate a sandboxed process and everything it started.
///
/// `SIGTERM` to the process group, then `SIGKILL` after `grace_ms`. The group is
/// the unit because a sandboxed command routinely spawns children, and killing
/// only the leader leaves them running.
///
/// @return 1 on success, 0 on failure.
int supra_sandbox_kill(const supra_sandbox_process* process, uint32_t grace_ms);

/* ------------------------------------------------------------------------- */
/* Self-identification                                                       */
/* ------------------------------------------------------------------------- */

/// Canonical path of the running executable.
///
/// Backs guard layer L4 in T12.5: comparing a candidate command against this
/// binary to refuse self-spawn. Path comparison alone is insufficient - a copy,
/// symlink, or rename defeats it - so `supra_sandbox_self_identity` is the
/// primary check and this exists for diagnostics.
///
/// @return Bytes written excluding the NUL, or 0 on failure.
size_t supra_sandbox_self_path(char* out, size_t cap);

/// Device and inode of the running executable.
///
/// Identity rather than name, so a copy or symlink cannot masquerade. Note that
/// a *duplicated* binary has a different inode and will not match; T12.5 layers
/// an environment marker to catch that case.
///
/// @return 1 on success, 0 on failure.
int supra_sandbox_self_identity(uint64_t* out_dev, uint64_t* out_ino);

/// Device and inode of the file `path` resolves to, following symlinks.
///
/// @return 1 on success, 0 when the path cannot be resolved.
int supra_sandbox_file_identity(const char* path, uint64_t* out_dev, uint64_t* out_ino);

#ifdef __cplusplus
}  /* extern "C" */
#endif

#endif /* SUPRA_SANDBOX_H */
