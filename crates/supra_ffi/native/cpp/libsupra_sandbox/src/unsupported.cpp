// Backend stubs for platforms whose implementation has not landed.
//
// These refuse rather than silently running the command unconfined. A stub that
// executed anyway would make T16.7's `auto` mode - which permits reversible
// mutations without asking - unsafe on those platforms, and the failure would be
// invisible until something escaped.
//
// macOS: `sandbox_init` with a generated SBPL profile. The API is deprecated but
// still the only unprivileged option, and Chromium and others rely on it.
//
// Windows: AppContainer via `CreateProcessAsUser` with a capability SID, plus a
// job object for the process-tree limit.

#if !defined(__linux__) && !defined(__APPLE__) && !defined(_WIN32)

#include <cstddef>
#include <cstring>

#include "internal.hpp"
#include "supra/sandbox.h"

namespace {

void reportUnsupported(char* dest, std::size_t cap) {
#if defined(__APPLE__)
    supra::sandbox::detail::setError(
        dest, cap, "macOS sandbox backend not implemented (sandbox_init + SBPL profile pending)");
#elif defined(_WIN32)
    supra::sandbox::detail::setError(
        dest, cap, "Windows sandbox backend not implemented (AppContainer + job object pending)");
#else
    supra::sandbox::detail::setError(dest, cap, "no sandbox backend for this platform");
#endif
}

}  // namespace

extern "C" {

void supra_sandbox_probe(supra_sandbox_capabilities* out) {
    if (out == nullptr) {
        return;
    }
    *out = supra_sandbox_capabilities{};
    out->tier = SUPRA_SANDBOX_TIER_NONE;
    reportUnsupported(out->detail, sizeof out->detail);
}

void supra_sandbox_force_tier_for_testing(uint8_t tier) {
    // No backend here, so there is no tier to raise. Accepting the call silently
    // keeps the ABI uniform across platforms.
    static_cast<void>(tier);
}

int supra_sandbox_spawn(const supra_sandbox_policy* policy, const supra_sandbox_command* command,
                        supra_sandbox_process* out) {
    static_cast<void>(policy);
    static_cast<void>(command);
    if (out == nullptr) {
        return 0;
    }
    *out = supra_sandbox_process{};
    out->pid = -1;
    out->tier = SUPRA_SANDBOX_TIER_NONE;
    reportUnsupported(out->error, sizeof out->error);
    return 0;
}

int supra_sandbox_wait(const supra_sandbox_process* process, uint32_t timeout_ms, int* out_status) {
    static_cast<void>(process);
    static_cast<void>(timeout_ms);
    static_cast<void>(out_status);
    return -1;
}

int supra_sandbox_kill(const supra_sandbox_process* process, uint32_t grace_ms) {
    static_cast<void>(process);
    static_cast<void>(grace_ms);
    return 0;
}

void supra_sandbox_release(supra_sandbox_process* process) {
    static_cast<void>(process);
}

}  // extern "C"

#endif  // unsupported platform
