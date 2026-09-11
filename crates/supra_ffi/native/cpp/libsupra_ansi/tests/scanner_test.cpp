// Scanner: the grammar cases a regex-based matcher gets wrong.
//
// Each group here corresponds to a real failure mode in terminal output, not to
// a hypothetical. The comment on each says which.

#include <cstdint>
#include <string>
#include <vector>

#include "supra/ansi.h"
#include "supra/testing.hpp"

namespace {

using supra::test::vis;

struct Token {
    supra_ansi_token_kind kind;
    std::size_t offset;
    std::size_t length;
    std::uint8_t final_byte;
    std::vector<std::int32_t> params;
    std::string payload;
    std::uint8_t intermediate_count;
    std::uint8_t intermediates[2];
};

/// Scan a whole buffer in one call, as every non-streaming caller does.
std::vector<Token> scanAll(const std::string& input) {
    supra_ansi_scanner scanner{};
    supra_ansi_token raw{};
    std::vector<Token> out;

    const auto* bytes = reinterpret_cast<const std::uint8_t*>(input.data());
    while (supra_ansi_scan(&scanner, bytes, input.size(), SUPRA_ANSI_FINAL, &raw) == 1) {
        Token token{};
        token.kind = static_cast<supra_ansi_token_kind>(raw.kind);
        token.offset = raw.offset;
        token.length = raw.length;
        token.final_byte = raw.final_byte;
        token.intermediate_count = raw.intermediate_count;
        token.intermediates[0] = raw.intermediates[0];
        token.intermediates[1] = raw.intermediates[1];
        for (std::size_t i = 0; i < raw.param_count; ++i) {
            token.params.push_back(raw.params[i]);
        }
        token.payload.assign(reinterpret_cast<const char*>(raw.payload), raw.payload_len);
        out.push_back(token);

        if (raw.length == 0 && token.kind == SUPRA_ANSI_TOKEN_MALFORMED) {
            break;  // Terminal malformed report; no further progress possible.
        }
    }
    return out;
}

std::string text(const std::string& input, const Token& token) {
    return input.substr(token.offset, token.length);
}

void testPlainText() {
    const auto tokens = scanAll("hello world");
    SUPRA_CHECK_EQ(tokens.size(), std::size_t{1});
    SUPRA_CHECK_EQ(tokens[0].kind, SUPRA_ANSI_TOKEN_TEXT);
    SUPRA_CHECK_EQ(tokens[0].length, std::size_t{11});
}

void testSimpleSgr() {
    const auto tokens = scanAll("\x1b[31mred\x1b[0m");
    SUPRA_CHECK_EQ(tokens.size(), std::size_t{3});

    SUPRA_CHECK_EQ(tokens[0].kind, SUPRA_ANSI_TOKEN_CSI);
    SUPRA_CHECK_EQ(tokens[0].final_byte, std::uint8_t{'m'});
    SUPRA_CHECK_EQ(tokens[0].params.size(), std::size_t{1});
    SUPRA_CHECK_EQ(tokens[0].params[0], 31);

    SUPRA_CHECK_EQ(tokens[1].kind, SUPRA_ANSI_TOKEN_TEXT);
    SUPRA_CHECK_STR_EQ(text("\x1b[31mred\x1b[0m", tokens[1]), "red", "text run");

    SUPRA_CHECK_EQ(tokens[2].params[0], 0);
}

/// An omitted parameter is not zero. `ESC [ m` is a bare reset; the distinction
/// changes the meaning of several sequences, so the scanner reports -1 rather
/// than substituting a default.
void testOmittedParams() {
    const auto bare = scanAll("\x1b[m");
    SUPRA_CHECK_EQ(bare.size(), std::size_t{1});
    SUPRA_CHECK_EQ_MSG(bare[0].params.size(), std::size_t{1}, "one omitted parameter");
    SUPRA_CHECK_EQ_MSG(bare[0].params[0], -1, "omitted reported as -1, not 0");

    const auto middle = scanAll("\x1b[1;;5m");
    SUPRA_CHECK_EQ(middle[0].params.size(), std::size_t{3});
    SUPRA_CHECK_EQ(middle[0].params[0], 1);
    SUPRA_CHECK_EQ_MSG(middle[0].params[1], -1, "middle parameter omitted");
    SUPRA_CHECK_EQ(middle[0].params[2], 5);
}

/// Sub-parameters use `:`, not `;`. Splitting only on `;` reads
/// `38:2::255:0:0` as one enormous parameter and renders the wrong colour.
void testSubparams() {
    const auto curly = scanAll("\x1b[4:3m");
    SUPRA_CHECK_EQ(curly.size(), std::size_t{1});
    SUPRA_CHECK_EQ(curly[0].params.size(), std::size_t{1});
    SUPRA_CHECK_EQ_MSG(curly[0].params[0], 4, "parameter is 4, sub-parameter separate");

    const auto rgb = scanAll("\x1b[38:2::255:0:0m");
    SUPRA_CHECK_EQ(rgb.size(), std::size_t{1});
    SUPRA_CHECK_EQ_MSG(rgb[0].params.size(), std::size_t{1},
                       "colon form is one parameter with sub-parameters");
    SUPRA_CHECK_EQ(rgb[0].params[0], 38);
}

/// Both `ESC \` and BEL terminate an OSC. A parser knowing only ST runs past the
/// end of a hyperlink and consumes the text after it.
void testOscTerminators() {
    const auto with_st = scanAll("\x1b]0;title\x1b\\after");
    SUPRA_CHECK_EQ(with_st.size(), std::size_t{2});
    SUPRA_CHECK_EQ(with_st[0].kind, SUPRA_ANSI_TOKEN_OSC);
    SUPRA_CHECK_STR_EQ(with_st[0].payload, "0;title", "OSC payload with ST");
    SUPRA_CHECK_EQ(with_st[1].kind, SUPRA_ANSI_TOKEN_TEXT);
    SUPRA_CHECK_STR_EQ(text("\x1b]0;title\x1b\\after", with_st[1]), "after", "text after ST");

    const auto with_bel = scanAll("\x1b]0;title\x07" "after");
    SUPRA_CHECK_EQ_MSG(with_bel.size(), std::size_t{2}, "BEL terminates OSC");
    SUPRA_CHECK_EQ(with_bel[0].kind, SUPRA_ANSI_TOKEN_OSC);
    SUPRA_CHECK_STR_EQ(with_bel[0].payload, "0;title", "OSC payload with BEL");
    SUPRA_CHECK_STR_EQ(text("\x1b]0;title\x07" "after", with_bel[1]), "after", "text after BEL");
}

void testOsc8Hyperlink() {
    const std::string input = "\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\";
    const auto tokens = scanAll(input);
    SUPRA_CHECK_EQ(tokens.size(), std::size_t{3});
    SUPRA_CHECK_EQ(tokens[0].kind, SUPRA_ANSI_TOKEN_OSC);
    SUPRA_CHECK_STR_EQ(tokens[0].payload, "8;;https://example.com", "hyperlink open");
    SUPRA_CHECK_STR_EQ(text(input, tokens[1]), "link", "link text");
    SUPRA_CHECK_STR_EQ(tokens[2].payload, "8;;", "hyperlink close");
}

/// 8-bit C1 controls carry no ESC byte, so `\x1b\[` never matches even though
/// 0x9B *is* CSI. Terminals emit these, and tmux passes them through.
void testC1Controls() {
    const std::string csi = "\x9b"
                            "31m";
    const auto tokens = scanAll(csi);
    SUPRA_CHECK_EQ_MSG(tokens.size(), std::size_t{1}, "0x9B is CSI");
    SUPRA_CHECK_EQ(tokens[0].kind, SUPRA_ANSI_TOKEN_CSI);
    SUPRA_CHECK_EQ(tokens[0].final_byte, std::uint8_t{'m'});
    SUPRA_CHECK_EQ(tokens[0].params[0], 31);

    const std::string osc = "\x9d"
                            "0;t\x9c";
    const auto osc_tokens = scanAll(osc);
    SUPRA_CHECK_EQ_MSG(osc_tokens.size(), std::size_t{1}, "0x9D is OSC, 0x9C is ST");
    SUPRA_CHECK_EQ(osc_tokens[0].kind, SUPRA_ANSI_TOKEN_OSC);
    SUPRA_CHECK_STR_EQ(osc_tokens[0].payload, "0;t", "C1 OSC payload");
}

/// A DCS payload may contain bytes that look like the start of other sequences.
/// Only ST ends it.
void testDcs() {
    const std::string input = "\x1bP1$r0m\x1b\\text";
    const auto tokens = scanAll(input);
    SUPRA_CHECK_EQ(tokens.size(), std::size_t{2});
    SUPRA_CHECK_EQ(tokens[0].kind, SUPRA_ANSI_TOKEN_DCS);
    SUPRA_CHECK_STR_EQ(text(input, tokens[1]), "text", "text after DCS");
}

void testApc() {
    const std::string input = "\x1b_Gf=100\x1b\\rest";
    const auto tokens = scanAll(input);
    SUPRA_CHECK_EQ(tokens.size(), std::size_t{2});
    SUPRA_CHECK_EQ(tokens[0].kind, SUPRA_ANSI_TOKEN_APC);
    SUPRA_CHECK_STR_EQ(tokens[0].payload, "Gf=100", "APC payload");
    SUPRA_CHECK_STR_EQ(text(input, tokens[1]), "rest", "text after APC");
}

/// The C1 control range (0x80..0x9F) is a **subset** of the UTF-8 continuation
/// range (0x80..0xBF), so a byte-wise C1 test tears multi-byte characters apart.
///
/// This is not hypothetical. U+6587 encodes as `E6 96 87`, and 0x96 is the C1
/// code for START OF GUARDED AREA. U+1F600 encodes as `F0 9F 98 80`, containing
/// three bytes in the C1 range - including 0x9B, which is CSI. A naive scanner
/// reads a grinning face as text, then a spurious CSI sequence that swallows
/// whatever follows.
///
/// The disambiguation has to be positional: no byte in 0x80..0xBF is a valid
/// UTF-8 lead, so such a byte is a C1 control exactly when it falls on a scalar
/// boundary.
void testUtf8ContainingC1Bytes() {
    struct Case {
        const char* utf8;
        const char* name;
    };

    const Case cases[] = {
        {"\xE6\x96\x87", "U+6587, second byte 0x96 = C1 SGA"},
        {"\xE2\x80\x8D", "U+200D ZWJ, second byte 0x80 = C1 PAD"},
        {"\xF0\x9F\x98\x80", "U+1F600, contains 0x9F 0x98 0x80"},
        {"\xF0\x9F\x91\xA8", "U+1F468, second byte 0x9F = C1 APC"},
        {"\xC2\x9B", "U+009B itself, encoded as UTF-8"},
        {"\xE4\xB8\xAD\xE6\x96\x87", "two CJK characters"},
    };

    for (const auto& test_case : cases) {
        const std::string input = test_case.utf8;
        const auto tokens = scanAll(input);

        SUPRA_CHECK_EQ_MSG(tokens.size(), std::size_t{1},
                           std::string(test_case.name) + " is one text token");
        if (!tokens.empty()) {
            SUPRA_CHECK_EQ_MSG(tokens[0].kind, SUPRA_ANSI_TOKEN_TEXT,
                               std::string(test_case.name) + " is text, not a control");
            SUPRA_CHECK_EQ_MSG(tokens[0].length, input.size(),
                               std::string(test_case.name) + " consumed whole");
        }
    }

    // A real C1 introducer immediately after a multi-byte character must still be
    // recognised: the expectation counter has to reach zero at the boundary.
    const std::string mixed = "\xE6\x96\x87\x9b"
                              "31m";
    const auto tokens = scanAll(mixed);
    SUPRA_CHECK_EQ_MSG(tokens.size(), std::size_t{2}, "CJK then a genuine C1 CSI");
    SUPRA_CHECK_EQ(tokens[0].kind, SUPRA_ANSI_TOKEN_TEXT);
    SUPRA_CHECK_EQ_MSG(tokens[0].length, std::size_t{3}, "text stops at the CJK boundary");
    SUPRA_CHECK_EQ_MSG(tokens[1].kind, SUPRA_ANSI_TOKEN_CSI, "0x9B after a character is CSI");
    SUPRA_CHECK_EQ(tokens[1].params[0], 31);
}

void testPrivateMode() {
    const auto tokens = scanAll("\x1b[?25l");
    SUPRA_CHECK_EQ(tokens.size(), std::size_t{1});
    SUPRA_CHECK_EQ(tokens[0].kind, SUPRA_ANSI_TOKEN_CSI);
    SUPRA_CHECK_EQ(tokens[0].final_byte, std::uint8_t{'l'});
    SUPRA_CHECK_EQ_MSG(tokens[0].intermediate_count, std::uint8_t{1}, "? recorded");
    SUPRA_CHECK_EQ(tokens[0].intermediates[0], std::uint8_t{'?'});
    SUPRA_CHECK_EQ(tokens[0].params[0], 25);
}

void testEscSequences() {
    const auto charset = scanAll("\x1b(B");
    SUPRA_CHECK_EQ(charset.size(), std::size_t{1});
    SUPRA_CHECK_EQ(charset[0].kind, SUPRA_ANSI_TOKEN_ESC);
    SUPRA_CHECK_EQ(charset[0].final_byte, std::uint8_t{'B'});

    const auto save = scanAll("\x1b""7");
    SUPRA_CHECK_EQ(save.size(), std::size_t{1});
    SUPRA_CHECK_EQ(save[0].kind, SUPRA_ANSI_TOKEN_ESC);
    SUPRA_CHECK_EQ(save[0].final_byte, std::uint8_t{'7'});
}

/// CAN and SUB abort a sequence in progress. A parser ignoring them keeps
/// accumulating and swallows the text that follows.
void testCancel() {
    const std::string input = "\x1b[31\x18text";
    const auto tokens = scanAll(input);
    SUPRA_CHECK(tokens.size() >= 2);
    SUPRA_CHECK_EQ_MSG(tokens[0].kind, SUPRA_ANSI_TOKEN_MALFORMED, "CAN aborts the sequence");
    SUPRA_CHECK_EQ_MSG(tokens[1].kind, SUPRA_ANSI_TOKEN_TEXT, "text after CAN is recovered");
    SUPRA_CHECK_STR_EQ(text(input, tokens[1]), "text", "recovered text");
}

void testControls() {
    const std::string input = "a\nb\tc";
    const auto tokens = scanAll(input);
    // Tab and newline are text, so this is a single run.
    SUPRA_CHECK_EQ_MSG(tokens.size(), std::size_t{1}, "tab and newline are text");
    SUPRA_CHECK_EQ(tokens[0].kind, SUPRA_ANSI_TOKEN_TEXT);

    const std::string with_bel = "a\x07"
                                 "b";
    const auto bel_tokens = scanAll(with_bel);
    SUPRA_CHECK_EQ_MSG(bel_tokens.size(), std::size_t{3}, "BEL splits the text run");
    SUPRA_CHECK_EQ(bel_tokens[1].kind, SUPRA_ANSI_TOKEN_CONTROL);
    SUPRA_CHECK_EQ(bel_tokens[1].final_byte, std::uint8_t{0x07});
}

/// Unterminated input at end of stream must be MALFORMED, never PARTIAL: a
/// caller must not be left waiting for a terminator that will never arrive.
void testUnterminatedAtEof() {
    const auto csi = scanAll("\x1b[31");
    SUPRA_CHECK(!csi.empty());
    SUPRA_CHECK_EQ_MSG(csi[0].kind, SUPRA_ANSI_TOKEN_MALFORMED, "unterminated CSI at EOF");

    const auto osc = scanAll("\x1b]0;title");
    SUPRA_CHECK(!osc.empty());
    SUPRA_CHECK_EQ_MSG(osc[0].kind, SUPRA_ANSI_TOKEN_MALFORMED, "unterminated OSC at EOF");

    const auto esc = scanAll("\x1b");
    SUPRA_CHECK(!esc.empty());
    SUPRA_CHECK_EQ_MSG(esc[0].kind, SUPRA_ANSI_TOKEN_MALFORMED, "lone ESC at EOF");
}

/// Parameter overflow must be consumed, not rejected. Saturation keeps the
/// sequence intact without an integer overflow.
void testParamSaturation() {
    const auto tokens = scanAll("\x1b[99999999999999999999m");
    SUPRA_CHECK_EQ(tokens.size(), std::size_t{1});
    SUPRA_CHECK_EQ(tokens[0].kind, SUPRA_ANSI_TOKEN_CSI);
    SUPRA_CHECK_EQ_MSG(tokens[0].params[0], 65535, "saturated, not overflowed");
}

void testTooManyParams() {
    std::string input = "\x1b[";
    for (int i = 0; i < 40; ++i) {
        input += "1;";
    }
    input += "m";

    const auto tokens = scanAll(input);
    SUPRA_CHECK_EQ_MSG(tokens.size(), std::size_t{1}, "over-long sequence still consumed whole");
    SUPRA_CHECK_EQ(tokens[0].kind, SUPRA_ANSI_TOKEN_CSI);
    SUPRA_CHECK_EQ_MSG(tokens[0].params.size(), std::size_t{SUPRA_ANSI_MAX_PARAMS},
                       "parameters capped");
}

/// Total over the byte space: every single byte must produce a token and
/// terminate. A byte that stalled the scanner would hang the renderer.
void testEveryByteTerminates() {
    for (int byte = 0; byte <= 0xFF; ++byte) {
        const std::string input(1, static_cast<char>(byte));
        supra_ansi_scanner scanner{};
        supra_ansi_token token{};
        int iterations = 0;
        while (supra_ansi_scan(&scanner, reinterpret_cast<const std::uint8_t*>(input.data()),
                              input.size(), SUPRA_ANSI_FINAL, &token) == 1) {
            if (++iterations > 8) {
                break;
            }
        }
        SUPRA_CHECK_MSG(iterations >= 1 && iterations <= 8,
                        "byte 0x" + std::to_string(byte) + " terminates");
    }
}

/// Offsets and lengths must tile the input exactly: no gaps, no overlaps.
/// A caller reconstructing output from token spans depends on it.
void testTokensTileInput() {
    const std::string inputs[] = {
        "\x1b[31mred\x1b[0m plain \x1b]8;;u\x1b\\link\x1b]8;;\x1b\\",
        "\x9b"
        "1m\x1bP1$r\x1b\\\x1b(B",
        "text\x07more\x1b[?25l",
    };

    for (const auto& input : inputs) {
        const auto tokens = scanAll(input);
        std::size_t expected = 0;
        for (const auto& token : tokens) {
            if (token.length == 0) {
                continue;
            }
            SUPRA_CHECK_EQ_MSG(token.offset, expected, "no gap in " + vis(input));
            expected += token.length;
        }
        SUPRA_CHECK_EQ_MSG(expected, input.size(), "tokens cover " + vis(input));
    }
}

void testNullArguments() {
    supra_ansi_token token{};
    supra_ansi_scanner scanner{};
    SUPRA_CHECK_EQ(supra_ansi_scan(nullptr, nullptr, 0, SUPRA_ANSI_FINAL, &token), 0);
    SUPRA_CHECK_EQ(supra_ansi_scan(&scanner, nullptr, 0, SUPRA_ANSI_MORE, nullptr), 0);
    SUPRA_CHECK_EQ(supra_ansi_scan(&scanner, nullptr, 0, SUPRA_ANSI_MORE, &token), 0);
    SUPRA_CHECK_EQ(supra_ansi_scanner_pending(nullptr), 0);
}

}  // namespace

int main() {
    testPlainText();
    testSimpleSgr();
    testOmittedParams();
    testSubparams();
    testOscTerminators();
    testOsc8Hyperlink();
    testC1Controls();
    testDcs();
    testApc();
    testUtf8ContainingC1Bytes();
    testPrivateMode();
    testEscSequences();
    testCancel();
    testControls();
    testUnterminatedAtEof();
    testParamSaturation();
    testTooManyParams();
    testEveryByteTerminates();
    testTokensTileInput();
    testNullArguments();
    return supra::test::finish("scanner_test");
}
