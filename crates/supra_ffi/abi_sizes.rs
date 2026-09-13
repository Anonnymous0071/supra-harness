// Sizes and alignments of the C ABI structures, in bytes.
//
// Included by BOTH build.rs and src/sys.rs, so there is exactly one place these
// numbers live.
//
// This closes a bug class that is otherwise silent and catastrophic. The extern
// declarations in src/sys.rs are hand-written rather than generated, so a field
// added to a C++ header without a matching Rust change produces a layout
// mismatch: the Rust side reads the wrong offsets, and nothing complains. There
// is no crash to debug, just wrong answers - a token whose payload comes from the
// middle of a parameter array, or a policy whose path_count lands in a pointer.
//
// The ratchet works in both directions at compile time:
//
//   * build.rs passes each value to abi_check.cpp as a -D macro, where a
//     static_assert compares it with the real C++ sizeof. Wrong value, no build.
//   * src/sys.rs asserts the same constants against Rust's size_of and
//     align_of. Wrong value, no build.
//
// So a divergence cannot compile, and no test needs to run to catch it. Both
// checks are compile-time, which also keeps them valid when cross-compiling -
// running a probe binary would not.
//
// Measured on x86-64 Linux with clang 19. A platform where these differ will
// fail loudly at build time, which is the correct outcome: it means the layout
// assumptions need revisiting rather than silently working by luck.

/// Size of `supra_ansi_color` in bytes.
pub const SIZEOF_ANSI_COLOR: usize = 5;
/// Alignment of `supra_ansi_color` in bytes.
pub const ALIGNOF_ANSI_COLOR: usize = 1;

/// Size of `supra_ansi_style` in bytes.
pub const SIZEOF_ANSI_STYLE: usize = 24;
/// Alignment of `supra_ansi_style` in bytes.
pub const ALIGNOF_ANSI_STYLE: usize = 4;

/// Size of `supra_ansi_token` in bytes.
pub const SIZEOF_ANSI_TOKEN: usize = 432;
/// Alignment of `supra_ansi_token` in bytes.
pub const ALIGNOF_ANSI_TOKEN: usize = 8;

/// Size of `supra_ansi_scanner` in bytes.
pub const SIZEOF_ANSI_SCANNER: usize = 432;
/// Alignment of `supra_ansi_scanner` in bytes.
pub const ALIGNOF_ANSI_SCANNER: usize = 8;

/// Size of `supra_ansi_truncation` in bytes.
pub const SIZEOF_ANSI_TRUNCATION: usize = 72;
/// Alignment of `supra_ansi_truncation` in bytes.
pub const ALIGNOF_ANSI_TRUNCATION: usize = 8;

/// Size of `supra_sandbox_capabilities` in bytes.
pub const SIZEOF_SANDBOX_CAPABILITIES: usize = 262;
/// Alignment of `supra_sandbox_capabilities` in bytes.
pub const ALIGNOF_SANDBOX_CAPABILITIES: usize = 1;

/// Size of `supra_sandbox_path_rule` in bytes.
pub const SIZEOF_SANDBOX_PATH_RULE: usize = 16;
/// Alignment of `supra_sandbox_path_rule` in bytes.
pub const ALIGNOF_SANDBOX_PATH_RULE: usize = 8;

/// Size of `supra_sandbox_policy` in bytes.
pub const SIZEOF_SANDBOX_POLICY: usize = 1104;
/// Alignment of `supra_sandbox_policy` in bytes.
pub const ALIGNOF_SANDBOX_POLICY: usize = 8;

/// Size of `supra_sandbox_process` in bytes.
pub const SIZEOF_SANDBOX_PROCESS: usize = 288;
/// Alignment of `supra_sandbox_process` in bytes.
pub const ALIGNOF_SANDBOX_PROCESS: usize = 8;

/// Size of `supra_sandbox_command` in bytes.
pub const SIZEOF_SANDBOX_COMMAND: usize = 48;
/// Alignment of `supra_sandbox_command` in bytes.
pub const ALIGNOF_SANDBOX_COMMAND: usize = 8;
