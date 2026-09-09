// Internal helpers shared between the libsupra_sandbox translation units.

#ifndef SUPRA_SANDBOX_INTERNAL_HPP
#define SUPRA_SANDBOX_INTERNAL_HPP

#include <cstddef>
#include <cstdint>

#include "supra/sandbox.h"

namespace supra::sandbox::detail {

/// Copy a message into a fixed error buffer, always NUL-terminating.
///
/// `std::strncpy` does not guarantee termination when the source overflows, and
/// an unterminated error string turns a diagnostic into a crash.
void setError(char* dest, std::size_t cap, const char* message);

/// Format `message: strerror(err)` into a fixed buffer.
void setErrorErrno(char* dest, std::size_t cap, const char* message, int err);

/// Append `tail` to the NUL-terminated string in `dest`, truncating rather
/// than overflowing. `snprintf` with `%s` warns under `-Wformat-truncation`
/// on GCC 12+; the loop states the truncation the warning asks about.
void appendTruncated(char* dest, std::size_t cap, const char* tail);

/// True when `path` is absolute.
///
/// A relative path is rejected at policy-construction time: resolving it would
/// depend on the caller's working directory at an unpredictable moment, and a
/// sandbox whose scope shifts with `chdir` is not a boundary.
[[nodiscard]] bool isAbsolute(const char* path);

/// Validate a policy without applying it.
///
/// @return 1 when usable, 0 otherwise with `error` populated.
[[nodiscard]] int validatePolicy(const supra_sandbox_policy* policy, char* error,
                                 std::size_t error_cap);

}  // namespace supra::sandbox::detail

#endif  // SUPRA_SANDBOX_INTERNAL_HPP
