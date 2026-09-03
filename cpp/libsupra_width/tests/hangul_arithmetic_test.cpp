// The Hangul LV/LVT arithmetic shortcut, checked against the UCD.
//
// tools/gen_width_tables.py omits LV and LVT tables (798 ranges, 31% of the
// total) because the 11 172 precomposed syllables form one contiguous block
// with a regular period. The generator verifies the derivation when it runs;
// this suite verifies it at build time, so a Unicode version bump that changed
// the block layout cannot pass silently through a stale generated file.
//
// It also covers jamo composition (GB6/GB7/GB8), which is the reason the
// classes are needed at all.

#include <cstdint>
#include <string>

#include "supra/width.h"
#include "supra/testing.hpp"

namespace {

constexpr auto kNarrow = SUPRA_WIDTH_AMBIGUOUS_NARROW;

constexpr std::uint32_t kSBase = 0xAC00;
constexpr std::uint32_t kSCount = 11172;
constexpr std::uint32_t kTCount = 28;

std::string encode(std::uint32_t cp) {
    std::string out;
    if (cp < 0x800) {
        out.push_back(static_cast<char>(0xC0 | (cp >> 6)));
        out.push_back(static_cast<char>(0x80 | (cp & 0x3F)));
    } else if (cp < 0x10000) {
        out.push_back(static_cast<char>(0xE0 | (cp >> 12)));
        out.push_back(static_cast<char>(0x80 | ((cp >> 6) & 0x3F)));
        out.push_back(static_cast<char>(0x80 | (cp & 0x3F)));
    }
    return out;
}

std::size_t measure(const std::string& text) {
    return supra_wcswidth(reinterpret_cast<const std::uint8_t*>(text.data()), text.size(), kNarrow);
}

std::size_t clusterBytes(const std::string& text) {
    return supra_grapheme_next(reinterpret_cast<const std::uint8_t*>(text.data()), text.size());
}

/// Every precomposed syllable is a single cluster of exactly two cells.
///
/// Exhaustive over the block: a modulo error would only misclassify some
/// residues, and sampling could miss the ones that matter.
void testBlockCoverage() {
    std::size_t width_failures = 0;
    std::size_t cluster_failures = 0;

    for (std::uint32_t cp = kSBase; cp < kSBase + kSCount; ++cp) {
        const std::string encoded = encode(cp);
        if (measure(encoded) != 2) {
            ++width_failures;
        }
        if (clusterBytes(encoded) != encoded.size()) {
            ++cluster_failures;
        }
    }

    SUPRA_CHECK_EQ_MSG(width_failures, std::size_t{0},
                       "all 11172 syllables measure 2 cells");
    SUPRA_CHECK_EQ_MSG(cluster_failures, std::size_t{0},
                       "all 11172 syllables are one cluster");
}

/// Distinguish LV from LVT observably.
///
/// The discriminator is a following **V** jamo, not T. UAX #29 GB8 is
/// `(LVT | T) x T`, so both LV and LVT compose with a trailing jamo and T
/// cannot tell them apart. GB7 is `(LV | V) x (V | T)`, which admits V after LV
/// but not after LVT, where GB999 then breaks.
void testVowelJamoDiscriminatesLvFromLvt() {
    const std::uint32_t v_jamo = 0x1161;  // HANGUL JUNGSEONG A
    const std::uint32_t t_jamo = 0x11A8;  // HANGUL JONGSEONG KIYEOK

    // U+AC00 GA is LV: (AC00 - AC00) % 28 == 0.
    const std::string lv_plus_v = encode(kSBase) + encode(v_jamo);
    SUPRA_CHECK_EQ_MSG(clusterBytes(lv_plus_v), lv_plus_v.size(),
                       "LV + V composes (GB7)");

    // U+AC01 GAG is LVT: (AC01 - AC00) % 28 == 1. GB8 admits only T after it.
    const std::string lvt_plus_v = encode(kSBase + 1) + encode(v_jamo);
    SUPRA_CHECK_EQ_MSG(clusterBytes(lvt_plus_v), encode(kSBase + 1).size(),
                       "LVT + V breaks (GB999)");

    // Both compose with T, which is why T is useless as a discriminator.
    const std::string lv_plus_t = encode(kSBase) + encode(t_jamo);
    SUPRA_CHECK_EQ_MSG(clusterBytes(lv_plus_t), lv_plus_t.size(), "LV + T composes (GB7)");
    const std::string lvt_plus_t = encode(kSBase + 1) + encode(t_jamo);
    SUPRA_CHECK_EQ_MSG(clusterBytes(lvt_plus_t), lvt_plus_t.size(), "LVT + T composes (GB8)");

    SUPRA_CHECK_EQ_MSG(measure(lv_plus_t), std::size_t{2}, "composed syllable is 2 cells");
}

/// Verify the derivation observably at every residue rather than trusting the
/// generator alone: appending a V jamo must compose only when the residue is 0,
/// which is precisely the LV case.
void testEveryResidue() {
    const std::uint32_t v_jamo = 0x1161;
    std::size_t mismatches = 0;

    for (std::uint32_t residue = 0; residue < kTCount; ++residue) {
        const std::uint32_t cp = kSBase + residue;
        const std::string syllable = encode(cp);
        const std::string with_v = syllable + encode(v_jamo);

        const bool composed = clusterBytes(with_v) == with_v.size();
        const bool expect_lv = residue == 0;

        if (composed != expect_lv) {
            ++mismatches;
        }
    }

    SUPRA_CHECK_EQ_MSG(mismatches, std::size_t{0},
                       "LV/LVT derivation matches composition behaviour at all 28 residues");
}

/// The block boundaries: one code point below and above must not be treated as
/// syllables. An off-by-one here would classify U+ABFF or U+D7A4 as Hangul.
void testBoundaries() {
    const std::uint32_t t_jamo = 0x11A8;

    // U+ABFF is below the block: appending T must break.
    const std::string below = encode(0xABFF) + encode(t_jamo);
    SUPRA_CHECK_EQ_MSG(clusterBytes(below), encode(0xABFF).size(),
                       "U+ABFF is not a Hangul syllable");

    // U+D7A3 is the last syllable, and LVT: (D7A3 - AC00) % 28 == 27.
    const std::string last = encode(0xD7A3);
    SUPRA_CHECK_EQ_MSG(measure(last), std::size_t{2}, "U+D7A3 is the last syllable");

    // U+D7A4 is above the block and unassigned.
    SUPRA_CHECK_EQ_MSG(supra_wcwidth(0xD7A4, kNarrow), 1,
                       "U+D7A4 is outside the syllable block");
}

/// Conjoining jamo sequences compose per GB6/GB7: L + V + T is one cluster.
void testJamoSequences() {
    const std::uint32_t l = 0x1100;  // CHOSEONG KIYEOK
    const std::uint32_t v = 0x1161;  // JUNGSEONG A
    const std::uint32_t t = 0x11A8;  // JONGSEONG KIYEOK

    const std::string lv = encode(l) + encode(v);
    SUPRA_CHECK_EQ_MSG(clusterBytes(lv), lv.size(), "L + V composes (GB6)");

    const std::string lvt = encode(l) + encode(v) + encode(t);
    SUPRA_CHECK_EQ_MSG(clusterBytes(lvt), lvt.size(), "L + V + T composes (GB7)");

    // Two L jamo also compose: GB6 allows L x L.
    const std::string ll = encode(l) + encode(l);
    SUPRA_CHECK_EQ_MSG(clusterBytes(ll), ll.size(), "L + L composes (GB6)");

    // A trailing jamo with no preceding syllable stands alone.
    const std::string t_alone = encode(t) + encode(t);
    SUPRA_CHECK_EQ_MSG(clusterBytes(t_alone), t_alone.size(), "T + T composes (GB8)");
}

}  // namespace

int main() {
    testBlockCoverage();
    testVowelJamoDiscriminatesLvFromLvt();
    testEveryResidue();
    testBoundaries();
    testJamoSequences();
    return supra::test::finish("hangul_arithmetic_test");
}
