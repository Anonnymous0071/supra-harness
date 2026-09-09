// Policy construction and validation, shared by every backend.

#include <cstddef>
#include <cstdio>
#include <cstring>

#include "internal.hpp"
#include "supra/sandbox.h"

namespace supra::sandbox::detail {

void setError(char* dest, std::size_t cap, const char* message) {
    if (dest == nullptr || cap == 0) {
        return;
    }
    if (message == nullptr) {
        dest[0] = '\0';
        return;
    }
    std::size_t i = 0;
    while (i + 1 < cap && message[i] != '\0') {
        dest[i] = message[i];
        ++i;
    }
    dest[i] = '\0';
}

void appendTruncated(char* dest, std::size_t cap, const char* tail) {
    if (dest == nullptr || cap == 0 || tail == nullptr) {
        return;
    }
    std::size_t end = 0;
    while (end < cap && dest[end] != '\0') {
        ++end;
    }
    if (end + 1 >= cap) {
        return;
    }
    std::size_t i = 0;
    while (end + i + 1 < cap && tail[i] != '\0') {
        dest[end + i] = tail[i];
        ++i;
    }
    dest[end + i] = '\0';
}

void setErrorErrno(char* dest, std::size_t cap, const char* message, int err) {
    if (dest == nullptr || cap == 0) {
        return;
    }
    // `strerror_s` on MSVC, `strerror_r` elsewhere: the reentrant spelling
    // is three incompatible signatures (POSIX returns `int`, GNU returns
    // `char*`, MSVC names it differently and returns `errno_t`). Each branch
    // fills `buffer` on its own path, because a shared initialiser is a dead
    // store under the GNU signature. The message is copied into the caller's
    // buffer before return, so nothing shared outlives the call.
    char buffer[128];
#if defined(_MSC_VER)
    const char* reason =
        (::strerror_s(buffer, sizeof buffer, err) == 0) ? buffer : "unknown error";
#elif defined(__GLIBC__) && defined(_GNU_SOURCE)
    const char* reason = ::strerror_r(err, buffer, sizeof buffer);
#else
    const char* reason =
        (::strerror_r(err, buffer, sizeof buffer) == 0) ? buffer : "unknown error";
#endif
    setError(dest, cap, message != nullptr ? message : "error");
    appendTruncated(dest, cap, ": ");
    appendTruncated(dest, cap, reason);
}

bool isAbsolute(const char* path) {
    return path != nullptr && path[0] == '/';
}

int validatePolicy(const supra_sandbox_policy* policy, char* error, std::size_t error_cap) {
    if (policy == nullptr) {
        setError(error, error_cap, "policy is null");
        return 0;
    }

    if (policy->path_count > SUPRA_SANDBOX_MAX_PATHS) {
        setError(error, error_cap, "path_count exceeds SUPRA_SANDBOX_MAX_PATHS");
        return 0;
    }
    if (policy->port_count > SUPRA_SANDBOX_MAX_PORTS) {
        setError(error, error_cap, "port_count exceeds SUPRA_SANDBOX_MAX_PORTS");
        return 0;
    }

    for (std::size_t i = 0; i < policy->path_count; ++i) {
        const auto& rule = policy->paths[i];
        if (!isAbsolute(rule.path)) {
            char message[SUPRA_SANDBOX_ERROR_LEN];
            std::snprintf(message, sizeof message, "path rule %zu is not absolute: %s", i,
                          rule.path != nullptr ? rule.path : "(null)");
            setError(error, error_cap, message);
            return 0;
        }
        if (rule.access == 0) {
            char message[SUPRA_SANDBOX_ERROR_LEN];
            std::snprintf(message, sizeof message, "path rule %zu grants no access: %s", i,
                          rule.path);
            setError(error, error_cap, message);
            return 0;
        }
    }

    if (policy->network == SUPRA_SANDBOX_NET_PORTS && policy->port_count == 0) {
        setError(error, error_cap, "NET_PORTS selected but no ports allowed");
        return 0;
    }

    if (policy->required_tier > SUPRA_SANDBOX_TIER_APPCONTAINER) {
        setError(error, error_cap, "required_tier is not a known tier");
        return 0;
    }

    return 1;
}

}  // namespace supra::sandbox::detail

extern "C" {

void supra_sandbox_policy_init(supra_sandbox_policy* policy) {
    if (policy == nullptr) {
        return;
    }
    // Zeroing is the most restrictive state by construction: no paths, no
    // network, no rlimits, and required_tier 0 meaning "accept what is
    // available". Restrictive-by-default matters because a caller that forgets
    // a field gets less access, not more.
    std::memset(policy, 0, sizeof *policy);
    policy->network = SUPRA_SANDBOX_NET_NONE;
}

int supra_sandbox_policy_allow(supra_sandbox_policy* policy, const char* path, uint32_t access) {
    if (policy == nullptr || access == 0) {
        return 0;
    }
    if (!supra::sandbox::detail::isAbsolute(path)) {
        return 0;
    }
    if (policy->path_count >= SUPRA_SANDBOX_MAX_PATHS) {
        return 0;
    }

    policy->paths[policy->path_count].path = path;
    // MANAGE without WRITE is meaningless: creating a file is a write. Normalise
    // here so backends do not each have to infer it.
    policy->paths[policy->path_count].access =
        (access & SUPRA_SANDBOX_MANAGE) != 0 ? access | SUPRA_SANDBOX_WRITE : access;
    ++policy->path_count;
    return 1;
}

int supra_sandbox_policy_allow_port(supra_sandbox_policy* policy, uint16_t port) {
    if (policy == nullptr) {
        return 0;
    }
    if (policy->port_count >= SUPRA_SANDBOX_MAX_PORTS) {
        return 0;
    }

    // Idempotent: adding the same port twice should not consume two slots.
    for (std::size_t i = 0; i < policy->port_count; ++i) {
        if (policy->allowed_ports[i] == port) {
            policy->network = SUPRA_SANDBOX_NET_PORTS;
            return 1;
        }
    }

    policy->allowed_ports[policy->port_count] = port;
    ++policy->port_count;
    policy->network = SUPRA_SANDBOX_NET_PORTS;
    return 1;
}

}  // extern "C"
