// Internal helpers shared between the libsupra_width translation units.
//
// Not part of the public ABI. Everything here is header-inline: these are the
// hottest calls in the render path, invoked per code point per frame.

#ifndef SUPRA_WIDTH_INTERNAL_HPP
#define SUPRA_WIDTH_INTERNAL_HPP

#include <cstddef>
#include <cstdint>

#include "supra/width.h"
#include "tables.hpp"

namespace supra::width::detail {

using tables::contains;

constexpr std::uint32_t kReplacement = SUPRA_WIDTH_REPLACEMENT;
constexpr std::uint32_t kMaxCodePoint = 0x10FFFF;
constexpr std::uint32_t kSurrogateLo = 0xD800;
constexpr std::uint32_t kSurrogateHi = 0xDFFF;
constexpr std::uint32_t kZwj = 0x200D;
constexpr std::uint32_t kVs15 = 0xFE0E;  // text presentation
constexpr std::uint32_t kVs16 = 0xFE0F;  // emoji presentation

/// Decode one UTF-8 scalar. Shared by the public entry point and the internal
/// scanners so there is exactly one definition of "malformed".
///
/// Guarantees `*consumed >= 1` whenever `len > 0`, which is what makes every
/// caller's scan loop terminate on arbitrary bytes.
struct Decoded {
    std::uint32_t cp;
    std::size_t len;
    bool valid;
};

[[nodiscard]] constexpr Decoded decode(const std::uint8_t* bytes, std::size_t len) noexcept {
    if (len == 0) {
        return {kReplacement, 0, false};
    }

    const std::uint8_t b0 = bytes[0];

    if (b0 < 0x80) {
        return {b0, 1, true};
    }

    // Continuation byte or an invalid lead (0xC0/0xC1 are always overlong,
    // 0xF5..0xFF exceed U+10FFFF).
    if (b0 < 0xC2 || b0 > 0xF4) {
        return {kReplacement, 1, false};
    }

    const auto is_cont = [](std::uint8_t b) noexcept { return (b & 0xC0) == 0x80; };

    if (b0 < 0xE0) {
        if (len < 2 || !is_cont(bytes[1])) {
            return {kReplacement, 1, false};
        }
        const std::uint32_t cp = (static_cast<std::uint32_t>(b0 & 0x1F) << 6) |
                                 static_cast<std::uint32_t>(bytes[1] & 0x3F);
        return {cp, 2, true};
    }

    if (b0 < 0xF0) {
        if (len < 3 || !is_cont(bytes[1]) || !is_cont(bytes[2])) {
            return {kReplacement, 1, false};
        }
        const std::uint32_t cp = (static_cast<std::uint32_t>(b0 & 0x0F) << 12) |
                                 (static_cast<std::uint32_t>(bytes[1] & 0x3F) << 6) |
                                 static_cast<std::uint32_t>(bytes[2] & 0x3F);
        // Overlong (below U+0800) and surrogate halves are both malformed.
        if (cp < 0x800 || (cp >= kSurrogateLo && cp <= kSurrogateHi)) {
            return {kReplacement, 1, false};
        }
        return {cp, 3, true};
    }

    if (len < 4 || !is_cont(bytes[1]) || !is_cont(bytes[2]) || !is_cont(bytes[3])) {
        return {kReplacement, 1, false};
    }
    const std::uint32_t cp = (static_cast<std::uint32_t>(b0 & 0x07) << 18) |
                             (static_cast<std::uint32_t>(bytes[1] & 0x3F) << 12) |
                             (static_cast<std::uint32_t>(bytes[2] & 0x3F) << 6) |
                             static_cast<std::uint32_t>(bytes[3] & 0x3F);
    if (cp < 0x10000 || cp > kMaxCodePoint) {
        return {kReplacement, 1, false};
    }
    return {cp, 4, true};
}

/// UAX #29 grapheme cluster break property.
enum class Gcb : std::uint8_t {
    Other,
    CR,
    LF,
    Control,
    Extend,
    ZWJ,
    RegionalIndicator,
    Prepend,
    SpacingMark,
    HangulL,
    HangulV,
    HangulT,
    HangulLV,
    HangulLVT,
};

[[nodiscard]] inline Gcb gcb(std::uint32_t cp) noexcept {
    // Order matters: ZWJ is also Extend-adjacent in some tables, and CR/LF must
    // be classified before the general Control range.
    if (cp == 0x0D) {
        return Gcb::CR;
    }
    if (cp == 0x0A) {
        return Gcb::LF;
    }
    if (cp == kZwj) {
        return Gcb::ZWJ;
    }

    // Control: Cc, Cs, Cf, Zl, Zp minus the exceptions UAX #29 carves out.
    // ZWJ (handled above) and the variation selectors are Cf but must behave as
    // Extend, so they are excluded here.
    if (cp < 0x20 || (cp >= 0x7F && cp < 0xA0)) {
        return Gcb::Control;
    }

    if (contains(tables::kRegionalIndicator, tables::kRegionalIndicatorCount, cp)) {
        return Gcb::RegionalIndicator;
    }
    if (contains(tables::kPrepend, tables::kPrependCount, cp)) {
        return Gcb::Prepend;
    }
    if (contains(tables::kSpacingMark, tables::kSpacingMarkCount, cp)) {
        return Gcb::SpacingMark;
    }
    if (contains(tables::kZeroWidth, tables::kZeroWidthCount, cp)) {
        return Gcb::Extend;
    }

    if (tables::isHangulLV(cp)) {
        return Gcb::HangulLV;
    }
    if (tables::isHangulLVT(cp)) {
        return Gcb::HangulLVT;
    }
    if (contains(tables::kHangulL, tables::kHangulLCount, cp)) {
        return Gcb::HangulL;
    }
    if (contains(tables::kHangulV, tables::kHangulVCount, cp)) {
        return Gcb::HangulV;
    }
    if (contains(tables::kHangulT, tables::kHangulTCount, cp)) {
        return Gcb::HangulT;
    }

    return Gcb::Other;
}

[[nodiscard]] inline bool isExtendedPictographic(std::uint32_t cp) noexcept {
    return contains(tables::kExtendedPictographic, tables::kExtendedPictographicCount, cp);
}

[[nodiscard]] inline bool isIncbConsonant(std::uint32_t cp) noexcept {
    return contains(tables::kIncbConsonant, tables::kIncbConsonantCount, cp);
}

[[nodiscard]] inline bool isIncbLinker(std::uint32_t cp) noexcept {
    return contains(tables::kIncbLinker, tables::kIncbLinkerCount, cp);
}

[[nodiscard]] inline bool isIncbExtend(std::uint32_t cp) noexcept {
    return contains(tables::kIncbExtend, tables::kIncbExtendCount, cp);
}

/// Width of a single code point, before cluster-level adjustment.
[[nodiscard]] inline int codePointWidth(std::uint32_t cp,
                                        supra_width_ambiguous ambiguous) noexcept {
    // Controls, surrogates, and out-of-range values must not reach a terminal.
    if (cp < 0x20 || (cp >= 0x7F && cp < 0xA0)) {
        return SUPRA_WIDTH_NONPRINTABLE;
    }
    if ((cp >= kSurrogateLo && cp <= kSurrogateHi) || cp > kMaxCodePoint) {
        return SUPRA_WIDTH_NONPRINTABLE;
    }

    // Combining marks, format characters, Extend. Checked before Wide because
    // a few Extend code points are also Wide, and zero must win.
    if (contains(tables::kZeroWidth, tables::kZeroWidthCount, cp)) {
        return 0;
    }

    // Skin-tone modifiers replace the base's presentation rather than adding a
    // cell, so they contribute nothing on their own.
    if (contains(tables::kEmojiModifier, tables::kEmojiModifierCount, cp)) {
        return 0;
    }

    if (contains(tables::kWide, tables::kWideCount, cp)) {
        return 2;
    }

    // Default emoji presentation renders double-width even where East Asian
    // Width does not say so.
    if (contains(tables::kEmojiPresentation, tables::kEmojiPresentationCount, cp)) {
        return 2;
    }

    if (contains(tables::kAmbiguous, tables::kAmbiguousCount, cp)) {
        return ambiguous == SUPRA_WIDTH_AMBIGUOUS_WIDE ? 2 : 1;
    }

    return 1;
}

}  // namespace supra::width::detail

#endif  // SUPRA_WIDTH_INTERNAL_HPP
