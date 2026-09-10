// Linux backend: namespaces for process and network isolation, Landlock for the
// filesystem, rlimits for resource pressure.
//
// No mount operations. That is the design decision that makes the whole thing
// compose - see the measurement note in the header - and it also removes an
// entire class of bug, since there is no pivot_root, no /proc remount, and no
// temporary directory to clean up.
//
// Ordering inside the child is not arbitrary. Each step depends on the previous
// one having succeeded:
//
//   1. setsid            - own process group, so kill() reaches the whole tree
//   2. unshare           - namespaces; NEWUSER first so the rest are permitted
//   3. uid/gid maps      - without these the child has no valid identity
//   4. fork              - CLONE_NEWPID applies to the NEXT child, not this one
//   5. redirect fds      - before Landlock, since /dev/null may not be reachable after
//   6. chdir             - before Landlock, for the same reason
//   7. rlimits           - cheap, and must precede exec
//   8. Landlock          - irreversible, so last
//   9. exec

#ifdef __linux__

#include <cerrno>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>

#include <fcntl.h>
#include <sched.h>
#include <signal.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <sys/wait.h>
#include <unistd.h>

#include "internal.hpp"
#include "landlock.hpp"
#include "supra/sandbox.h"

namespace {

using supra::sandbox::detail::appendTruncated;
using supra::sandbox::detail::setError;
using supra::sandbox::detail::setErrorErrno;

/// Cached capability probe. Landlock enforcement costs a fork, so it runs once.
supra_sandbox_capabilities g_caps{};
bool g_caps_ready = false;

/// Testing-only tier override. See supra_sandbox_force_tier_for_testing.
std::uint8_t g_forced_tier = SUPRA_SANDBOX_TIER_UNSET;

/// Write a small string to a proc file. Used for the uid/gid maps.
bool writeProcFile(const char* path, const char* value) {
    const int fd = ::open(path, O_WRONLY | O_CLOEXEC);
    if (fd < 0) {
        return false;
    }
    const std::size_t len = std::strlen(value);
    const ssize_t written = ::write(fd, value, len);
    ::close(fd);
    return written == static_cast<ssize_t>(len);
}

/// Can an unprivileged user namespace be created?
///
/// Tested by actually creating one in a forked child. The sysctls that gate this
/// vary between distributions, so reading them is less reliable than trying.
/// The canary must mirror the spawn path, not just its first step: Ubuntu
/// 24.04's AppArmor restriction allows `unshare(CLONE_NEWUSER)` and then denies
/// the `uid_map` write, so a canary that stopped at unshare would report a
/// namespace the sandbox then cannot use. Child-side code is raw syscalls
/// only - async-signal-safe in the forked child, no stdio, no allocation.
bool userNamespacesAvailable() {
    // Captured before the fork: after unshare(CLONE_NEWUSER) the child is
    // nobody (65534) until the map is written, so reading the ids inside the
    // child maps 65534-to-65534, which the kernel refuses. The spawn path
    // below makes the same capture for host_uid/host_gid.
    const auto host_uid = static_cast<unsigned>(::getuid());
    const pid_t pid = ::fork();
    if (pid < 0) {
        return false;
    }
    if (pid == 0) {
        if (::unshare(CLONE_NEWUSER) != 0) {
            ::_exit(1);
        }
        // Best-effort, matching the spawn path: pre-3.19 kernels have no
        // setgroups file and allow the map write without the deny.
        const int groups_fd = ::open("/proc/self/setgroups", O_WRONLY);
        if (groups_fd >= 0) {
            const ssize_t denied = ::write(groups_fd, "deny", 4);
            static_cast<void>(denied);
            ::close(groups_fd);
        }
        const int map_fd = ::open("/proc/self/uid_map", O_WRONLY);
        if (map_fd < 0) {
            ::_exit(2);
        }
        // "0 <uid> 1": the stock mapping unshare(1) writes, which an
        // unprivileged process may always install for its own uid.
        char digits[10];
        std::size_t count = 0;
        unsigned uid = host_uid;
        do {
            digits[count++] = static_cast<char>('0' + uid % 10);
            uid /= 10;
        } while (uid != 0);
        char map[32];
        std::size_t used = 0;
        map[used++] = '0';
        map[used++] = ' ';
        for (std::size_t i = count; i > 0; --i) {
            map[used++] = digits[i - 1];
        }
        map[used++] = ' ';
        map[used++] = '1';
        map[used++] = '\n';
        const ssize_t written = ::write(map_fd, map, used);
        ::close(map_fd);
        ::_exit(written == static_cast<ssize_t>(used) ? 0 : 3);
    }
    int status = 0;
    if (::waitpid(pid, &status, 0) < 0) {
        return false;
    }
    return WIFEXITED(status) && WEXITSTATUS(status) == 0;
}

bool onPath(const char* program) {
    const char* path_env = std::getenv("PATH");
    if (path_env == nullptr || program == nullptr) {
        return false;
    }

    char buffer[4096];
    std::size_t start = 0;
    const std::size_t path_len = std::strlen(path_env);
    const std::size_t program_len = std::strlen(program);

    for (std::size_t i = 0; i <= path_len; ++i) {
        if (path_env[i] != ':' && path_env[i] != '\0') {
            continue;
        }
        const std::size_t segment_len = i - start;
        // Bounded throughout: memcpy with an explicit length rather than strcpy,
        // so the compiler and the analyser can both see the bound. `+ 2` covers
        // the separator and the NUL.
        if (segment_len > 0 && segment_len + program_len + 2 <= sizeof buffer) {
            std::memcpy(buffer, path_env + start, segment_len);
            buffer[segment_len] = '/';
            std::memcpy(buffer + segment_len + 1, program, program_len);
            buffer[segment_len + 1 + program_len] = '\0';
            if (::access(buffer, X_OK) == 0) {
                return true;
            }
        }
        start = i + 1;
    }
    return false;
}

/// Report a child-side setup failure through the CLOEXEC pipe, then exit.
///
/// The pipe is what makes a failed spawn diagnosable: without it the caller sees
/// only a non-zero exit and has to guess which of nine steps failed.
[[noreturn]] void failChild(int pipe_fd, const char* step, int err) {
    char message[SUPRA_SANDBOX_ERROR_LEN];
    if (err != 0) {
        setErrorErrno(message, sizeof message, step, err);
    } else {
        setError(message, sizeof message, step);
    }
    const ssize_t ignored = ::write(pipe_fd, message, std::strlen(message));
    static_cast<void>(ignored);
    ::_exit(127);
}

void applyRlimits(const supra_sandbox_policy& policy) {
    // Best-effort: an rlimit that cannot be lowered is not a security failure,
    // it is scheduling pressure that did not take effect. The header says so.
    if (policy.max_processes > 0) {
        const ::rlimit limit{policy.max_processes, policy.max_processes};
        static_cast<void>(::setrlimit(RLIMIT_NPROC, &limit));
    }
    if (policy.max_address_space > 0) {
        const ::rlimit limit{policy.max_address_space, policy.max_address_space};
        static_cast<void>(::setrlimit(RLIMIT_AS, &limit));
    }
    if (policy.max_cpu_seconds > 0) {
        const ::rlimit limit{policy.max_cpu_seconds, policy.max_cpu_seconds};
        static_cast<void>(::setrlimit(RLIMIT_CPU, &limit));
    }
    if (policy.max_file_size > 0) {
        const ::rlimit limit{policy.max_file_size, policy.max_file_size};
        static_cast<void>(::setrlimit(RLIMIT_FSIZE, &limit));
    }
}

/// Point a child descriptor at `target_fd`, or at /dev/null when -1.
bool redirect(int target_fd, int child_fd) {
    if (target_fd < 0) {
        const int null_fd = ::open("/dev/null", child_fd == STDIN_FILENO ? O_RDONLY : O_WRONLY);
        if (null_fd < 0) {
            return false;
        }
        const bool ok = ::dup2(null_fd, child_fd) >= 0;
        ::close(null_fd);
        return ok;
    }
    return ::dup2(target_fd, child_fd) >= 0;
}

}  // namespace

extern "C" {

void supra_sandbox_probe(supra_sandbox_capabilities* out) {
    if (out == nullptr) {
        return;
    }

    if (g_caps_ready) {
        *out = g_caps;
        // Apply the testing override on the cached path too, or the first probe
        // after setting it would still report the real tier.
        if (g_forced_tier != SUPRA_SANDBOX_TIER_UNSET) {
            out->tier = g_forced_tier;
            if (out->tier < SUPRA_SANDBOX_TIER_LANDLOCK) {
                out->landlock_abi = 0;
                out->port_granular_network = 0;
                setError(out->detail, sizeof out->detail,
                         "tier forced below Landlock for testing; filesystem policy NOT enforced");
            }
        }
        return;
    }

    supra_sandbox_capabilities caps{};
    caps.bubblewrap_present = onPath("bwrap") ? 1 : 0;
    caps.user_namespaces = userNamespacesAvailable() ? 1 : 0;

    const int abi = supra::sandbox::landlock::abiVersion();
    const bool landlock_works = abi > 0 && supra::sandbox::landlock::enforces();

    caps.landlock_abi = static_cast<std::uint8_t>(landlock_works ? abi : 0);
    caps.port_granular_network = landlock_works && abi >= 4 ? 1 : 0;
    caps.network_isolation = caps.user_namespaces != 0 || caps.port_granular_network != 0 ? 1 : 0;

    if (landlock_works && caps.user_namespaces != 0) {
        caps.tier = SUPRA_SANDBOX_TIER_LANDLOCK;
        if (abi < 4) {
            std::snprintf(caps.detail, sizeof caps.detail,
                          "Landlock ABI %d: per-port network policy needs ABI 4 (kernel 6.7+)",
                          abi);
        } else if (abi < 6) {
            std::snprintf(caps.detail, sizeof caps.detail,
                          "Landlock ABI %d: abstract-socket and signal scoping need ABI 6", abi);
        }
    } else if (caps.user_namespaces != 0) {
        caps.tier = SUPRA_SANDBOX_TIER_NAMESPACES;
        if (abi > 0 && !landlock_works) {
            // The distinction matters: compiled-in but inert looks identical to
            // available unless the probe actually tests a denial.
            std::snprintf(caps.detail, sizeof caps.detail,
                          "Landlock reports ABI %d but does not enforce (not in the boot-time LSM "
                          "list); filesystem policy NOT enforced",
                          abi);
        } else {
            setError(caps.detail, sizeof caps.detail,
                     "Landlock unavailable (needs kernel 5.13+); filesystem policy NOT enforced");
        }
    } else {
        caps.tier = SUPRA_SANDBOX_TIER_NONE;
        setError(caps.detail, sizeof caps.detail,
                 "unprivileged user namespaces unavailable; no isolation possible");
    }

    g_caps = caps;
    g_caps_ready = true;

    // Testing override, applied after caching so clearing it restores the real
    // probe result without another fork.
    if (g_forced_tier != SUPRA_SANDBOX_TIER_UNSET) {
        caps.tier = g_forced_tier;
        if (caps.tier < SUPRA_SANDBOX_TIER_LANDLOCK) {
            caps.landlock_abi = 0;
            caps.port_granular_network = 0;
            setError(caps.detail, sizeof caps.detail,
                     "tier forced below Landlock for testing; filesystem policy NOT enforced");
        }
    }

    *out = caps;
}

void supra_sandbox_force_tier_for_testing(std::uint8_t tier) {
    g_forced_tier = tier;
}

int supra_sandbox_spawn(const supra_sandbox_policy* policy, const supra_sandbox_command* command,
                        supra_sandbox_process* out) {
    if (out == nullptr) {
        return 0;
    }
    *out = supra_sandbox_process{};
    out->pid = -1;

    if (command == nullptr || command->program == nullptr || command->argv == nullptr) {
        setError(out->error, sizeof out->error, "command, program, and argv are required");
        return 0;
    }
    if (!supra::sandbox::detail::validatePolicy(policy, out->error, sizeof out->error)) {
        return 0;
    }

    supra_sandbox_capabilities caps{};
    supra_sandbox_probe(&caps);

    if (caps.tier == SUPRA_SANDBOX_TIER_NONE) {
        setError(out->error, sizeof out->error, caps.detail);
        return 0;
    }
    if (policy->required_tier != 0 && caps.tier < policy->required_tier) {
        // No `%u` here: GCC 12+ `-Wformat-truncation` under `-Werror` rejects
        // printing the tier numbers into this buffer, and the numbers are
        // already in the policy the caller holds. The refusal names the
        // decision ("requires tier"), not the operands.
        setError(out->error, sizeof out->error, "policy requires tier above available: ");
        appendTruncated(out->error, sizeof out->error, caps.detail);
        return 0;
    }
    // Refuse rather than run unconfined. A filesystem policy that is not
    // enforced but reports success is the failure this library exists to
    // prevent.
    if (policy->path_count > 0 && caps.tier < SUPRA_SANDBOX_TIER_LANDLOCK) {
        setError(out->error, sizeof out->error,
                 "policy specifies filesystem rules but this platform cannot enforce them: ");
        appendTruncated(out->error, sizeof out->error, caps.detail);
        return 0;
    }
    if (policy->network == SUPRA_SANDBOX_NET_PORTS && caps.port_granular_network == 0) {
        setError(out->error, sizeof out->error,
                 "policy requests per-port network but this platform cannot enforce it: ");
        appendTruncated(out->error, sizeof out->error, caps.detail);
        return 0;
    }

    // CLOEXEC so a successful exec closes it, which is how the parent
    // distinguishes "setup failed" from "started fine".
    int report[2] = {-1, -1};
    if (::pipe2(report, O_CLOEXEC) != 0) {
        setErrorErrno(out->error, sizeof out->error, "pipe2", errno);
        return 0;
    }

    const uid_t host_uid = ::getuid();
    const gid_t host_gid = ::getgid();

    const pid_t outer = ::fork();
    if (outer < 0) {
        setErrorErrno(out->error, sizeof out->error, "fork", errno);
        ::close(report[0]);
        ::close(report[1]);
        return 0;
    }

    if (outer == 0) {
        ::close(report[0]);
        const int err_fd = report[1];

        // Own process group, so supra_sandbox_kill can signal the whole tree.
        if (::setsid() < 0) {
            failChild(err_fd, "setsid", errno);
        }

        int flags = CLONE_NEWUSER;
        if (policy->network != SUPRA_SANDBOX_NET_FULL) {
            flags |= CLONE_NEWNET;
        }
        if (policy->isolate_processes != 0) {
            flags |= CLONE_NEWPID;
        }
        if (policy->isolate_ipc != 0) {
            flags |= CLONE_NEWIPC | CLONE_NEWUTS;
        }

        if (::unshare(flags) != 0) {
            failChild(err_fd, "unshare", errno);
        }

        // setgroups must be denied before writing gid_map, or the write is
        // refused. Ignore failure: on some kernels the file is absent.
        static_cast<void>(writeProcFile("/proc/self/setgroups", "deny"));

        char mapping[64];
        std::snprintf(mapping, sizeof mapping, "0 %u 1", static_cast<unsigned>(host_uid));
        if (!writeProcFile("/proc/self/uid_map", mapping)) {
            failChild(err_fd, "write /proc/self/uid_map", errno);
        }
        std::snprintf(mapping, sizeof mapping, "0 %u 1", static_cast<unsigned>(host_gid));
        if (!writeProcFile("/proc/self/gid_map", mapping)) {
            failChild(err_fd, "write /proc/self/gid_map", errno);
        }

        // CLONE_NEWPID takes effect for the next child, so a second fork is
        // needed for the payload to become pid 1 in its namespace.
        if (policy->isolate_processes != 0) {
            const pid_t inner = ::fork();
            if (inner < 0) {
                failChild(err_fd, "fork for PID namespace", errno);
            }
            if (inner > 0) {
                // Close this copy of the report pipe before waiting.
                //
                // CLOEXEC closes the payload's copy when it execs, but this
                // intermediate never execs, so its copy would keep the pipe open
                // and block the parent's read() until the whole tree exits. That
                // turned spawn() into a synchronous call - measured at 5004ms for
                // a 5000ms payload - which in turn made kill() untestable,
                // because by the time spawn returned the process was always
                // already gone.
                ::close(err_fd);

                // Relay the payload's status so the caller sees the real exit
                // code rather than this intermediate's.
                int inner_status = 0;
                if (::waitpid(inner, &inner_status, 0) < 0) {
                    ::_exit(126);
                }
                if (WIFEXITED(inner_status)) {
                    ::_exit(WEXITSTATUS(inner_status));
                }
                if (WIFSIGNALED(inner_status)) {
                    ::_exit(128 + WTERMSIG(inner_status));
                }
                ::_exit(126);
            }
        }

        // Descriptors and cwd before Landlock: /dev/null and the working
        // directory may be unreachable once the ruleset is in force.
        if (!redirect(command->stdin_fd, STDIN_FILENO)) {
            failChild(err_fd, "redirect stdin", errno);
        }
        if (!redirect(command->stdout_fd, STDOUT_FILENO)) {
            failChild(err_fd, "redirect stdout", errno);
        }
        if (!redirect(command->stderr_fd, STDERR_FILENO)) {
            failChild(err_fd, "redirect stderr", errno);
        }

        if (command->working_dir != nullptr && ::chdir(command->working_dir) != 0) {
            failChild(err_fd, "chdir", errno);
        }

        applyRlimits(*policy);

        if (caps.tier >= SUPRA_SANDBOX_TIER_LANDLOCK) {
            char landlock_error[SUPRA_SANDBOX_ERROR_LEN];
            if (supra::sandbox::landlock::apply(*policy, caps.landlock_abi, landlock_error,
                                                sizeof landlock_error) != 1) {
                failChild(err_fd, landlock_error, 0);
            }
        }

        // The environment is not inherited: it routinely carries credentials, and
        // passing it silently would leak them into every sandboxed command.
        static const char* const kEmptyEnv[] = {nullptr};
        const char* const* envp = command->envp != nullptr ? command->envp : kEmptyEnv;

        ::execve(command->program, const_cast<char* const*>(command->argv),
                 const_cast<char* const*>(envp));
        failChild(err_fd, "execve", errno);
    }

    ::close(report[1]);

    // A successful exec closes the pipe, so a read of 0 bytes means the child
    // started. Any bytes are a setup failure message.
    char message[SUPRA_SANDBOX_ERROR_LEN];
    std::memset(message, 0, sizeof message);
    const ssize_t received = ::read(report[0], message, sizeof message - 1);
    ::close(report[0]);

    if (received > 0) {
        setError(out->error, sizeof out->error, message);
        // Reap so the failed child does not linger as a zombie.
        int status = 0;
        static_cast<void>(::waitpid(outer, &status, 0));
        return 0;
    }

    out->pid = outer;
    out->tier = caps.tier;
    return 1;
}

int supra_sandbox_wait(const supra_sandbox_process* process, uint32_t timeout_ms, int* out_status) {
    if (process == nullptr || process->pid < 0) {
        return -1;
    }

    const pid_t pid = static_cast<pid_t>(process->pid);

    if (timeout_ms == 0) {
        int status = 0;
        if (::waitpid(pid, &status, 0) < 0) {
            return -1;
        }
        if (out_status != nullptr) {
            *out_status = WIFEXITED(status) ? WEXITSTATUS(status)
                                            : (WIFSIGNALED(status) ? 128 + WTERMSIG(status) : -1);
        }
        return 1;
    }

    // Poll rather than use SIGCHLD: a library must not install a signal handler
    // in a host process it does not own.
    const std::uint32_t interval_ms = 5;
    for (std::uint32_t waited = 0; waited < timeout_ms; waited += interval_ms) {
        int status = 0;
        const pid_t result = ::waitpid(pid, &status, WNOHANG);
        if (result < 0) {
            return -1;
        }
        if (result > 0) {
            if (out_status != nullptr) {
                *out_status = WIFEXITED(status)
                                  ? WEXITSTATUS(status)
                                  : (WIFSIGNALED(status) ? 128 + WTERMSIG(status) : -1);
            }
            return 1;
        }
        ::usleep(interval_ms * 1000);
    }

    return 0;
}

int supra_sandbox_kill(const supra_sandbox_process* process, uint32_t grace_ms) {
    if (process == nullptr || process->pid < 0) {
        return 0;
    }

    // Signal the process group: a sandboxed command routinely spawns children,
    // and killing only the leader leaves them running.
    const pid_t group = -static_cast<pid_t>(process->pid);

    if (::kill(group, SIGTERM) != 0 && errno != ESRCH) {
        return 0;
    }

    int status = 0;
    if (supra_sandbox_wait(process, grace_ms, &status) == 1) {
        return 1;
    }

    static_cast<void>(::kill(group, SIGKILL));
    return supra_sandbox_wait(process, 1000, &status) == 1 ? 1 : 0;
}

}  // extern "C"

#endif  // __linux__
