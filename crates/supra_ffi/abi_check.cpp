// Compile-time confirmation that the C++ structures are the size the Rust side
// believes.
//
// Every value arrives as a -D macro from build.rs, sourced from abi_sizes.rs -
// the same file src/sys.rs asserts against. A mismatch in either direction fails
// to compile, so a hand-written extern declaration cannot silently drift from the
// header it mirrors.
//
// Compiled, never run: a runtime probe would be useless when cross-compiling.

#include "supra/ansi.h"
#include "supra/sandbox.h"
#include "supra/width.h"

#define SUPRA_ABI_ASSERT(type, size_macro, align_macro)                        \
    static_assert(sizeof(type) == (size_macro),                                \
                  "sizeof(" #type ") disagrees with abi_sizes.rs");            \
    static_assert(alignof(type) == (align_macro),                              \
                  "alignof(" #type ") disagrees with abi_sizes.rs")

SUPRA_ABI_ASSERT(supra_ansi_color, SUPRA_ABI_SIZEOF_ANSI_COLOR, SUPRA_ABI_ALIGNOF_ANSI_COLOR);
SUPRA_ABI_ASSERT(supra_ansi_style, SUPRA_ABI_SIZEOF_ANSI_STYLE, SUPRA_ABI_ALIGNOF_ANSI_STYLE);
SUPRA_ABI_ASSERT(supra_ansi_token, SUPRA_ABI_SIZEOF_ANSI_TOKEN, SUPRA_ABI_ALIGNOF_ANSI_TOKEN);
SUPRA_ABI_ASSERT(supra_ansi_scanner, SUPRA_ABI_SIZEOF_ANSI_SCANNER,
                 SUPRA_ABI_ALIGNOF_ANSI_SCANNER);
SUPRA_ABI_ASSERT(supra_ansi_truncation, SUPRA_ABI_SIZEOF_ANSI_TRUNCATION,
                 SUPRA_ABI_ALIGNOF_ANSI_TRUNCATION);

SUPRA_ABI_ASSERT(supra_sandbox_capabilities, SUPRA_ABI_SIZEOF_SANDBOX_CAPABILITIES,
                 SUPRA_ABI_ALIGNOF_SANDBOX_CAPABILITIES);
SUPRA_ABI_ASSERT(supra_sandbox_path_rule, SUPRA_ABI_SIZEOF_SANDBOX_PATH_RULE,
                 SUPRA_ABI_ALIGNOF_SANDBOX_PATH_RULE);
SUPRA_ABI_ASSERT(supra_sandbox_policy, SUPRA_ABI_SIZEOF_SANDBOX_POLICY,
                 SUPRA_ABI_ALIGNOF_SANDBOX_POLICY);
SUPRA_ABI_ASSERT(supra_sandbox_process, SUPRA_ABI_SIZEOF_SANDBOX_PROCESS,
                 SUPRA_ABI_ALIGNOF_SANDBOX_PROCESS);
SUPRA_ABI_ASSERT(supra_sandbox_command, SUPRA_ABI_SIZEOF_SANDBOX_COMMAND,
                 SUPRA_ABI_ALIGNOF_SANDBOX_COMMAND);

// Also pin the limits the Rust side hard-codes into array lengths. A header bump
// without a matching Rust change would otherwise produce arrays of the wrong
// length, which is the same silent-offset failure as a size mismatch.
static_assert(SUPRA_ANSI_MAX_PARAMS == 16, "SUPRA_ANSI_MAX_PARAMS changed");
static_assert(SUPRA_ANSI_MAX_SUBPARAMS == 8, "SUPRA_ANSI_MAX_SUBPARAMS changed");
static_assert(SUPRA_ANSI_MAX_PAYLOAD == 256, "SUPRA_ANSI_MAX_PAYLOAD changed");
static_assert(SUPRA_ANSI_MAX_TRAILER == 24, "SUPRA_ANSI_MAX_TRAILER changed");
static_assert(SUPRA_SANDBOX_MAX_PATHS == 64, "SUPRA_SANDBOX_MAX_PATHS changed");
static_assert(SUPRA_SANDBOX_MAX_PORTS == 16, "SUPRA_SANDBOX_MAX_PORTS changed");
static_assert(SUPRA_SANDBOX_ERROR_LEN == 256, "SUPRA_SANDBOX_ERROR_LEN changed");

// The translation unit needs one symbol so the archive is not empty.
extern "C" int supra_ffi_abi_check(void);
extern "C" int supra_ffi_abi_check(void) {
    return 0;
}
