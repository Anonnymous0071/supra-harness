// Code point and cluster width, including the ambiguous-width contract.
//
// The ambiguous cases carry the most weight: they are why this library exists
// as a separate component rather than a lookup table inside the TUI.

#include <cstdint>
#include <string>

#include "supra/width.h"
#include "test_assert.hpp"

namespace {

constexpr auto kNarrow = SUPRA_WIDTH_AMBIGUOUS_NARROW;
constexpr auto kWide = SUPRA_WIDTH_AMBIGUOUS_WIDE;

std::size_t measure(const std::string& text, supra_width_ambiguous amb) {
    return supra_wcswidth(reinterpret_cast<const std::uint8_t*>(text.data()), text.size(), amb);
}

int clusterWidth(const std::string& text, supra_width_ambiguous amb) {
    return supra_grapheme_width(reinterpret_cast<const std::uint8_t*>(text.data()), text.size(),
                                amb, nullptr);
}

void testAscii() {
    for (std::uint32_t cp = 0x20; cp < 0x7F; ++cp) {
        SUPRA_CHECK_EQ_MSG(supra_wcwidth(cp, kNarrow), 1,
                           "printable ASCII " + supra::test::codepoint(cp));
    }
    SUPRA_CHECK_EQ(measure("hello", kNarrow), std::size_t{5});
}

void testNonPrintable() {
    // C0 controls, DEL, and C1 controls must be distinguishable from a
    // legitimate zero width: the caller has to know "must not be sent" from
    // "renders as nothing".
    SUPRA_CHECK_EQ(supra_wcwidth(0x00, kNarrow), SUPRA_WIDTH_NONPRINTABLE);
    SUPRA_CHECK_EQ(supra_wcwidth(0x07, kNarrow), SUPRA_WIDTH_NONPRINTABLE);
    SUPRA_CHECK_EQ(supra_wcwidth(0x0A, kNarrow), SUPRA_WIDTH_NONPRINTABLE);
    SUPRA_CHECK_EQ(supra_wcwidth(0x1B, kNarrow), SUPRA_WIDTH_NONPRINTABLE);
    SUPRA_CHECK_EQ(supra_wcwidth(0x7F, kNarrow), SUPRA_WIDTH_NONPRINTABLE);
    SUPRA_CHECK_EQ(supra_wcwidth(0x9F, kNarrow), SUPRA_WIDTH_NONPRINTABLE);

    // Surrogates and out-of-range values.
    SUPRA_CHECK_EQ(supra_wcwidth(0xD800, kNarrow), SUPRA_WIDTH_NONPRINTABLE);
    SUPRA_CHECK_EQ(supra_wcwidth(0xDFFF, kNarrow), SUPRA_WIDTH_NONPRINTABLE);
    SUPRA_CHECK_EQ(supra_wcwidth(0x110000, kNarrow), SUPRA_WIDTH_NONPRINTABLE);
}

void testZeroWidth() {
    SUPRA_CHECK_EQ(supra_wcwidth(0x0300, kNarrow), 0);  // combining grave
    SUPRA_CHECK_EQ(supra_wcwidth(0x0308, kNarrow), 0);  // combining diaeresis
    SUPRA_CHECK_EQ(supra_wcwidth(0x200B, kNarrow), 0);  // zero width space
    SUPRA_CHECK_EQ(supra_wcwidth(0x200D, kNarrow), 0);  // ZWJ
    SUPRA_CHECK_EQ(supra_wcwidth(0xFE0F, kNarrow), 0);  // VS16
}

void testEastAsianWide() {
    SUPRA_CHECK_EQ(supra_wcwidth(0x4E2D, kNarrow), 2);  // CJK
    SUPRA_CHECK_EQ(supra_wcwidth(0x3042, kNarrow), 2);  // hiragana A
    SUPRA_CHECK_EQ(supra_wcwidth(0xAC00, kNarrow), 2);  // Hangul GA
    SUPRA_CHECK_EQ(supra_wcwidth(0xFF21, kNarrow), 2);  // fullwidth A
    SUPRA_CHECK_EQ(supra_wcwidth(0x1F600, kNarrow), 2); // grinning face

    // Wide is unconditional: the ambiguous flag must not change it.
    SUPRA_CHECK_EQ(supra_wcwidth(0x4E2D, kWide), 2);

    SUPRA_CHECK_EQ(measure("\xE4\xB8\xAD\xE6\x96\x87", kNarrow), std::size_t{4});
}

/// The reason this library exists.
///
/// Nearly every glyph the supra TUI wants - block elements for the context bar,
/// the because/therefore pair for thinking blocks, geometric shapes for status
/// markers - is East Asian Ambiguous. One cell in a Latin locale, two under a
/// CJK locale. Baking in either answer corrupts layout for half the world's
/// terminals.
void testAmbiguous() {
    // Block elements: the context gauge. Note that the block range is not
    // uniformly Ambiguous - U+2590..U+2591 are Neutral while their neighbours
    // are Ambiguous - so the gauge must be probed glyph by glyph rather than
    // assumed safe because "block elements are ambiguous".
    SUPRA_CHECK_EQ_MSG(supra_wcwidth(0x2588, kNarrow), 1, "FULL BLOCK narrow");
    SUPRA_CHECK_EQ_MSG(supra_wcwidth(0x2588, kWide), 2, "FULL BLOCK wide");
    SUPRA_CHECK_EQ_MSG(supra_wcwidth(0x2593, kNarrow), 1, "DARK SHADE narrow");
    SUPRA_CHECK_EQ_MSG(supra_wcwidth(0x2593, kWide), 2, "DARK SHADE wide");

    // U+2591 LIGHT SHADE is Neutral, not Ambiguous: one cell in every locale.
    // The natural pairing with U+2588 for a gauge therefore mixes width
    // classes, which is exactly the trap the startup probe exists to catch.
    SUPRA_CHECK_EQ_MSG(supra_wcwidth(0x2591, kNarrow), 1, "LIGHT SHADE is Neutral, narrow");
    SUPRA_CHECK_EQ_MSG(supra_wcwidth(0x2591, kWide), 1, "LIGHT SHADE is Neutral, still 1 wide");

    // Geometric shapes: status markers.
    SUPRA_CHECK_EQ_MSG(supra_wcwidth(0x25CF, kNarrow), 1, "BLACK CIRCLE narrow");
    SUPRA_CHECK_EQ_MSG(supra_wcwidth(0x25CF, kWide), 2, "BLACK CIRCLE wide");
    SUPRA_CHECK_EQ_MSG(supra_wcwidth(0x25D0, kNarrow), 1, "CIRCLE LEFT HALF BLACK narrow");
    SUPRA_CHECK_EQ_MSG(supra_wcwidth(0x25D0, kWide), 2, "CIRCLE LEFT HALF BLACK wide");

    // Mathematical operators: the thinking-block glyph pair. Ambiguous, which
    // is why the TUI probes them and falls back to words.
    SUPRA_CHECK_EQ_MSG(supra_wcwidth(0x2235, kNarrow), 1, "BECAUSE narrow");
    SUPRA_CHECK_EQ_MSG(supra_wcwidth(0x2235, kWide), 2, "BECAUSE wide");
    SUPRA_CHECK_EQ_MSG(supra_wcwidth(0x2234, kNarrow), 1, "THEREFORE narrow");
    SUPRA_CHECK_EQ_MSG(supra_wcwidth(0x2234, kWide), 2, "THEREFORE wide");
}

/// Braille was chosen for the spinner precisely because it is Neutral, not
/// Ambiguous: one cell in every locale, so the spinner cannot shift a layout.
void testBrailleIsUnambiguous() {
    for (std::uint32_t cp = 0x2800; cp <= 0x28FF; ++cp) {
        SUPRA_CHECK_EQ_MSG(supra_wcwidth(cp, kNarrow), 1,
                           "braille narrow " + supra::test::codepoint(cp));
        SUPRA_CHECK_EQ_MSG(supra_wcwidth(cp, kWide), 1,
                           "braille wide " + supra::test::codepoint(cp));
    }
}

void testCombiningClusters() {
    // Base plus combining mark is the width of the base, not the sum.
    SUPRA_CHECK_EQ(clusterWidth("e\xCC\x81", kNarrow), 1);        // e + acute
    SUPRA_CHECK_EQ(measure("e\xCC\x81", kNarrow), std::size_t{1});
    SUPRA_CHECK_EQ(measure("cafe\xCC\x81", kNarrow), std::size_t{4});

    // Several marks on one base still occupy one cell.
    SUPRA_CHECK_EQ(measure("a\xCC\x80\xCC\x81\xCC\x82", kNarrow), std::size_t{1});
}

void testEmojiSequences() {
    // Single emoji: two cells.
    SUPRA_CHECK_EQ(measure("\xF0\x9F\x98\x80", kNarrow), std::size_t{2});

    // Skin tone modifier does not add a cell - it replaces presentation.
    SUPRA_CHECK_EQ_MSG(measure("\xF0\x9F\x91\x8D\xF0\x9F\x8F\xBD", kNarrow), std::size_t{2},
                       "thumbs up + medium skin tone");

    // Family ZWJ sequence: four people joined, still two cells. Summing code
    // point widths would give eight and wreck the line.
    const std::string family = "\xF0\x9F\x91\xA8"      // man
                               "\xE2\x80\x8D"          // ZWJ
                               "\xF0\x9F\x91\xA9"      // woman
                               "\xE2\x80\x8D"          // ZWJ
                               "\xF0\x9F\x91\xA7";     // girl
    SUPRA_CHECK_EQ_MSG(measure(family, kNarrow), std::size_t{2}, "family ZWJ sequence");

    // Flag: two regional indicators are one flag, two cells.
    const std::string flag_id = "\xF0\x9F\x87\xAE\xF0\x9F\x87\xA9";  // ID
    SUPRA_CHECK_EQ_MSG(measure(flag_id, kNarrow), std::size_t{2}, "one flag");

    // Two flags are four cells, not two: pairing must reset.
    const std::string two_flags = "\xF0\x9F\x87\xAE\xF0\x9F\x87\xA9"
                                  "\xF0\x9F\x87\xAF\xF0\x9F\x87\xB5";
    SUPRA_CHECK_EQ_MSG(measure(two_flags, kNarrow), std::size_t{4}, "two flags");
}

/// Variation selectors change presentation, not cell count. VS15 forcing text
/// presentation is the case that most often renders narrow while a naive
/// measurement assumes wide.
void testVariationSelectors() {
    // U+2764 HEAVY BLACK HEART is Ambiguous on its own.
    SUPRA_CHECK_EQ(measure("\xE2\x9D\xA4", kNarrow), std::size_t{1});
    // With VS16 it is emoji presentation: two cells.
    SUPRA_CHECK_EQ_MSG(measure("\xE2\x9D\xA4\xEF\xB8\x8F", kNarrow), std::size_t{2},
                       "heart + VS16");
    // With VS15 it is text presentation: one cell.
    SUPRA_CHECK_EQ_MSG(measure("\xE2\x9D\xA4\xEF\xB8\x8E", kNarrow), std::size_t{1},
                       "heart + VS15");
}

void testMeasureClusters() {
    std::size_t clusters = 0;
    const std::string text = "a\xCC\x81"                          // 1 cluster
                             "\xF0\x9F\x91\xA8\xE2\x80\x8D\xF0\x9F\x91\xA9"  // 1 cluster
                             "z";                                 // 1 cluster
    const std::size_t width = supra_width_measure(
        reinterpret_cast<const std::uint8_t*>(text.data()), text.size(), kNarrow, &clusters);
    SUPRA_CHECK_EQ_MSG(clusters, std::size_t{3}, "cluster count");
    SUPRA_CHECK_EQ_MSG(width, std::size_t{4}, "1 + 2 + 1 cells");
}

void testNonPrintableInStringContributesZero() {
    // Measuring reports the room text needs; a control needs none. Rejecting
    // such text is supra_width_validate's job, not the measurer's.
    SUPRA_CHECK_EQ(measure("a\x07\x62", kNarrow), std::size_t{2});
}

void testEmptyAndNull() {
    SUPRA_CHECK_EQ(measure("", kNarrow), std::size_t{0});
    SUPRA_CHECK_EQ(supra_wcswidth(nullptr, 0, kNarrow), std::size_t{0});
    SUPRA_CHECK_EQ(supra_grapheme_width(nullptr, 0, kNarrow, nullptr), SUPRA_WIDTH_NONPRINTABLE);
}

void testUnicodeVersion() {
    const char* version = supra_width_unicode_version();
    SUPRA_CHECK(version != nullptr);
    SUPRA_CHECK(std::string(version) == "17.0.0");
}

}  // namespace

int main() {
    testAscii();
    testNonPrintable();
    testZeroWidth();
    testEastAsianWide();
    testAmbiguous();
    testBrailleIsUnambiguous();
    testCombiningClusters();
    testEmojiSequences();
    testVariationSelectors();
    testMeasureClusters();
    testNonPrintableInStringContributesZero();
    testEmptyAndNull();
    testUnicodeVersion();
    return supra::test::finish("width_test");
}
