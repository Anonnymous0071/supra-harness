// Internal helpers shared between the libsupra_ansi translation units.
//
// Not part of the public ABI.

#ifndef SUPRA_ANSI_INTERNAL_HPP
#define SUPRA_ANSI_INTERNAL_HPP

#include <cstddef>
#include <cstdint>

#include "supra/ansi.h"

namespace supra::ansi::detail {

/// Parser states, following the DEC STD 070 / VT500 structure.
///
/// The shape is dictated by the grammar, not chosen: OSC, DCS, and APC strings
/// all end at either `ESC \` or `BEL`, so each needs its own "saw ESC inside a
/// string" state to tell a terminator from an escape in the payload.
enum class State : std::uint8_t {
    Ground = 0,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    OscString,
    OscEsc,
    DcsEntry,
    DcsParam,
    DcsIntermediate,
    DcsPassthrough,
    DcsEsc,
    DcsIgnore,
    ApcString,
    ApcEsc,
};

// C0 controls that introduce or terminate sequences.
constexpr std::uint8_t kEsc = 0x1B;
constexpr std::uint8_t kBel = 0x07;
constexpr std::uint8_t kCan = 0x18;
constexpr std::uint8_t kSub = 0x1A;

// 8-bit C1 controls. A regex-based matcher misses these entirely: they carry no
// ESC byte, so `\x1b\[` never fires even though 0x9B *is* CSI.
constexpr std::uint8_t kC1Dcs = 0x90;
constexpr std::uint8_t kC1Sos = 0x98;
constexpr std::uint8_t kC1Csi = 0x9B;
constexpr std::uint8_t kC1St = 0x9C;
constexpr std::uint8_t kC1Osc = 0x9D;
constexpr std::uint8_t kC1Pm = 0x9E;
constexpr std::uint8_t kC1Apc = 0x9F;

/// Parameter value reported when the sender omitted it.
///
/// Distinct from an explicit 0, and the difference is semantic: `ESC [ m` is a
/// full reset while `ESC [ 0 m` happens to mean the same, but `ESC [ ; 5 m` and
/// `ESC [ 0 ; 5 m` do not agree on every terminal.
constexpr std::int32_t kParamOmitted = -1;

/// Cap on accumulated parameter values. Prevents overflow on adversarial input
/// like `ESC [ 99999999999999999999 m` without rejecting the sequence.
constexpr std::int32_t kParamMax = 65535;

[[nodiscard]] constexpr bool isC1(std::uint8_t byte) noexcept {
    return byte >= 0x80 && byte <= 0x9F;
}

/// True for a UTF-8 continuation byte.
///
/// This range (0x80..0xBF) **fully contains** the 8-bit C1 control range
/// (0x80..0x9F), and that overlap is a trap. U+6587 encodes as `E6 96 87`, whose
/// second byte 0x96 is the C1 code for START OF GUARDED AREA; U+1F600 encodes as
/// `F0 9F 98 80`, containing three bytes in the C1 range. A scanner that tests
/// raw bytes for C1 membership tears those characters apart.
///
/// The disambiguation is positional rather than value-based: no byte in
/// 0x80..0xBF is a valid UTF-8 *lead*, so such a byte is a C1 control exactly
/// when it appears at a scalar boundary. That is why the Ground-state text scan
/// advances by decoded scalars rather than by bytes.
[[nodiscard]] constexpr bool isContinuation(std::uint8_t byte) noexcept {
    return (byte & 0xC0) == 0x80;
}

[[nodiscard]] constexpr bool isParamByte(std::uint8_t byte) noexcept {
    return byte >= 0x30 && byte <= 0x3F;
}

[[nodiscard]] constexpr bool isIntermediateByte(std::uint8_t byte) noexcept {
    return byte >= 0x20 && byte <= 0x2F;
}

[[nodiscard]] constexpr bool isFinalByte(std::uint8_t byte) noexcept {
    return byte >= 0x40 && byte <= 0x7E;
}

/// Bytes that abort any sequence in progress, per DEC STD 070. CAN and SUB
/// cancel; both are treated as a cancellation rather than payload.
[[nodiscard]] constexpr bool isCancel(std::uint8_t byte) noexcept {
    return byte == kCan || byte == kSub;
}

/// A C0 control that is content rather than a command.
///
/// Tab and newline are layout, and stripping them would corrupt the text.
/// Everything else in C0 can move the cursor or reprogram the terminal, so it is
/// dropped when stripping untrusted output.
[[nodiscard]] constexpr bool isTextControl(std::uint8_t byte) noexcept {
    return byte == 0x09 || byte == 0x0A;
}

}  // namespace supra::ansi::detail

#endif  // SUPRA_ANSI_INTERNAL_HPP
