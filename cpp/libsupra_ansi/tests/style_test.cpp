// SGR folding and serialisation, and the round trip between them.
//
// The round trip is load-bearing: the truncation trailer is generated from a
// folded style, so if serialisation disagreed with folding, the trailer would
// fail to close what the prefix opened and colour would bleed into unrelated
// output.

#include <cstdint>
#include <string>
#include <vector>

#include "supra/ansi.h"
#include "supra/testing.hpp"

namespace {

/// Fold every token in `input` into a style, as a renderer does.
supra_ansi_style fold(const std::string& input) {
    supra_ansi_style style = supra_ansi_style_default();
    supra_ansi_scanner scanner{};
    supra_ansi_token token{};
    const auto* bytes = reinterpret_cast<const std::uint8_t*>(input.data());

    while (supra_ansi_scan(&scanner, bytes, input.size(), SUPRA_ANSI_FINAL, &token) == 1) {
        supra_ansi_style_apply(&style, &token);
        supra_ansi_style_apply_osc(&style, &token);
        if (token.length == 0 && token.kind == SUPRA_ANSI_TOKEN_MALFORMED) {
            break;
        }
    }
    return style;
}

std::string emit(const supra_ansi_style& style) {
    // Two-phase: ask for the length, then write. Callers on the render path
    // allocate exactly once, so a truncated write must still report true size.
    const std::size_t needed = supra_ansi_style_emit(&style, nullptr, 0);
    if (needed == 0) {
        return {};
    }
    std::string out(needed, '\0');
    const std::size_t written =
        supra_ansi_style_emit(&style, reinterpret_cast<std::uint8_t*>(out.data()), out.size());
    SUPRA_CHECK_EQ_MSG(written, needed, "emit length is stable between passes");
    return out;
}

std::string emitReset(const supra_ansi_style& style) {
    const std::size_t needed = supra_ansi_style_emit_reset(&style, nullptr, 0);
    if (needed == 0) {
        return {};
    }
    std::string out(needed, '\0');
    supra_ansi_style_emit_reset(&style, reinterpret_cast<std::uint8_t*>(out.data()), out.size());
    return out;
}

void testDefaultStyle() {
    const supra_ansi_style style = supra_ansi_style_default();
    SUPRA_CHECK_EQ(supra_ansi_style_is_default(&style), 1);
    // A default style emits nothing: a spurious reset per line is measurable
    // waste on the render path.
    SUPRA_CHECK_EQ_MSG(emit(style).size(), std::size_t{0}, "default style emits nothing");
    SUPRA_CHECK_EQ_MSG(emitReset(style).size(), std::size_t{0}, "default needs no reset");
}

void testBasicAttributes() {
    const auto bold = fold("\x1b[1m");
    SUPRA_CHECK(( bold.attrs & SUPRA_ANSI_ATTR_BOLD) != 0);
    SUPRA_CHECK_EQ(supra_ansi_style_is_default(&bold), 0);

    const auto multi = fold("\x1b[1;3;7m");
    SUPRA_CHECK((multi.attrs & SUPRA_ANSI_ATTR_BOLD) != 0);
    SUPRA_CHECK((multi.attrs & SUPRA_ANSI_ATTR_ITALIC) != 0);
    SUPRA_CHECK((multi.attrs & SUPRA_ANSI_ATTR_INVERSE) != 0);

    // SGR 22 clears bold *and* dim: they share one off-code.
    const auto cleared = fold("\x1b[1;2m\x1b[22m");
    SUPRA_CHECK_EQ_MSG((cleared.attrs & SUPRA_ANSI_ATTR_BOLD), 0u, "22 clears bold");
    SUPRA_CHECK_EQ_MSG((cleared.attrs & SUPRA_ANSI_ATTR_DIM), 0u, "22 also clears dim");
}

void testReset() {
    const auto reset = fold("\x1b[1;31;44m\x1b[0m");
    SUPRA_CHECK_EQ_MSG(supra_ansi_style_is_default(&reset), 1, "SGR 0 resets everything");

    // `ESC [ m` with no parameters is also a full reset.
    const auto bare = fold("\x1b[1;31m\x1b[m");
    SUPRA_CHECK_EQ_MSG(supra_ansi_style_is_default(&bare), 1, "bare ESC[m resets");
}

void testBasicColors() {
    const auto red = fold("\x1b[31m");
    SUPRA_CHECK_EQ(red.fg.kind, std::uint8_t{SUPRA_ANSI_COLOR_INDEXED});
    SUPRA_CHECK_EQ(red.fg.index, std::uint8_t{1});

    const auto bright = fold("\x1b[91m");
    SUPRA_CHECK_EQ_MSG(bright.fg.index, std::uint8_t{9}, "bright red is index 9");

    const auto bg = fold("\x1b[44m");
    SUPRA_CHECK_EQ(bg.bg.kind, std::uint8_t{SUPRA_ANSI_COLOR_INDEXED});
    SUPRA_CHECK_EQ(bg.bg.index, std::uint8_t{4});

    const auto defaulted = fold("\x1b[31m\x1b[39m");
    SUPRA_CHECK_EQ_MSG(defaulted.fg.kind, std::uint8_t{SUPRA_ANSI_COLOR_DEFAULT},
                       "39 restores default fg");
}

/// Both extended-colour forms occur in the wild. The colon form carries an
/// almost-always-empty colour-space id, which is why `38:2::255:0:0` has a
/// doubled colon.
void testExtendedColors() {
    const auto indexed = fold("\x1b[38;5;196m");
    SUPRA_CHECK_EQ(indexed.fg.kind, std::uint8_t{SUPRA_ANSI_COLOR_INDEXED});
    SUPRA_CHECK_EQ(indexed.fg.index, std::uint8_t{196});

    const auto rgb = fold("\x1b[38;2;255;128;0m");
    SUPRA_CHECK_EQ(rgb.fg.kind, std::uint8_t{SUPRA_ANSI_COLOR_RGB});
    SUPRA_CHECK_EQ(rgb.fg.r, std::uint8_t{255});
    SUPRA_CHECK_EQ(rgb.fg.g, std::uint8_t{128});
    SUPRA_CHECK_EQ(rgb.fg.b, std::uint8_t{0});

    const auto colon_indexed = fold("\x1b[38:5:196m");
    SUPRA_CHECK_EQ_MSG(colon_indexed.fg.kind, std::uint8_t{SUPRA_ANSI_COLOR_INDEXED},
                       "colon form indexed");
    SUPRA_CHECK_EQ(colon_indexed.fg.index, std::uint8_t{196});

    const auto colon_rgb = fold("\x1b[38:2::255:128:0m");
    SUPRA_CHECK_EQ_MSG(colon_rgb.fg.kind, std::uint8_t{SUPRA_ANSI_COLOR_RGB},
                       "colon form with empty colour-space id");
    SUPRA_CHECK_EQ(colon_rgb.fg.r, std::uint8_t{255});
    SUPRA_CHECK_EQ(colon_rgb.fg.g, std::uint8_t{128});
    SUPRA_CHECK_EQ(colon_rgb.fg.b, std::uint8_t{0});

    const auto bg_rgb = fold("\x1b[48;2;10;20;30m");
    SUPRA_CHECK_EQ(bg_rgb.bg.kind, std::uint8_t{SUPRA_ANSI_COLOR_RGB});
    SUPRA_CHECK_EQ(bg_rgb.bg.r, std::uint8_t{10});
}

void testUnderlineStyles() {
    SUPRA_CHECK_EQ(fold("\x1b[4m").underline, std::uint8_t{SUPRA_ANSI_UNDERLINE_SINGLE});
    SUPRA_CHECK_EQ(fold("\x1b[21m").underline, std::uint8_t{SUPRA_ANSI_UNDERLINE_DOUBLE});
    SUPRA_CHECK_EQ_MSG(fold("\x1b[4:3m").underline, std::uint8_t{SUPRA_ANSI_UNDERLINE_CURLY},
                       "4:3 is curly");
    SUPRA_CHECK_EQ(fold("\x1b[4:4m").underline, std::uint8_t{SUPRA_ANSI_UNDERLINE_DOTTED});
    SUPRA_CHECK_EQ(fold("\x1b[4:5m").underline, std::uint8_t{SUPRA_ANSI_UNDERLINE_DASHED});
    SUPRA_CHECK_EQ_MSG(fold("\x1b[4:0m").underline, std::uint8_t{SUPRA_ANSI_UNDERLINE_NONE},
                       "4:0 is off");
    SUPRA_CHECK_EQ(fold("\x1b[4m\x1b[24m").underline, std::uint8_t{SUPRA_ANSI_UNDERLINE_NONE});
}

void testUnderlineColor() {
    const auto styled = fold("\x1b[58;5;42m");
    SUPRA_CHECK_EQ(styled.underline_color.kind, std::uint8_t{SUPRA_ANSI_COLOR_INDEXED});
    SUPRA_CHECK_EQ(styled.underline_color.index, std::uint8_t{42});

    const auto cleared = fold("\x1b[58;5;42m\x1b[59m");
    SUPRA_CHECK_EQ(cleared.underline_color.kind, std::uint8_t{SUPRA_ANSI_COLOR_DEFAULT});
}

/// An unterminated hyperlink makes every subsequent cell clickable, so the
/// style has to track it separately from SGR - and SGR 0 must not clear it,
/// because only OSC 8 can.
void testHyperlink() {
    const auto opened = fold("\x1b]8;;https://example.com\x1b\\");
    SUPRA_CHECK_EQ_MSG(opened.hyperlink_open, std::uint8_t{1}, "OSC 8 with URI opens");

    const auto closed = fold("\x1b]8;;https://example.com\x1b\\\x1b]8;;\x1b\\");
    SUPRA_CHECK_EQ_MSG(closed.hyperlink_open, std::uint8_t{0}, "OSC 8 with empty URI closes");

    const auto after_sgr_reset = fold("\x1b]8;;u\x1b\\\x1b[0m");
    SUPRA_CHECK_EQ_MSG(after_sgr_reset.hyperlink_open, std::uint8_t{1},
                       "SGR 0 does not close a hyperlink");
}

/// Reset must close a hyperlink before resetting SGR, or the link stays open
/// while its styling disappears.
void testResetIncludesHyperlink() {
    const auto style = fold("\x1b[31m\x1b]8;;u\x1b\\");
    const std::string reset = emitReset(style);
    SUPRA_CHECK_MSG(reset.find("\x1b]8;;\x1b\\") != std::string::npos,
                    "reset closes the hyperlink");
    SUPRA_CHECK_MSG(reset.find("\x1b[0m") != std::string::npos, "reset clears SGR");
    SUPRA_CHECK_MSG(reset.find("\x1b]8;;\x1b\\") < reset.find("\x1b[0m"),
                    "hyperlink closes before SGR reset");
}

/// The property that makes the truncation trailer trustworthy: folding a
/// serialised style must reproduce the style.
void testRoundTrip() {
    const std::string inputs[] = {
        "\x1b[1m",
        "\x1b[1;3;4;7m",
        "\x1b[31m",
        "\x1b[91;44m",
        "\x1b[38;5;196m",
        "\x1b[38;2;255;128;0m",
        "\x1b[48;2;1;2;3m",
        "\x1b[4:3m",
        "\x1b[21m",
        "\x1b[58;5;42m",
        "\x1b[1;38;2;10;20;30;48;5;9;4:3;53m",
        "\x1b[9;3;2m",
    };

    for (const auto& input : inputs) {
        const supra_ansi_style original = fold(input);
        const std::string serialised = emit(original);
        const supra_ansi_style refolded = fold(serialised);

        SUPRA_CHECK_EQ_MSG(refolded.attrs, original.attrs,
                           "attrs survive round trip for " + supra::test::vis(input));
        SUPRA_CHECK_EQ_MSG(refolded.underline, original.underline,
                           "underline survives for " + supra::test::vis(input));
        SUPRA_CHECK_EQ_MSG(refolded.fg.kind, original.fg.kind,
                           "fg kind survives for " + supra::test::vis(input));
        SUPRA_CHECK_EQ_MSG(refolded.bg.kind, original.bg.kind,
                           "bg kind survives for " + supra::test::vis(input));

        if (original.fg.kind == SUPRA_ANSI_COLOR_INDEXED) {
            SUPRA_CHECK_EQ_MSG(refolded.fg.index, original.fg.index, "fg index survives");
        }
        if (original.fg.kind == SUPRA_ANSI_COLOR_RGB) {
            SUPRA_CHECK_EQ_MSG(refolded.fg.r, original.fg.r, "fg r survives");
            SUPRA_CHECK_EQ_MSG(refolded.fg.g, original.fg.g, "fg g survives");
            SUPRA_CHECK_EQ_MSG(refolded.fg.b, original.fg.b, "fg b survives");
        }
        if (original.underline_color.kind == SUPRA_ANSI_COLOR_INDEXED) {
            SUPRA_CHECK_EQ_MSG(refolded.underline_color.index, original.underline_color.index,
                               "underline colour survives");
        }
    }
}

/// The two-phase length protocol: a short buffer writes nothing and reports the
/// true size, so a caller can allocate once and retry.
void testBufferSizing() {
    const auto style = fold("\x1b[1;38;2;255;128;0m");
    const std::size_t needed = supra_ansi_style_emit(&style, nullptr, 0);
    SUPRA_CHECK(needed > 0);

    std::vector<std::uint8_t> small(needed - 1, 0xAA);
    const std::size_t reported = supra_ansi_style_emit(&style, small.data(), small.size());
    SUPRA_CHECK_EQ_MSG(reported, needed, "short buffer still reports the true length");

    std::vector<std::uint8_t> exact(needed, 0);
    SUPRA_CHECK_EQ(supra_ansi_style_emit(&style, exact.data(), exact.size()), needed);
}

/// Non-SGR tokens must be ignored, so a caller can pass every token
/// unconditionally without filtering.
void testIgnoresNonSgr() {
    supra_ansi_style style = supra_ansi_style_default();

    supra_ansi_token cursor{};
    cursor.kind = SUPRA_ANSI_TOKEN_CSI;
    cursor.final_byte = 'H';
    cursor.param_count = 2;
    cursor.params[0] = 5;
    cursor.params[1] = 10;
    SUPRA_CHECK_EQ_MSG(supra_ansi_style_apply(&style, &cursor), 0, "CSI H is not SGR");
    SUPRA_CHECK_EQ(supra_ansi_style_is_default(&style), 1);

    // `ESC [ ? 25 m` has an intermediate, so it is not SGR despite the final
    // byte.
    supra_ansi_token private_mode{};
    private_mode.kind = SUPRA_ANSI_TOKEN_CSI;
    private_mode.final_byte = 'm';
    private_mode.intermediate_count = 1;
    private_mode.intermediates[0] = '?';
    private_mode.param_count = 1;
    private_mode.params[0] = 1;
    SUPRA_CHECK_EQ_MSG(supra_ansi_style_apply(&style, &private_mode), 0,
                       "intermediate disqualifies SGR");
    SUPRA_CHECK_EQ(supra_ansi_style_is_default(&style), 1);
}

void testUnknownParamsIgnored() {
    // Terminals keep adding parameters; a strict parser would discard the whole
    // sequence over one unrecognised value.
    const auto style = fold("\x1b[1;999;31m");
    SUPRA_CHECK((style.attrs & SUPRA_ANSI_ATTR_BOLD) != 0);
    SUPRA_CHECK_EQ_MSG(style.fg.index, std::uint8_t{1}, "known parameters still applied");
}

void testNullArguments() {
    SUPRA_CHECK_EQ(supra_ansi_style_is_default(nullptr), 1);
    SUPRA_CHECK_EQ(supra_ansi_style_apply(nullptr, nullptr), 0);
    SUPRA_CHECK_EQ(supra_ansi_style_apply_osc(nullptr, nullptr), 0);
    SUPRA_CHECK_EQ(supra_ansi_style_emit(nullptr, nullptr, 0), std::size_t{0});
    SUPRA_CHECK_EQ(supra_ansi_style_emit_reset(nullptr, nullptr, 0), std::size_t{0});
}

}  // namespace

int main() {
    testDefaultStyle();
    testBasicAttributes();
    testReset();
    testBasicColors();
    testExtendedColors();
    testUnderlineStyles();
    testUnderlineColor();
    testHyperlink();
    testResetIncludesHyperlink();
    testRoundTrip();
    testBufferSizing();
    testIgnoresNonSgr();
    testUnknownParamsIgnored();
    testNullArguments();
    return supra::test::finish("style_test");
}
