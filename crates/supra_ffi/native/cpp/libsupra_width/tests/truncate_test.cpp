// Truncation, validation, and the startup width probe.
//
// Truncation is where a width bug becomes visible corruption: a half-emitted
// wide glyph leaves a broken cell that persists until the next full repaint.

#include <cstdint>
#include <string>

#include "supra/width.h"
#include "supra/testing.hpp"

namespace {

constexpr auto kNarrow = SUPRA_WIDTH_AMBIGUOUS_NARROW;
constexpr auto kWide = SUPRA_WIDTH_AMBIGUOUS_WIDE;

struct Truncated {
    std::size_t bytes;
    std::size_t cells;
};

Truncated truncate(const std::string& text, std::size_t max_cells, supra_width_ambiguous amb) {
    Truncated out{};
    out.bytes = supra_width_truncate(reinterpret_cast<const std::uint8_t*>(text.data()),
                                     text.size(), max_cells, amb, &out.cells);
    return out;
}

void testAsciiTruncation() {
    const std::string text = "hello world";
    SUPRA_CHECK_EQ(truncate(text, 5, kNarrow).bytes, std::size_t{5});
    SUPRA_CHECK_EQ(truncate(text, 5, kNarrow).cells, std::size_t{5});
    SUPRA_CHECK_EQ(truncate(text, 0, kNarrow).bytes, std::size_t{0});
    // A limit beyond the text returns the whole text, not an error.
    SUPRA_CHECK_EQ(truncate(text, 100, kNarrow).bytes, text.size());
    SUPRA_CHECK_EQ(truncate(text, 100, kNarrow).cells, std::size_t{11});
}

/// A two-cell cluster that would cross the limit is excluded entirely, so the
/// result may measure one cell short. Emitting half of it would corrupt the
/// cell instead of narrowing the glyph.
void testWideNeverSplit() {
    const std::string cjk = "\xE4\xB8\xAD\xE6\x96\x87";  // 2 chars, 4 cells

    const auto at_three = truncate(cjk, 3, kNarrow);
    SUPRA_CHECK_EQ_MSG(at_three.cells, std::size_t{2}, "3-cell limit fits only the first char");
    SUPRA_CHECK_EQ_MSG(at_three.bytes, std::size_t{3}, "one CJK char is 3 bytes");

    const auto at_one = truncate(cjk, 1, kNarrow);
    SUPRA_CHECK_EQ_MSG(at_one.cells, std::size_t{0}, "1-cell limit fits no wide char");
    SUPRA_CHECK_EQ_MSG(at_one.bytes, std::size_t{0}, "nothing emitted");

    const auto at_four = truncate(cjk, 4, kNarrow);
    SUPRA_CHECK_EQ(at_four.cells, std::size_t{4});
    SUPRA_CHECK_EQ(at_four.bytes, cjk.size());
}

/// Clusters are atomic. Cutting inside one drops the base or orphans a mark,
/// both of which render as a different character than the source.
void testClusterAtomicity() {
    // Base plus two combining marks: one cell, five bytes.
    const std::string text = "a\xCC\x80\xCC\x81";
    const auto result = truncate(text, 1, kNarrow);
    SUPRA_CHECK_EQ_MSG(result.bytes, text.size(), "whole cluster or nothing");
    SUPRA_CHECK_EQ(result.cells, std::size_t{1});

    // Family ZWJ sequence: two cells, 18 bytes. A 1-cell limit must emit none
    // of it rather than the first person.
    const std::string family = "\xF0\x9F\x91\xA8\xE2\x80\x8D\xF0\x9F\x91\xA9\xE2\x80\x8D"
                               "\xF0\x9F\x91\xA7";
    SUPRA_CHECK_EQ_MSG(truncate(family, 1, kNarrow).bytes, std::size_t{0},
                       "no partial ZWJ sequence");
    SUPRA_CHECK_EQ_MSG(truncate(family, 2, kNarrow).bytes, family.size(),
                       "whole sequence at 2 cells");
}

/// Never exceed the limit, at any width, for any input. This is the invariant
/// the TUI depends on: one over-wide line wraps and desynchronises the whole
/// frame.
void testNeverExceedsLimit() {
    const std::string inputs[] = {
        "hello",
        "\xE4\xB8\xAD\xE6\x96\x87\xE5\xAD\x97",              // CJK
        "\xF0\x9F\x98\x80\xF0\x9F\x98\x81",                  // emoji
        "a\xCC\x81\x65\xCC\x82\x69\xCC\x83",                 // combining
        "\xF0\x9F\x87\xAE\xF0\x9F\x87\xA9",                  // flag
        "\xE2\x96\x88\xE2\x96\x91\xE2\x96\x88",              // block elements
        "mixed \xE4\xB8\xAD ascii \xF0\x9F\x98\x80 end",     // mixed
    };

    for (const auto& text : inputs) {
        for (std::size_t limit = 0; limit <= 20; ++limit) {
            for (const auto amb : {kNarrow, kWide}) {
                std::size_t cells = 0;
                const std::size_t bytes =
                    supra_width_truncate(reinterpret_cast<const std::uint8_t*>(text.data()),
                                         text.size(), limit, amb, &cells);
                SUPRA_CHECK_EQ_MSG(cells <= limit, true,
                                   "limit " + std::to_string(limit) + " not exceeded for " +
                                       supra::test::hex(text));
                SUPRA_CHECK_EQ_MSG(bytes <= text.size(), true, "byte count within input");

                // The reported width must match what the prefix actually
                // measures, or callers padding to the limit compute the wrong
                // padding.
                const std::size_t remeasured = supra_wcswidth(
                    reinterpret_cast<const std::uint8_t*>(text.data()), bytes, amb);
                SUPRA_CHECK_EQ_MSG(remeasured, cells, "reported width matches prefix");
            }
        }
    }
}

void testValidate() {
    std::size_t offset = 0;

    SUPRA_CHECK_EQ(supra_width_validate(reinterpret_cast<const std::uint8_t*>("hello"), 5, &offset),
                   SUPRA_WIDTH_VALID);
    SUPRA_CHECK_EQ(offset, std::size_t{5});

    // Malformed UTF-8, with the offset of the first bad byte.
    const std::string bad = "ab\xFF"
                            "cd";
    SUPRA_CHECK_EQ(supra_width_validate(reinterpret_cast<const std::uint8_t*>(bad.data()),
                                        bad.size(), &offset),
                   SUPRA_WIDTH_INVALID_UTF8);
    SUPRA_CHECK_EQ_MSG(offset, std::size_t{2}, "offset of the invalid byte");

    // Well-formed but carrying a control character: a distinct outcome, because
    // the caller's remedy differs (strip versus reject).
    const std::string control = "ab\x07"
                                "cd";
    SUPRA_CHECK_EQ(supra_width_validate(reinterpret_cast<const std::uint8_t*>(control.data()),
                                        control.size(), &offset),
                   SUPRA_WIDTH_HAS_NONPRINTABLE);
    SUPRA_CHECK_EQ(offset, std::size_t{2});

    SUPRA_CHECK_EQ(supra_width_validate(nullptr, 0, &offset), SUPRA_WIDTH_VALID);
}

/// The probe is how the TUI decides between its Unicode and ASCII glyph tiers.
/// It must reject anything it cannot measure as exactly one cluster, or the
/// caller would conclude a two-glyph fallback fits in one cell.
void testProbe() {
    SUPRA_CHECK_EQ(supra_width_probe("a", kNarrow), 1);
    SUPRA_CHECK_EQ(supra_width_probe("\xE4\xB8\xAD", kNarrow), 2);

    // Braille: one cell in both locales, which is why the spinner uses it.
    SUPRA_CHECK_EQ(supra_width_probe("\xE2\xA0\x8B", kNarrow), 1);
    SUPRA_CHECK_EQ(supra_width_probe("\xE2\xA0\x8B", kWide), 1);

    // Ambiguous glyphs report differently per locale. This is the signal the
    // TUI acts on.
    SUPRA_CHECK_EQ_MSG(supra_width_probe("\xE2\x88\xB5", kNarrow), 1, "BECAUSE narrow");
    SUPRA_CHECK_EQ_MSG(supra_width_probe("\xE2\x88\xB5", kWide), 2, "BECAUSE wide");

    // A glyph plus VS15 is still one cluster.
    SUPRA_CHECK_EQ(supra_width_probe("\xE2\x9D\xA4\xEF\xB8\x8E", kNarrow), 1);

    // Rejected: empty, NULL, malformed, and multi-cluster input.
    SUPRA_CHECK_EQ(supra_width_probe(nullptr, kNarrow), SUPRA_WIDTH_NONPRINTABLE);
    SUPRA_CHECK_EQ(supra_width_probe("", kNarrow), SUPRA_WIDTH_NONPRINTABLE);
    SUPRA_CHECK_EQ_MSG(supra_width_probe("\xFF", kNarrow), SUPRA_WIDTH_NONPRINTABLE,
                       "malformed input rejected, not measured as U+FFFD");
    SUPRA_CHECK_EQ_MSG(supra_width_probe("ab", kNarrow), SUPRA_WIDTH_NONPRINTABLE,
                       "two clusters rejected");
    SUPRA_CHECK_EQ_MSG(supra_width_probe("\x07", kNarrow), SUPRA_WIDTH_NONPRINTABLE,
                       "control rejected");
}

}  // namespace

int main() {
    testAsciiTruncation();
    testWideNeverSplit();
    testClusterAtomicity();
    testNeverExceedsLimit();
    testValidate();
    testProbe();
    return supra::test::finish("truncate_test");
}
