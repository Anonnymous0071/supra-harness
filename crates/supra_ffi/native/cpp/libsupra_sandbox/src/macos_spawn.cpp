// macOS backend: sandbox_init/SBPL filesystem and network confinement plus
// process-group ownership for reliable tree cleanup.

#if defined(__APPLE__)

#include <atomic>
#include <cerrno>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <mutex>

#include <fcntl.h>
#include <sandbox.h>
#include <signal.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#include "internal.hpp"
#include "supra/sandbox.h"

namespace {

using supra::sandbox::detail::appendTruncated;
using supra::sandbox::detail::setError;
using supra::sandbox::detail::setErrorErrno;

constexpr std::size_t kProfileCapacity = 131072;
constexpr std::uint32_t kWaitIntervalMs = 5;

supra_sandbox_capabilities g_caps{};
std::once_flag g_caps_once;
std::atomic<std::uint8_t> g_forced_tier{SUPRA_SANDBOX_TIER_UNSET};

struct ProfileBuffer {
    char bytes[kProfileCapacity]{};
    std::size_t used = 0;

    bool append(const char* text) {
        if (text == nullptr) {
            return false;
        }
        const std::size_t length = std::strlen(text);
        if (length >= sizeof bytes - used) {
            return false;
        }
        std::memcpy(bytes + used, text, length);
        used += length;
        bytes[used] = '\0';
        return true;
    }
};

bool appendLiteral(ProfileBuffer& profile, const char* path) {
    if (!profile.append("(literal \"") || path == nullptr) {
        return false;
    }
    for (const unsigned char* cursor = reinterpret_cast<const unsigned char*>(path);
         *cursor != 0; ++cursor) {
        char escaped[5]{};
        switch (*cursor) {
            case '\\':
                if (!profile.append("\\\\")) {
                    return false;
                }
                break;
            case '"':
                if (!profile.append("\\\"")) {
                    return false;
                }
                break;
            case '\n':
                if (!profile.append("\\n")) {
                    return false;
                }
                break;
            case '\r':
                if (!profile.append("\\r")) {
                    return false;
                }
                break;
            case '\t':
                if (!profile.append("\\t")) {
                    return false;
                }
                break;
            default:
                if (*cursor < 0x20U || *cursor == 0x7fU) {
                    std::snprintf(escaped, sizeof escaped, "\\%03o",
                                  static_cast<unsigned>(*cursor));
                    if (!profile.append(escaped)) {
                        return false;
                    }
                } else {
                    escaped[0] = static_cast<char>(*cursor);
                    escaped[1] = '\0';
                    if (!profile.append(escaped)) {
                        return false;
                    }
                }
                break;
        }
    }
    return profile.append("\")") ;
}

bool appendSubpath(ProfileBuffer& profile, const char* path) {
    if (!profile.append("(subpath \"") || path == nullptr) {
        return false;
    }
    for (const unsigned char* cursor = reinterpret_cast<const unsigned char*>(path);
         *cursor != 0; ++cursor) {
        char escaped[5]{};
        if (*cursor == '\\') {
            if (!profile.append("\\\\")) {
                return false;
            }
        } else if (*cursor == '"') {
            if (!profile.append("\\\"")) {
                return false;
            }
        } else if (*cursor < 0x20U || *cursor == 0x7fU) {
            std::snprintf(escaped, sizeof escaped, "\\%03o", static_cast<unsigned>(*cursor));
            if (!profile.append(escaped)) {
                return false;
            }
        } else {
            escaped[0] = static_cast<char>(*cursor);
            escaped[1] = '\0';
            if (!profile.append(escaped)) {
                return false;
            }
        }
    }
    return profile.append("\")") ;
}

bool appendPathFilter(ProfileBuffer& profile, const char* path, bool directory) {
    return appendLiteral(profile, path) &&
           (!directory || (profile.append(" ") && appendSubpath(profile, path)));
}

bool appendRule(ProfileBuffer& profile, const supra_sandbox_path_rule& rule,
                char* error, std::size_t error_cap) {
    constexpr std::uint32_t kKnownAccess = SUPRA_SANDBOX_READ | SUPRA_SANDBOX_WRITE |
                                            SUPRA_SANDBOX_EXECUTE | SUPRA_SANDBOX_MANAGE;
    if ((rule.access & ~kKnownAccess) != 0U) {
        setError(error, error_cap, "path rule contains unknown access bits");
        return false;
    }

    struct ::stat status {};
    if (::stat(rule.path, &status) != 0) {
        setErrorErrno(error, error_cap, rule.path, errno);
        return false;
    }
    const bool directory = S_ISDIR(status.st_mode);

    if ((rule.access & (SUPRA_SANDBOX_READ | SUPRA_SANDBOX_EXECUTE)) != 0U) {
        if (!profile.append("(allow file-read* ") ||
            !appendPathFilter(profile, rule.path, directory) || !profile.append(")\n")) {
            setError(error, error_cap, "generated SBPL profile exceeds capacity");
            return false;
        }
    }
    if ((rule.access & (SUPRA_SANDBOX_WRITE | SUPRA_SANDBOX_MANAGE)) != 0U) {
        if (!profile.append("(allow file-write* ") ||
            !appendPathFilter(profile, rule.path, directory) || !profile.append(")\n")) {
            setError(error, error_cap, "generated SBPL profile exceeds capacity");
            return false;
        }
    }
    if ((rule.access & SUPRA_SANDBOX_EXECUTE) != 0U) {
        if (!profile.append("(allow process-exec ") ||
            !appendPathFilter(profile, rule.path, directory) || !profile.append(")\n")) {
            setError(error, error_cap, "generated SBPL profile exceeds capacity");
            return false;
        }
    }
    return true;
}

bool buildProfile(const supra_sandbox_policy& policy, ProfileBuffer& profile,
                  char* error, std::size_t error_cap) {
    if (policy.network > SUPRA_SANDBOX_NET_FULL) {
        setError(error, error_cap, "unknown network policy");
        return false;
    }
    if (policy.network == SUPRA_SANDBOX_NET_PORTS) {
        setError(error, error_cap, "macOS SBPL backend cannot enforce per-port network policy");
        return false;
    }
    if (policy.isolate_processes != 0U || policy.isolate_ipc != 0U) {
        setError(error, error_cap,
                 "macOS SBPL backend cannot enforce process or IPC namespace isolation");
        return false;
    }

    if (!profile.append("(version 1)\n(deny default)\n"
                        "(allow process-fork)\n"
                        "(allow sysctl-read)\n")) {
        setError(error, error_cap, "generated SBPL profile exceeds capacity");
        return false;
    }
    if (policy.network == SUPRA_SANDBOX_NET_FULL && !profile.append("(allow network*)\n")) {
        setError(error, error_cap, "generated SBPL profile exceeds capacity");
        return false;
    }
    for (std::size_t index = 0; index < policy.path_count; ++index) {
        if (!appendRule(profile, policy.paths[index], error, error_cap)) {
            return false;
        }
    }
    return true;
}

bool makeReportPipe(int (&descriptors)[2]) {
    if (::pipe(descriptors) != 0) {
        return false;
    }
    const int flags = ::fcntl(descriptors[1], F_GETFD);
    if (flags < 0 || ::fcntl(descriptors[1], F_SETFD, flags | FD_CLOEXEC) != 0) {
        const int saved = errno;
        ::close(descriptors[0]);
        ::close(descriptors[1]);
        descriptors[0] = -1;
        descriptors[1] = -1;
        errno = saved;
        return false;
    }
    return true;
}

[[noreturn]] void failChild(int report_fd, const char* step, int error_number) {
    char message[SUPRA_SANDBOX_ERROR_LEN];
    if (error_number != 0) {
        setErrorErrno(message, sizeof message, step, error_number);
    } else {
        setError(message, sizeof message, step);
    }
    const ssize_t ignored = ::write(report_fd, message, std::strlen(message));
    static_cast<void>(ignored);
    ::_exit(127);
}

bool redirectDescriptor(int source, int destination) {
    if (source >= 0) {
        return source == destination || ::dup2(source, destination) >= 0;
    }
    const int flags = destination == STDIN_FILENO ? O_RDONLY : O_WRONLY;
    const int null_fd = ::open("/dev/null", flags);
    if (null_fd < 0) {
        return false;
    }
    const bool redirected = null_fd == destination || ::dup2(null_fd, destination) >= 0;
    if (null_fd != destination) {
        ::close(null_fd);
    }
    return redirected;
}

bool applyRlimits(const supra_sandbox_policy& policy, char* error, std::size_t error_cap) {
    if (policy.max_processes > 0U) {
        const ::rlimit limit{policy.max_processes, policy.max_processes};
        if (::setrlimit(RLIMIT_NPROC, &limit) != 0) {
            setErrorErrno(error, error_cap, "setrlimit processes", errno);
            return false;
        }
    }
    if (policy.max_address_space > 0U) {
        const ::rlimit limit{policy.max_address_space, policy.max_address_space};
        if (::setrlimit(RLIMIT_AS, &limit) != 0) {
            setErrorErrno(error, error_cap, "setrlimit address space", errno);
            return false;
        }
    }
    if (policy.max_cpu_seconds > 0U) {
        const ::rlimit limit{policy.max_cpu_seconds, policy.max_cpu_seconds};
        if (::setrlimit(RLIMIT_CPU, &limit) != 0) {
            setErrorErrno(error, error_cap, "setrlimit CPU", errno);
            return false;
        }
    }
    if (policy.max_file_size > 0U) {
        const ::rlimit limit{policy.max_file_size, policy.max_file_size};
        if (::setrlimit(RLIMIT_FSIZE, &limit) != 0) {
            setErrorErrno(error, error_cap, "setrlimit file size", errno);
            return false;
        }
    }
    return true;
}

bool sandboxActuallyEnforces() {
    const pid_t child = ::fork();
    if (child < 0) {
        return false;
    }
    if (child == 0) {
        char* sandbox_error = nullptr;
        constexpr const char* kProbe =
            "(version 1)\n(deny default)\n(allow process*)\n"
            "(allow file-read* (literal \"/dev/null\"))\n";
        if (::sandbox_init(kProbe, 0, &sandbox_error) != 0) {
            if (sandbox_error != nullptr) {
                ::sandbox_free_error(sandbox_error);
            }
            ::_exit(2);
        }
        const int denied = ::open("/etc/passwd", O_RDONLY);
        if (denied >= 0) {
            ::close(denied);
            ::_exit(3);
        }
        const int allowed = ::open("/dev/null", O_RDONLY);
        if (allowed < 0) {
            ::_exit(4);
        }
        ::close(allowed);
        ::_exit(0);
    }
    int status = 0;
    return ::waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
}

void applyForcedTier(supra_sandbox_capabilities& caps) {
    const std::uint8_t forced_tier = g_forced_tier.load(std::memory_order_relaxed);
    if (forced_tier == SUPRA_SANDBOX_TIER_UNSET) {
        return;
    }
    caps.tier = forced_tier;
    if (caps.tier != SUPRA_SANDBOX_TIER_SBPL) {
        caps.network_isolation = 0;
        setError(caps.detail, sizeof caps.detail,
                 "tier forced below SBPL for testing; policy enforcement unavailable");
    }
}

int decodeStatus(int status) {
    if (WIFEXITED(status)) {
        return WEXITSTATUS(status);
    }
    if (WIFSIGNALED(status)) {
        return 128 + WTERMSIG(status);
    }
    return -1;
}

bool terminateRemainingGroup(pid_t child) {
    return ::kill(-child, SIGKILL) == 0 || errno == ESRCH;
}

}  // namespace

extern "C" {

void supra_sandbox_probe(supra_sandbox_capabilities* out) {
    if (out == nullptr) {
        return;
    }
    std::call_once(g_caps_once, [] {
        supra_sandbox_capabilities caps{};
        caps.port_granular_network = 0;
        if (sandboxActuallyEnforces()) {
            caps.tier = SUPRA_SANDBOX_TIER_SBPL;
            caps.network_isolation = 1;
        } else {
            caps.tier = SUPRA_SANDBOX_TIER_NONE;
            setError(caps.detail, sizeof caps.detail,
                     "sandbox_init is unavailable or failed a behavioral denial probe");
        }
        g_caps = caps;
    });
    *out = g_caps;
    applyForcedTier(*out);
}

void supra_sandbox_force_tier_for_testing(std::uint8_t tier) {
    g_forced_tier.store(tier, std::memory_order_relaxed);
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
    if (caps.tier != SUPRA_SANDBOX_TIER_SBPL) {
        setError(out->error, sizeof out->error, caps.detail);
        return 0;
    }
    if (policy->required_tier != 0U && caps.tier < policy->required_tier) {
        setError(out->error, sizeof out->error, "policy requires tier above available: ");
        appendTruncated(out->error, sizeof out->error, caps.detail);
        return 0;
    }
    if (policy->network == SUPRA_SANDBOX_NET_PORTS) {
        setError(out->error, sizeof out->error,
                 "policy requests per-port network but macOS SBPL cannot enforce it");
        return 0;
    }

    ProfileBuffer profile{};
    if (!buildProfile(*policy, profile, out->error, sizeof out->error)) {
        return 0;
    }

    int report[2] = {-1, -1};
    if (!makeReportPipe(report)) {
        setErrorErrno(out->error, sizeof out->error, "pipe", errno);
        return 0;
    }

    const pid_t child = ::fork();
    if (child < 0) {
        setErrorErrno(out->error, sizeof out->error, "fork", errno);
        ::close(report[0]);
        ::close(report[1]);
        return 0;
    }
    if (child == 0) {
        ::close(report[0]);
        if (::setsid() < 0) {
            failChild(report[1], "setsid", errno);
        }
        if (!redirectDescriptor(command->stdin_fd, STDIN_FILENO)) {
            failChild(report[1], "redirect stdin", errno);
        }
        if (!redirectDescriptor(command->stdout_fd, STDOUT_FILENO)) {
            failChild(report[1], "redirect stdout", errno);
        }
        if (!redirectDescriptor(command->stderr_fd, STDERR_FILENO)) {
            failChild(report[1], "redirect stderr", errno);
        }
        if (command->working_dir != nullptr && ::chdir(command->working_dir) != 0) {
            failChild(report[1], "chdir", errno);
        }
        char rlimit_error[SUPRA_SANDBOX_ERROR_LEN]{};
        if (!applyRlimits(*policy, rlimit_error, sizeof rlimit_error)) {
            const ssize_t ignored = ::write(report[1], rlimit_error, std::strlen(rlimit_error));
            static_cast<void>(ignored);
            ::_exit(127);
        }

        char* sandbox_error = nullptr;
        if (::sandbox_init(profile.bytes, 0, &sandbox_error) != 0) {
            if (sandbox_error != nullptr) {
                char message[SUPRA_SANDBOX_ERROR_LEN];
                setError(message, sizeof message, "sandbox_init: ");
                appendTruncated(message, sizeof message, sandbox_error);
                ::sandbox_free_error(sandbox_error);
                failChild(report[1], message, 0);
            }
            failChild(report[1], "sandbox_init", errno);
        }

        static const char* const kEmptyEnvironment[] = {nullptr};
        const char* const* environment =
            command->envp != nullptr ? command->envp : kEmptyEnvironment;
        ::execve(command->program, const_cast<char* const*>(command->argv),
                 const_cast<char* const*>(environment));
        failChild(report[1], "execve", errno);
    }

    ::close(report[1]);
    char message[SUPRA_SANDBOX_ERROR_LEN]{};
    ssize_t received = -1;
    do {
        received = ::read(report[0], message, sizeof message - 1U);
    } while (received < 0 && errno == EINTR);
    ::close(report[0]);

    if (received != 0) {
        if (received > 0) {
            setError(out->error, sizeof out->error, message);
        } else {
            setErrorErrno(out->error, sizeof out->error, "read setup report", errno);
        }
        static_cast<void>(::kill(-child, SIGKILL));
        int status = 0;
        static_cast<void>(::waitpid(child, &status, 0));
        return 0;
    }

    out->pid = child;
    out->tier = caps.tier;
    return 1;
}

int supra_sandbox_wait(const supra_sandbox_process* process, std::uint32_t timeout_ms,
                       int* out_status) {
    if (process == nullptr || process->pid < 0) {
        return -1;
    }
    const pid_t child = static_cast<pid_t>(process->pid);
    if (timeout_ms == 0U) {
        int status = 0;
        pid_t result = -1;
        do {
            result = ::waitpid(child, &status, 0);
        } while (result < 0 && errno == EINTR);
        if (result != child) {
            return -1;
        }
        if (!terminateRemainingGroup(child)) {
            return -1;
        }
        if (out_status != nullptr) {
            *out_status = decodeStatus(status);
        }
        return 1;
    }

    std::uint32_t waited = 0;
    while (waited < timeout_ms) {
        int status = 0;
        const pid_t result = ::waitpid(child, &status, WNOHANG);
        if (result == child) {
            if (!terminateRemainingGroup(child)) {
                return -1;
            }
            if (out_status != nullptr) {
                *out_status = decodeStatus(status);
            }
            return 1;
        }
        if (result < 0 && errno != EINTR) {
            return -1;
        }
        const std::uint32_t remaining = timeout_ms - waited;
        const std::uint32_t delay = remaining < kWaitIntervalMs ? remaining : kWaitIntervalMs;
        ::usleep(delay * 1000U);
        waited += delay;
    }
    return 0;
}

int supra_sandbox_kill(const supra_sandbox_process* process, std::uint32_t grace_ms) {
    if (process == nullptr || process->pid < 0) {
        return 0;
    }
    const pid_t group = -static_cast<pid_t>(process->pid);
    if (::kill(group, SIGTERM) != 0 && errno != ESRCH) {
        return 0;
    }
    int status = 0;
    if (supra_sandbox_wait(process, grace_ms == 0U ? 1U : grace_ms, &status) == 1) {
        return 1;
    }
    if (::kill(group, SIGKILL) != 0 && errno != ESRCH) {
        return 0;
    }
    return supra_sandbox_wait(process, 1000U, &status) == 1 ? 1 : 0;
}

void supra_sandbox_release(supra_sandbox_process* process) {
    if (process != nullptr) {
        process->native_process = 0U;
        process->native_job = 0U;
    }
}

}  // extern "C"

#endif  // __APPLE__