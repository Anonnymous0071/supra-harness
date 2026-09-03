// Landlock wrapper: filesystem and per-port TCP policy.
//
// Landlock is preferred over mount-based isolation for three measured reasons.
//
// It composes with the ordering supra actually needs. Applying a ruleset and
// then exec'ing bwrap fails with "Failed to make / slave: Operation not
// permitted", and still fails under a ruleset that grants write access
// everywhere - so the cause is bwrap's mount setup, not policy tightness.
//
// It is inode-based, which makes path traversal and chroot irrelevant: `..`
// resolves to an inode outside the allowlist and is refused, and chroot cannot
// widen a policy expressed over inodes.
//
// It is monotonic. A confined process may add further rulesets, but each one can
// only narrow. There is no operation that re-widens, so a compromised child
// cannot escape by installing a permissive policy of its own.
//
// Two failure modes this file is written around:
//
//   * Directory-only access bits are rejected when applied to a regular file.
//     Ignoring the return value therefore *silently widens* the sandbox - the
//     rule simply never exists. Every rule is masked to what the target's file
//     type accepts, and a rejection aborts the spawn.
//
//   * A reported ABI version proves the LSM is compiled in, not that it is
//     enabled in the boot-time LSM list. The probe applies a real ruleset in a
//     forked child and confirms a denial actually occurs.

#ifdef __linux__

#include <cerrno>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>

#include <fcntl.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#include "internal.hpp"
#include "landlock.hpp"
#include "supra/sandbox.h"

// The kernel headers may predate the ABI the running kernel supports, so the
// structures and constants are declared locally rather than included. This keeps
// the build working on an older toolchain while still using newer features when
// the kernel offers them.
namespace {

struct RulesetAttr {
    std::uint64_t handled_access_fs;
    std::uint64_t handled_access_net;
    std::uint64_t scoped;
};

struct PathBeneathAttr {
    std::uint64_t allowed_access;
    std::int32_t parent_fd;
};

struct NetPortAttr {
    std::uint64_t allowed_access;
    std::uint64_t port;
};

constexpr int kRuleTypePathBeneath = 1;
constexpr int kRuleTypeNetPort = 2;
constexpr std::uint32_t kCreateRulesetVersion = 1U << 0;

// Filesystem access bits, ABI 1 through 5.
constexpr std::uint64_t kFsExecute = 1ULL << 0;
constexpr std::uint64_t kFsWriteFile = 1ULL << 1;
constexpr std::uint64_t kFsReadFile = 1ULL << 2;
constexpr std::uint64_t kFsReadDir = 1ULL << 3;
constexpr std::uint64_t kFsRemoveDir = 1ULL << 4;
constexpr std::uint64_t kFsRemoveFile = 1ULL << 5;
constexpr std::uint64_t kFsMakeChar = 1ULL << 6;
constexpr std::uint64_t kFsMakeDir = 1ULL << 7;
constexpr std::uint64_t kFsMakeReg = 1ULL << 8;
constexpr std::uint64_t kFsMakeSock = 1ULL << 9;
constexpr std::uint64_t kFsMakeFifo = 1ULL << 10;
constexpr std::uint64_t kFsMakeBlock = 1ULL << 11;
constexpr std::uint64_t kFsMakeSym = 1ULL << 12;
constexpr std::uint64_t kFsRefer = 1ULL << 13;      // ABI 2
constexpr std::uint64_t kFsTruncate = 1ULL << 14;   // ABI 3
constexpr std::uint64_t kFsIoctlDev = 1ULL << 15;   // ABI 5

// Network access bits, ABI 4.
constexpr std::uint64_t kNetBindTcp = 1ULL << 0;
constexpr std::uint64_t kNetConnectTcp = 1ULL << 1;

// Scoping, ABI 6. Without these, a confined process can still reach the host
// through an abstract unix socket or signal a process outside its own domain.
constexpr std::uint64_t kScopeAbstractUnixSocket = 1ULL << 0;
constexpr std::uint64_t kScopeSignal = 1ULL << 1;

long createRuleset(const RulesetAttr* attr, std::size_t size, std::uint32_t flags) {
    return ::syscall(444 /* landlock_create_ruleset */, attr, size, flags);
}

long addRule(int ruleset_fd, int rule_type, const void* attr, std::uint32_t flags) {
    return ::syscall(445 /* landlock_add_rule */, ruleset_fd, rule_type, attr, flags);
}

long restrictSelf(int ruleset_fd, std::uint32_t flags) {
    return ::syscall(446 /* landlock_restrict_self */, ruleset_fd, flags);
}

/// Filesystem bits available at `abi`.
///
/// Requesting a bit the running kernel does not know makes `create_ruleset` fail
/// outright, so the handled set is trimmed to the ABI rather than assumed.
std::uint64_t handledFsForAbi(int abi) {
    std::uint64_t bits = kFsExecute | kFsWriteFile | kFsReadFile | kFsReadDir | kFsRemoveDir |
                         kFsRemoveFile | kFsMakeChar | kFsMakeDir | kFsMakeReg | kFsMakeSock |
                         kFsMakeFifo | kFsMakeBlock | kFsMakeSym;
    if (abi >= 2) {
        bits |= kFsRefer;
    }
    if (abi >= 3) {
        bits |= kFsTruncate;
    }
    if (abi >= 5) {
        bits |= kFsIoctlDev;
    }
    return bits;
}

/// Bits meaningful for a directory.
std::uint64_t dirBits(std::uint32_t access, int abi) {
    std::uint64_t out = 0;
    if ((access & SUPRA_SANDBOX_READ) != 0) {
        out |= kFsReadFile | kFsReadDir;
    }
    if ((access & SUPRA_SANDBOX_EXECUTE) != 0) {
        out |= kFsExecute;
    }
    if ((access & SUPRA_SANDBOX_WRITE) != 0) {
        out |= kFsWriteFile;
        if (abi >= 3) {
            out |= kFsTruncate;
        }
    }
    if ((access & SUPRA_SANDBOX_MANAGE) != 0) {
        out |= kFsMakeReg | kFsMakeDir | kFsMakeSym | kFsMakeFifo | kFsMakeSock |
               kFsRemoveFile | kFsRemoveDir;
        if (abi >= 2) {
            out |= kFsRefer;
        }
    }
    return out;
}

/// Bits meaningful for a regular file or device node.
///
/// Directory-only bits must be excluded: the kernel refuses the rule outright,
/// and a refused rule is an absent rule - which widens the sandbox rather than
/// narrowing it.
std::uint64_t fileBits(std::uint32_t access, int abi, bool is_device) {
    std::uint64_t out = 0;
    if ((access & SUPRA_SANDBOX_READ) != 0) {
        out |= kFsReadFile;
    }
    if ((access & SUPRA_SANDBOX_EXECUTE) != 0) {
        out |= kFsExecute;
    }
    if ((access & SUPRA_SANDBOX_WRITE) != 0) {
        out |= kFsWriteFile;
        if (abi >= 3) {
            out |= kFsTruncate;
        }
    }
    if (is_device && abi >= 5) {
        out |= kFsIoctlDev;
    }
    return out;
}

}  // namespace

namespace supra::sandbox::landlock {

int abiVersion() {
    const long version = createRuleset(nullptr, 0, kCreateRulesetVersion);
    return version > 0 ? static_cast<int>(version) : 0;
}

bool enforces() {
    const int abi = abiVersion();
    if (abi <= 0) {
        return false;
    }

    // Fork rather than test in-process: Landlock is irreversible, so a
    // self-check would permanently confine the caller. The child confirms a
    // real denial, because a version number does not prove the LSM is enabled in
    // the boot-time LSM list.
    const pid_t pid = ::fork();
    if (pid < 0) {
        return false;
    }

    if (pid == 0) {
        RulesetAttr attr{};
        attr.handled_access_fs = kFsReadFile;

        const int ruleset_fd = static_cast<int>(createRuleset(&attr, sizeof attr, 0));
        if (ruleset_fd < 0) {
            ::_exit(1);
        }

        // Permit reads beneath /usr only; a read elsewhere must then fail.
        const int dir_fd = ::open("/usr", O_PATH | O_CLOEXEC);
        if (dir_fd < 0) {
            ::_exit(1);
        }
        PathBeneathAttr rule{};
        rule.allowed_access = kFsReadFile;
        rule.parent_fd = dir_fd;
        if (addRule(ruleset_fd, kRuleTypePathBeneath, &rule, 0) != 0) {
            ::_exit(1);
        }
        ::close(dir_fd);

        if (::prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0) {
            ::_exit(1);
        }
        if (restrictSelf(ruleset_fd, 0) != 0) {
            ::_exit(1);
        }
        ::close(ruleset_fd);

        // /etc/hostname exists on every Linux system and is outside /usr. If it
        // opens, Landlock is present but inert - which is the failure this probe
        // exists to catch.
        const int probe_fd = ::open("/etc/hostname", O_RDONLY | O_CLOEXEC);
        if (probe_fd >= 0) {
            ::close(probe_fd);
            ::_exit(2);
        }
        ::_exit(0);
    }

    int status = 0;
    if (::waitpid(pid, &status, 0) < 0) {
        return false;
    }
    return WIFEXITED(status) && WEXITSTATUS(status) == 0;
}

int apply(const supra_sandbox_policy& policy, int abi, char* error, std::size_t error_cap) {
    using detail::setError;
    using detail::setErrorErrno;

    RulesetAttr attr{};
    attr.handled_access_fs = handledFsForAbi(abi);

    const bool want_ports = policy.network == SUPRA_SANDBOX_NET_PORTS;
    if (want_ports) {
        if (abi < 4) {
            // Refuse rather than fall back to full access. Silently granting the
            // network because per-port policy is unavailable is exactly the
            // decorative-sandbox failure mode.
            setError(error, error_cap,
                     "policy requests per-port network but Landlock ABI < 4 (needs kernel 6.7+)");
            return 0;
        }
        attr.handled_access_net = kNetBindTcp | kNetConnectTcp;
    } else if (policy.network == SUPRA_SANDBOX_NET_NONE && abi >= 4) {
        // Handle network access while permitting none: belt-and-braces with the
        // network namespace, and it yields EACCES rather than ENETUNREACH, which
        // is a clearer signal to the caller.
        attr.handled_access_net = kNetBindTcp | kNetConnectTcp;
    }

    if (abi >= 6) {
        // Without scoping, a confined process can still reach outside its domain
        // via an abstract unix socket, or signal an unrelated process.
        attr.scoped = kScopeAbstractUnixSocket | kScopeSignal;
    }

    const int ruleset_fd = static_cast<int>(createRuleset(&attr, sizeof attr, 0));
    if (ruleset_fd < 0) {
        setErrorErrno(error, error_cap, "landlock_create_ruleset", errno);
        return 0;
    }

    for (std::size_t i = 0; i < policy.path_count; ++i) {
        const auto& path_rule = policy.paths[i];

        struct ::stat st{};
        if (::stat(path_rule.path, &st) != 0) {
            // A missing path is a policy error, not something to skip. Skipping
            // would leave the caller believing access was granted.
            char message[SUPRA_SANDBOX_ERROR_LEN];
            std::snprintf(message, sizeof message, "stat %s", path_rule.path);
            setErrorErrno(error, error_cap, message, errno);
            ::close(ruleset_fd);
            return 0;
        }

        const bool is_dir = S_ISDIR(st.st_mode);
        const bool is_device = S_ISCHR(st.st_mode) || S_ISBLK(st.st_mode);

        // Mask to what this file type accepts. Directory-only bits on a regular
        // file make the kernel reject the whole rule.
        const std::uint64_t allowed = is_dir ? dirBits(path_rule.access, abi)
                                             : fileBits(path_rule.access, abi, is_device);

        // Unreachable through the public API: supra_sandbox_policy_allow refuses
        // an access mask of 0, and every non-zero mask yields at least one bit
        // for both file and directory targets. Kept because a future access bit
        // that is directory-only would make it reachable, and a rule reducing to
        // nothing must be reported rather than silently added as a no-op.
        if (allowed == 0) {
            char message[SUPRA_SANDBOX_ERROR_LEN];
            std::snprintf(message, sizeof message,
                          "path rule %zu (%s) reduces to no access for this file type", i,
                          path_rule.path);
            setError(error, error_cap, message);
            ::close(ruleset_fd);
            return 0;
        }

        const int dir_fd = ::open(path_rule.path, O_PATH | O_CLOEXEC);
        if (dir_fd < 0) {
            char message[SUPRA_SANDBOX_ERROR_LEN];
            std::snprintf(message, sizeof message, "open %s", path_rule.path);
            setErrorErrno(error, error_cap, message, errno);
            ::close(ruleset_fd);
            return 0;
        }

        PathBeneathAttr beneath{};
        beneath.allowed_access = allowed & attr.handled_access_fs;
        beneath.parent_fd = dir_fd;

        const long rc = addRule(ruleset_fd, kRuleTypePathBeneath, &beneath, 0);
        const int add_errno = errno;
        ::close(dir_fd);

        if (rc != 0) {
            // Never ignored: a rejected rule is an absent rule, and an absent
            // rule widens the sandbox.
            //
            // Also unreachable while masking is correct. Probed directly: with
            // properly masked bits, add_rule returns 0 for directories, regular
            // files, device nodes, /proc, /sys, and for 20 000 consecutive rules -
            // no input was found that fails. It rejects only mismatched bits,
            // which masking prevents.
            //
            // So this is defence in depth against a *future* masking regression,
            // and no single-fault test can reach it. Mutating it in isolation
            // therefore survives, which is a property of the code rather than a
            // gap in the suite; the masking itself is covered by
            // testAccessCombinationsSurviveMasking.
            char message[SUPRA_SANDBOX_ERROR_LEN];
            std::snprintf(message, sizeof message, "landlock_add_rule for %s", path_rule.path);
            setErrorErrno(error, error_cap, message, add_errno);
            ::close(ruleset_fd);
            return 0;
        }
    }

    if (want_ports) {
        for (std::size_t i = 0; i < policy.port_count; ++i) {
            NetPortAttr port_rule{};
            port_rule.allowed_access = kNetConnectTcp;
            port_rule.port = policy.allowed_ports[i];

            if (addRule(ruleset_fd, kRuleTypeNetPort, &port_rule, 0) != 0) {
                char message[SUPRA_SANDBOX_ERROR_LEN];
                std::snprintf(message, sizeof message, "landlock_add_rule for TCP port %u",
                              static_cast<unsigned>(policy.allowed_ports[i]));
                setErrorErrno(error, error_cap, message, errno);
                ::close(ruleset_fd);
                return 0;
            }
        }
    }

    // Landlock requires no_new_privs, which also stops a setuid binary from
    // regaining privilege inside the sandbox.
    if (::prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0) {
        setErrorErrno(error, error_cap, "prctl(PR_SET_NO_NEW_PRIVS)", errno);
        ::close(ruleset_fd);
        return 0;
    }

    if (restrictSelf(ruleset_fd, 0) != 0) {
        setErrorErrno(error, error_cap, "landlock_restrict_self", errno);
        ::close(ruleset_fd);
        return 0;
    }

    ::close(ruleset_fd);
    return 1;
}

}  // namespace supra::sandbox::landlock

#endif  // __linux__
