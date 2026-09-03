// Style-safe truncation: the function this library exists for.
//
// Four guarantees, each corresponding to a corruption that outlives the line
// that caused it:
//
//   1. cells <= max_cells always. One over-wide line wraps and desynchronises
//      the whole frame.
//   2. Never cut inside an escape sequence. A fragment is interpreted as a
//      command over whatever follows.
//   3. Never split a grapheme cluster. Half a wide glyph is a corrupted cell.
//   4. Prefix plus trailer leaves the terminal in its default state. Open
//      styling bleeds into unrelated output.

#include <cstdint>
#include <string>
#include <vector>

#include "supra/ansi.h"
#include "supra/testing.hpp"
#include "supra/width.h"

namespace {

constexpr auto kNarrow = SUPRA_WIDTH_AMBIGUOUS_NARROW;
constexpr auto kWide = SUPRA_WIDTH_AMBIGUOUS_WIDE;

supra_ansi_truncation plan(const std::string& input, std::size_t max_cells,
                           supra_width_ambiguous amb = kNarrow) {
    supra_ansi_truncation out{};
    supra_ansi_plan_truncate(reinterpret_cast<const std::uint8_t*>(input.data()), input.size(),
                             max_cells, amb, &out);
    return out;
}

/// Assemble what the caller would actually write to the terminal.
std::string render(const std::string& input, const supra_ansi_truncation& t) {
    std::string out = input.substr(0, t.prefix_len);
    out.append(reinterpret_cast<const char*>(t.trailer), t.trailer_len);
    return out;
}

/// Fold a byte string and report whether the terminal is left in its default
/// state. This is the check that guarantee 4 rests on.
bool endsDefault(const std::string& bytes) {
    supra_ansi_style style = supra_ansi_style_default();
    supra_ansi_scanner scanner{};
    supra_ansi_token token{};
    const auto* raw = reinterpret_cast<const std::uint8_t*>(bytes.data());

    while (supra_ansi_scan(&scanner, raw, bytes.size(), SUPRA_ANSI_FINAL, &token) == 1) {
        supra_ansi_style_apply(&style, &token);
        supra_ansi_style_apply_osc(&style, &token);
        if (token.length == 0 && token.kind == SUPRA_ANSI_TOKEN_MALFORMED) {
            break;
        }
    }
    return supra_ansi_style_is_default(&style) == 1;
}

/// Report whether any escape sequence in `bytes` is incomplete, which is what a
/// mid-sequence cut produces.
bool hasPartialSequence(const std::string& bytes) {
    supra_ansi_scanner scanner{};
    supra_ansi_token token{};
    const auto* raw = reinterpret_cast<const std::uint8_t*>(bytes.data());

    while (supra_ansi_scan(&scanner, raw, bytes.size(), SUPRA_ANSI_FINAL, &token) == 1) {
        if (token.kind == SUPRA_ANSI_TOKEN_MALFORMED || token.kind == SUPRA_ANSI_TOKEN_PARTIAL) {
            return true;
        }
        if (token.length == 0) {
            break;
        }
    }
    return false;
}

void testPlainText() {
    const auto t = plan("hello world", 5);
    SUPRA_CHECK_EQ(t.prefix_len, std::size_t{5});
    SUPRA_CHECK_EQ(t.cells, std::size_t{5});
    SUPRA_CHECK_EQ(t.truncated, std::uint8_t{1});
    // Plain text costs no extra bytes: an unstyled line needs no trailer.
    SUPRA_CHECK_EQ_MSG(t.trailer_len, std::uint8_t{0}, "unstyled line needs no trailer");
}

void testFitsWhole() {
    const auto t = plan("short", 20);
    SUPRA_CHECK_EQ(t.prefix_len, std::size_t{5});
    SUPRA_CHECK_EQ(t.cells, std::size_t{5});
    SUPRA_CHECK_EQ_MSG(t.truncated, std::uint8_t{0}, "not truncated when it fits");
}

/// Escape sequences occupy no cells, so a styled line fits the same text as an
/// unstyled one at the same width.
void testEscapesAreFree() {
    const std::string styled = "\x1b[31mhello\x1b[0m";
    const auto t = plan(styled, 5);
    SUPRA_CHECK_EQ_MSG(t.cells, std::size_t{5}, "escapes contribute no cells");
    SUPRA_CHECK_EQ_MSG(t.prefix_len, styled.size(), "whole styled line fits in 5 cells");
    SUPRA_CHECK_EQ(t.truncated, std::uint8_t{0});
}

/// Guarantee 4. Cutting mid-colour without a trailer leaves red bleeding into
/// every subsequent line.
void testTrailerClosesStyle() {
    const std::string input = "\x1b[31mred text here";
    const auto t = plan(input, 5);

    SUPRA_CHECK_EQ(t.cells, std::size_t{5});
    SUPRA_CHECK_MSG(t.trailer_len > 0, "cut inside styling needs a trailer");

    const std::string rendered = render(input, t);
    SUPRA_CHECK_MSG(endsDefault(rendered), "rendered output ends in the default style");
    SUPRA_CHECK_MSG(!hasPartialSequence(rendered), "no partial sequence in output");
}

/// Guarantee 2. The cut point must never fall inside a sequence, or the terminal
/// receives a command fragment that applies to whatever follows.
void testNeverCutsInsideEscape() {
    // A sequence straddling every plausible cut point.
    const std::string input = "ab\x1b[38;2;255;128;0mcd\x1b[0mef";

    for (std::size_t limit = 0; limit <= 10; ++limit) {
        const auto t = plan(input, limit);
        const std::string rendered = render(input, t);
        SUPRA_CHECK_MSG(!hasPartialSequence(rendered),
                        "limit " + std::to_string(limit) + " leaves no partial sequence");
        SUPRA_CHECK_MSG(endsDefault(rendered),
                        "limit " + std::to_string(limit) + " ends in default style");
        SUPRA_CHECK_MSG(t.cells <= limit, "limit " + std::to_string(limit) + " not exceeded");
    }
}

/// A hyperlink left open makes every subsequent cell clickable, which is worse
/// than a colour bleed because it is invisible until hovered.
void testHyperlinkClosed() {
    const std::string input = "\x1b]8;;https://example.com\x1b\\clickable text";
    const auto t = plan(input, 5);

    const std::string rendered = render(input, t);
    SUPRA_CHECK_MSG(rendered.find("\x1b]8;;\x1b\\") != std::string::npos,
                    "trailer closes the hyperlink");
    SUPRA_CHECK_MSG(endsDefault(rendered), "hyperlink not left open");
}

/// Guarantee 3, inherited from libsupra_width.
void testWideClusters() {
    const std::string cjk = "\xE4\xB8\xAD\xE6\x96\x87";  // 2 chars, 4 cells

    const auto at_three = plan(cjk, 3);
    SUPRA_CHECK_EQ_MSG(at_three.cells, std::size_t{2}, "3-cell limit fits one wide char");
    SUPRA_CHECK_EQ_MSG(at_three.prefix_len, std::size_t{3}, "one CJK char is 3 bytes");

    const auto at_one = plan(cjk, 1);
    SUPRA_CHECK_EQ_MSG(at_one.cells, std::size_t{0}, "no room for a wide char");
    SUPRA_CHECK_EQ(at_one.prefix_len, std::size_t{0});
}

void testEmojiZwjSequence() {
    // Family: 5 code points, 2 cells, 18 bytes.
    const std::string family = "\xF0\x9F\x91\xA8\xE2\x80\x8D\xF0\x9F\x91\xA9\xE2\x80\x8D"
                               "\xF0\x9F\x91\xA7";

    SUPRA_CHECK_EQ_MSG(plan(family, 1).prefix_len, std::size_t{0},
                       "no partial ZWJ sequence at 1 cell");
    SUPRA_CHECK_EQ_MSG(plan(family, 2).prefix_len, family.size(),
                       "whole sequence at 2 cells");
}

/// Guarantee 1, exhaustively. One over-wide line wraps and desynchronises the
/// entire frame, so this is checked across every input shape the TUI produces.
void testNeverExceedsLimit() {
    const std::string inputs[] = {
        "plain text",
        "\x1b[31mred\x1b[0m",
        "\x1b[1;4;38;2;255;0;0mstyled\x1b[0m",
        "\xE4\xB8\xAD\xE6\x96\x87\xE5\xAD\x97",
        "\x1b[31m\xE4\xB8\xAD\x1b[0m\xE6\x96\x87",
        "\xF0\x9F\x98\x80\xF0\x9F\x98\x81",
        "\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\",
        "a\xCC\x81\x65\xCC\x82",
        "\x1b[31mred\x1b[32mgreen\x1b[34mblue\x1b[0m",
        "mixed \xE4\xB8\xAD \x1b[1mbold\x1b[0m \xF0\x9F\x98\x80 end",
        "\x9b"
        "31mC1 red",
        "\x1b[38:2::255:128:0mcolon form",
    };

    for (const auto& input : inputs) {
        for (std::size_t limit = 0; limit <= 25; ++limit) {
            for (const auto amb : {kNarrow, kWide}) {
                const auto t = plan(input, limit, amb);

                SUPRA_CHECK_MSG(t.cells <= limit,
                                "limit " + std::to_string(limit) + " not exceeded for " +
                                    supra::test::vis(input));
                SUPRA_CHECK_MSG(t.prefix_len <= input.size(), "prefix within input");

                const std::string rendered = render(input, t);
                SUPRA_CHECK_MSG(!hasPartialSequence(rendered),
                                "no partial sequence at limit " + std::to_string(limit) + " for " +
                                    supra::test::vis(input));
                SUPRA_CHECK_MSG(endsDefault(rendered),
                                "ends default at limit " + std::to_string(limit) + " for " +
                                    supra::test::vis(input));

                // The reported width must match what the prefix measures, or a
                // caller padding to the limit computes the wrong padding.
                const std::size_t remeasured = supra_ansi_measure(
                    reinterpret_cast<const std::uint8_t*>(input.data()), t.prefix_len, amb);
                SUPRA_CHECK_EQ_MSG(remeasured, t.cells, "reported width matches prefix");
            }
        }
    }
}

/// Every escape sequence inside the prefix is emitted, so `style_at_cut` reports
/// the state after all of them - including a trailing sequence that covers no
/// cell.
///
/// This is what makes the trailer sound. Reporting the style in force over the
/// last accepted *text* instead would leave that trailing sequence emitted but
/// unclosed, bleeding its styling into every following line.
void testStyleAtCut() {
    const std::string input = "\x1b[31mred\x1b[32m";
    const auto t = plan(input, 3);

    SUPRA_CHECK_EQ(t.cells, std::size_t{3});
    SUPRA_CHECK_EQ_MSG(t.prefix_len, input.size(), "the zero-width trailing sequence is emitted");
    SUPRA_CHECK_EQ_MSG(t.style_at_cut.fg.index, std::uint8_t{2},
                       "style_at_cut reflects the emitted trailing sequence");
    SUPRA_CHECK_MSG(endsDefault(render(input, t)), "trailing sequence still gets closed");

    // Cutting mid-text reports the style covering that text.
    const std::string longer = "\x1b[31mred and more";
    const auto mid = plan(longer, 3);
    SUPRA_CHECK_EQ_MSG(mid.style_at_cut.fg.index, std::uint8_t{1}, "style over the cut text");
}

/// Continuation lines inherit styling opened on a previous line, so the planner
/// must accept an initial style and still close it.
void testInheritedStyle() {
    supra_ansi_style initial = supra_ansi_style_default();
    initial.fg.kind = SUPRA_ANSI_COLOR_INDEXED;
    initial.fg.index = 1;
    initial.attrs = SUPRA_ANSI_ATTR_BOLD;

    const std::string input = "continuation";
    supra_ansi_truncation t{};
    supra_ansi_plan_truncate_from(reinterpret_cast<const std::uint8_t*>(input.data()),
                                  input.size(), 6, kNarrow, &initial, &t);

    SUPRA_CHECK_EQ(t.cells, std::size_t{6});
    SUPRA_CHECK_MSG(t.trailer_len > 0, "inherited style needs closing");
    SUPRA_CHECK_MSG(endsDefault(render(input, t)), "inherited style closed");

    // An empty line with an inherited style still needs the trailer, or the
    // style leaks past it.
    supra_ansi_truncation empty{};
    supra_ansi_plan_truncate_from(nullptr, 0, 10, kNarrow, &initial, &empty);
    SUPRA_CHECK_MSG(empty.trailer_len > 0, "empty line still closes an inherited style");
}

void testZeroLimit() {
    const auto t = plan("\x1b[31mtext", 0);
    SUPRA_CHECK_EQ(t.cells, std::size_t{0});
    // The escape is still emitted (it occupies no cells) and therefore still
    // needs closing.
    SUPRA_CHECK_MSG(endsDefault(render("\x1b[31mtext", t)), "zero limit still balanced");
}

void testMeasure() {
    SUPRA_CHECK_EQ(supra_ansi_measure(reinterpret_cast<const std::uint8_t*>("hello"), 5, kNarrow),
                   std::size_t{5});

    const std::string styled = "\x1b[31mhello\x1b[0m";
    SUPRA_CHECK_EQ_MSG(
        supra_ansi_measure(reinterpret_cast<const std::uint8_t*>(styled.data()), styled.size(),
                           kNarrow),
        std::size_t{5}, "escapes contribute nothing");

    const std::string cjk = "\xE4\xB8\xAD\xE6\x96\x87";
    SUPRA_CHECK_EQ(
        supra_ansi_measure(reinterpret_cast<const std::uint8_t*>(cjk.data()), cjk.size(), kNarrow),
        std::size_t{4});

    SUPRA_CHECK_EQ(supra_ansi_measure(nullptr, 0, kNarrow), std::size_t{0});
}

void testStrip() {
    const auto stripped = [](const std::string& input) {
        const auto* bytes = reinterpret_cast<const std::uint8_t*>(input.data());
        const std::size_t needed = supra_ansi_strip(bytes, input.size(), nullptr, 0);
        std::string out(needed, '\0');
        supra_ansi_strip(bytes, input.size(), reinterpret_cast<std::uint8_t*>(out.data()),
                         out.size());
        return out;
    };

    SUPRA_CHECK_STR_EQ(stripped("\x1b[31mred\x1b[0m"), "red", "SGR removed");
    SUPRA_CHECK_STR_EQ(stripped("\x1b]8;;u\x1b\\link\x1b]8;;\x1b\\"), "link", "OSC removed");
    SUPRA_CHECK_STR_EQ(stripped("a\nb\tc"), "a\nb\tc", "tab and newline survive");
    // Other C0 controls can move the cursor, so tool output must not carry them
    // through.
    SUPRA_CHECK_STR_EQ(stripped("a\x07"
                                "b"),
                       "ab", "BEL dropped");
    SUPRA_CHECK_STR_EQ(stripped("\x1b[?25lhidden"), "hidden", "private mode removed");
}

void testNullArguments() {
    supra_ansi_truncation t{};
    supra_ansi_plan_truncate(nullptr, 0, 10, kNarrow, &t);
    SUPRA_CHECK_EQ(t.prefix_len, std::size_t{0});
    SUPRA_CHECK_EQ(t.cells, std::size_t{0});

    // Must not crash on a null out pointer.
    supra_ansi_plan_truncate(nullptr, 0, 10, kNarrow, nullptr);
    SUPRA_CHECK_EQ(supra_ansi_strip(nullptr, 0, nullptr, 0), std::size_t{0});
}

}  // namespace

int main() {
    testPlainText();
    testFitsWhole();
    testEscapesAreFree();
    testTrailerClosesStyle();
    testNeverCutsInsideEscape();
    testHyperlinkClosed();
    testWideClusters();
    testEmojiZwjSequence();
    testNeverExceedsLimit();
    testStyleAtCut();
    testInheritedStyle();
    testZeroLimit();
    testMeasure();
    testStrip();
    testNullArguments();
    return supra::test::finish("ansi_truncate_test");
}
