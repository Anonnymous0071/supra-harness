// Landlock backend, internal interface.
//
// Declared separately so the spawn path can apply a ruleset without knowing the
// syscall details, and so the probe can be exercised on its own.

#ifndef SUPRA_SANDBOX_LANDLOCK_HPP
#define SUPRA_SANDBOX_LANDLOCK_HPP

#ifdef __linux__

#include <cstddef>

#include "supra/sandbox.h"

namespace supra::sandbox::landlock {

/// Landlock ABI version the running kernel reports, or 0 when unavailable.
///
/// A non-zero value proves only that the LSM is compiled in, not that it is
/// enabled in the boot-time LSM list. Use `enforces()` before relying on it.
[[nodiscard]] int abiVersion();

/// Whether Landlock actually denies access on this kernel.
///
/// Applies a real ruleset in a forked child and confirms a denial occurs. Forked
/// because Landlock is irreversible: an in-process check would permanently
/// confine the caller.
[[nodiscard]] bool enforces();

/// Apply `policy` to the calling process. Irreversible.
///
/// Call after `unshare` and immediately before `exec`. Fails closed: any rule the
/// kernel rejects aborts with `error` populated, because a rejected rule is an
/// absent rule and an absent rule widens the sandbox.
///
/// @return 1 on success, 0 on failure.
[[nodiscard]] int apply(const supra_sandbox_policy& policy, int abi, char* error,
                        std::size_t error_cap);

}  // namespace supra::sandbox::landlock

#endif  // __linux__

#endif  // SUPRA_SANDBOX_LANDLOCK_HPP
