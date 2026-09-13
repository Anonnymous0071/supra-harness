// Internal table declarations for libsupra_width.
//
// Definitions live in tables_generated.cpp, produced by
// tools/gen_width_tables.py from the vendored UCD extracts under data/.
//
// Not part of the public ABI: nothing here appears in include/supra/width.h.

#ifndef SUPRA_WIDTH_TABLES_HPP
#define SUPRA_WIDTH_TABLES_HPP

#include <cstddef>
#include <cstdint>

namespace supra::width::tables {

/// Inclusive code point range. Tables are sorted and coalesced, which is what
/// makes binary search valid.
struct Range {
    std::uint32_t lo;
    std::uint32_t hi;
};

extern const char* kUnicodeVersion;

// Width classes.
//
// Wide covers East Asian Width W and F: unconditionally two cells.
//
// Ambiguous covers East Asian Width A, deliberately kept separate. The same
// code point is one cell in a Latin locale and two in a CJK locale, so the
// runtime resolves it from a flag rather than the table baking in an answer.
// Every terminal layout bug involving a "nice" glyph originates here.
extern const Range kWide[];
extern const std::size_t kWideCount;
extern const Range kAmbiguous[];
extern const std::size_t kAmbiguousCount;

/// Mn, Me, Cf, and the UAX #29 Extend class: occupies no cell of its own.
extern const Range kZeroWidth[];
extern const std::size_t kZeroWidthCount;

// Emoji properties.
extern const Range kEmojiPresentation[];
extern const std::size_t kEmojiPresentationCount;
extern const Range kExtendedPictographic[];
extern const std::size_t kExtendedPictographicCount;
extern const Range kEmojiModifier[];
extern const std::size_t kEmojiModifierCount;
extern const Range kEmoji[];
extern const std::size_t kEmojiCount;

// UAX #29 grapheme cluster break classes.
extern const Range kPrepend[];
extern const std::size_t kPrependCount;
extern const Range kSpacingMark[];
extern const std::size_t kSpacingMarkCount;
extern const Range kRegionalIndicator[];
extern const std::size_t kRegionalIndicatorCount;
extern const Range kHangulL[];
extern const std::size_t kHangulLCount;
extern const Range kHangulV[];
extern const std::size_t kHangulVCount;
extern const Range kHangulT[];
extern const std::size_t kHangulTCount;

// Precomposed Hangul syllables (LV and LVT) carry no table. The 11 172
// syllables form one contiguous block with a regular period, so the classes
// are derived arithmetically; as tables they would cost 798 ranges for
// information a modulo already carries. tools/gen_width_tables.py verifies the
// derivation against the UCD on every regeneration, and
// tests/hangul_arithmetic_test.cpp checks it at build time.
constexpr std::uint32_t kHangulSBase = 0xAC00;
constexpr std::uint32_t kHangulSCount = 11172;
constexpr std::uint32_t kHangulTCountMod = 28;

/// True for a precomposed syllable with no trailing jamo (UAX #29 class LV).
[[nodiscard]] constexpr bool isHangulLV(std::uint32_t cp) noexcept {
    if (cp < kHangulSBase || cp >= kHangulSBase + kHangulSCount) {
        return false;
    }
    return (cp - kHangulSBase) % kHangulTCountMod == 0;
}

/// True for a precomposed syllable carrying a trailing jamo (class LVT).
[[nodiscard]] constexpr bool isHangulLVT(std::uint32_t cp) noexcept {
    if (cp < kHangulSBase || cp >= kHangulSBase + kHangulSCount) {
        return false;
    }
    return (cp - kHangulSBase) % kHangulTCountMod != 0;
}

// Indic conjunct break, for GB9c.
extern const Range kIncbLinker[];
extern const std::size_t kIncbLinkerCount;
extern const Range kIncbConsonant[];
extern const std::size_t kIncbConsonantCount;
extern const Range kIncbExtend[];
extern const std::size_t kIncbExtendCount;

/// Binary search a sorted, coalesced range table.
///
/// constexpr and header-inline: this is the hottest call in the render path,
/// invoked per code point per frame.
[[nodiscard]] constexpr bool contains(const Range* table, std::size_t count,
                                      std::uint32_t cp) noexcept {
    std::size_t lo = 0;
    std::size_t hi = count;
    while (lo < hi) {
        const std::size_t mid = lo + ((hi - lo) / 2);
        if (cp < table[mid].lo) {
            hi = mid;
        } else if (cp > table[mid].hi) {
            lo = mid + 1;
        } else {
            return true;
        }
    }
    return false;
}

}  // namespace supra::width::tables

#endif  // SUPRA_WIDTH_TABLES_HPP
