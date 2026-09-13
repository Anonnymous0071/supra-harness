// Streaming: escape sequences straddling a chunk boundary.
//
// This is the case a stateless matcher cannot handle, and the reason the scanner
// carries state at all. Shell output arrives in chunks; a sequence split across
// two reads must produce the same tokens as the same bytes delivered whole.
//
// The failure it prevents is specific: a progress line or a coloured error
// arriving in two pieces gets mangled at exactly the moment it is written.

#include <cstdint>
#include <string>
#include <vector>

#include "supra/ansi.h"
#include "supra/testing.hpp"

namespace {

struct Observed {
    supra_ansi_token_kind kind;
    std::uint8_t final_byte;
    std::vector<std::int32_t> params;
    std::string payload;
    std::string text;
};

/// Feed `input` in fixed-size chunks and collect the completed tokens.
///
/// PARTIAL tokens are bookkeeping rather than content: they report bytes
/// consumed so far so a caller can carry them, and they are not part of the
/// logical token stream.
std::vector<Observed> scanChunked(const std::string& input, std::size_t chunk_size) {
    supra_ansi_scanner scanner{};
    supra_ansi_token token{};
    std::vector<Observed> out;

    std::size_t offset = 0;
    while (offset < input.size()) {
        const std::size_t len = std::min(chunk_size, input.size() - offset);
        const bool last = offset + len >= input.size();
        const auto* bytes = reinterpret_cast<const std::uint8_t*>(input.data() + offset);
        const auto eof = last ? SUPRA_ANSI_FINAL : SUPRA_ANSI_MORE;

        while (supra_ansi_scan(&scanner, bytes, len, eof, &token) == 1) {
            if (token.kind == SUPRA_ANSI_TOKEN_PARTIAL) {
                continue;
            }
            Observed obs{};
            obs.kind = static_cast<supra_ansi_token_kind>(token.kind);
            obs.final_byte = token.final_byte;
            for (std::size_t i = 0; i < token.param_count; ++i) {
                obs.params.push_back(token.params[i]);
            }
            obs.payload.assign(reinterpret_cast<const char*>(token.payload), token.payload_len);
            if (token.kind == SUPRA_ANSI_TOKEN_TEXT) {
                obs.text.assign(input.data() + offset + token.offset, token.length);
            }
            out.push_back(obs);

            if (token.length == 0 && token.kind == SUPRA_ANSI_TOKEN_MALFORMED) {
                break;
            }
        }

        offset += len;
    }

    return out;
}

/// Text runs split across chunks arrive as separate tokens, so comparison is on
/// the concatenation rather than token-by-token.
std::string textOf(const std::vector<Observed>& tokens) {
    std::string out;
    for (const auto& token : tokens) {
        if (token.kind == SUPRA_ANSI_TOKEN_TEXT) {
            out += token.text;
        }
    }
    return out;
}

std::vector<Observed> nonText(const std::vector<Observed>& tokens) {
    std::vector<Observed> out;
    for (const auto& token : tokens) {
        if (token.kind != SUPRA_ANSI_TOKEN_TEXT) {
            out.push_back(token);
        }
    }
    return out;
}

/// The core property: chunking must not change the outcome, at any chunk size.
///
/// Every size from 1 upward is exercised, because the interesting boundaries are
/// exactly the ones that fall inside a sequence, and which those are depends on
/// the input.
void testChunkingIsInvariant() {
    const std::string inputs[] = {
        "\x1b[31mred\x1b[0m",
        "\x1b[38;2;255;128;0mtruecolor\x1b[0m",
        "\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\",
        "\x1b]0;window title\x07text",
        "\x1bP1$r0m\x1b\\after",
        "\x9b"
        "1;31mC1 introducer",
        "plain \x1b[1mbold\x1b[0m plain",
        "\x1b[4:3mcurly\x1b[0m",
        "\x1b_Gf=100\x1b\\apc",
        "a\x1b[Kb\x1b[2Kc",
    };

    for (const auto& input : inputs) {
        const auto whole = scanChunked(input, input.size());
        const std::string whole_text = textOf(whole);
        const auto whole_tokens = nonText(whole);

        for (std::size_t chunk = 1; chunk <= input.size(); ++chunk) {
            const auto chunked = scanChunked(input, chunk);

            SUPRA_CHECK_STR_EQ(textOf(chunked), whole_text,
                               "text identical at chunk " + std::to_string(chunk) + " for " +
                                   supra::test::vis(input));

            const auto chunked_tokens = nonText(chunked);
            SUPRA_CHECK_EQ_MSG(chunked_tokens.size(), whole_tokens.size(),
                               "same token count at chunk " + std::to_string(chunk) + " for " +
                                   supra::test::vis(input));

            const std::size_t common = std::min(chunked_tokens.size(), whole_tokens.size());
            for (std::size_t i = 0; i < common; ++i) {
                SUPRA_CHECK_EQ_MSG(chunked_tokens[i].kind, whole_tokens[i].kind,
                                   "token " + std::to_string(i) + " kind at chunk " +
                                       std::to_string(chunk));
                SUPRA_CHECK_EQ_MSG(chunked_tokens[i].final_byte, whole_tokens[i].final_byte,
                                   "token " + std::to_string(i) + " final byte at chunk " +
                                       std::to_string(chunk));
                SUPRA_CHECK_MSG(chunked_tokens[i].params == whole_tokens[i].params,
                                "token " + std::to_string(i) + " params at chunk " +
                                    std::to_string(chunk));
                SUPRA_CHECK_STR_EQ(chunked_tokens[i].payload, whole_tokens[i].payload,
                                   "token " + std::to_string(i) + " payload at chunk " +
                                       std::to_string(chunk));
            }
        }
    }
}

/// Byte-at-a-time is the worst case: every sequence is split at every possible
/// point simultaneously.
void testSingleByteChunks() {
    const std::string input = "\x1b[1;38;2;255;128;0mstyled \xE4\xB8\xAD text\x1b[0m";
    const auto chunked = scanChunked(input, 1);
    const auto whole = scanChunked(input, input.size());

    SUPRA_CHECK_STR_EQ(textOf(chunked), textOf(whole), "text survives byte-at-a-time");
    SUPRA_CHECK_EQ(nonText(chunked).size(), nonText(whole).size());
}

/// A chunk boundary landing between a UTF-8 lead byte and a continuation byte
/// that is also in the C1 control range.
///
/// This is the only path where the *entry* condition of the text scan must know
/// it is mid-character. Within one buffer the inner loop carries the
/// expectation, so whole-buffer tests cannot reach it; only a split between the
/// lead and a continuation byte in 0x80..0x9F does.
///
/// U+6587 encodes as `E6 96 87`. Split after `E6`, the next chunk opens with
/// 0x96 - also the C1 code for START OF GUARDED AREA. Without the carried
/// expectation the scanner reads that as a control and tears the character apart.
///
/// Many CJK characters cannot expose this: U+4E2D is `E4 B8 AD`, whose
/// continuation bytes both exceed 0x9F. The cases below are chosen for having
/// continuation bytes inside the C1 range.
void testChunkSplitBeforeC1RangeContinuation() {
    struct Case {
        const char* utf8;
        const char* name;
    };

    const Case cases[] = {
        {"\xE6\x96\x87", "U+6587, continuation 0x96 = C1 SGA"},
        {"\xE2\x80\x8D", "U+200D ZWJ, continuation 0x80 = C1 PAD"},
        {"\xF0\x9F\x98\x80", "U+1F600, continuations 0x9F 0x98 0x80"},
        {"\xF0\x9F\x91\xA8", "U+1F468, continuation 0x9F = C1 APC"},
        {"\xE2\x88\xB5", "U+2235 BECAUSE, continuation 0x88 = C1 HTS"},
    };

    for (const auto& test_case : cases) {
        const std::string input = test_case.utf8;

        // Every split point: which byte lands first depends on the offset.
        for (std::size_t split = 1; split < input.size(); ++split) {
            supra_ansi_scanner scanner{};
            supra_ansi_token token{};
            std::string recovered;
            std::size_t control_tokens = 0;

            const std::string chunks[] = {input.substr(0, split), input.substr(split)};
            for (std::size_t c = 0; c < 2; ++c) {
                const auto* bytes = reinterpret_cast<const std::uint8_t*>(chunks[c].data());
                const auto eof = c == 1 ? SUPRA_ANSI_FINAL : SUPRA_ANSI_MORE;
                while (supra_ansi_scan(&scanner, bytes, chunks[c].size(), eof, &token) == 1) {
                    if (token.kind == SUPRA_ANSI_TOKEN_TEXT) {
                        recovered.append(chunks[c].data() + token.offset, token.length);
                    } else if (token.kind != SUPRA_ANSI_TOKEN_PARTIAL) {
                        ++control_tokens;
                    }
                }
            }

            SUPRA_CHECK_STR_EQ(recovered, input,
                               std::string(test_case.name) + " intact at split " +
                                   std::to_string(split));
            SUPRA_CHECK_EQ_MSG(control_tokens, std::size_t{0},
                               std::string(test_case.name) + " yields no control token at split " +
                                   std::to_string(split));
        }
    }

    // The counterpart: a genuine C1 introducer arriving in its own chunk, right
    // after a complete character, must still be recognised. The expectation has
    // to have reached zero at the boundary.
    supra_ansi_scanner scanner{};
    supra_ansi_token token{};
    const std::string first = "\xE6\x96\x87";
    const std::string second = "\x9b"
                               "31m";

    bool saw_text = false;
    bool saw_csi = false;
    std::int32_t csi_param = -1;

    const std::string chunks[] = {first, second};
    for (std::size_t c = 0; c < 2; ++c) {
        const auto* bytes = reinterpret_cast<const std::uint8_t*>(chunks[c].data());
        const auto eof = c == 1 ? SUPRA_ANSI_FINAL : SUPRA_ANSI_MORE;
        while (supra_ansi_scan(&scanner, bytes, chunks[c].size(), eof, &token) == 1) {
            if (token.kind == SUPRA_ANSI_TOKEN_TEXT) {
                saw_text = true;
            } else if (token.kind == SUPRA_ANSI_TOKEN_CSI) {
                saw_csi = true;
                csi_param = token.param_count > 0 ? token.params[0] : -1;
            }
        }
    }

    SUPRA_CHECK_MSG(saw_text, "the character before the boundary is text");
    SUPRA_CHECK_MSG(saw_csi, "0x9B in the next chunk is still recognised as CSI");
    SUPRA_CHECK_EQ_MSG(csi_param, 31, "C1 CSI parameters parsed across the boundary");
}

/// The scanner must report that it is holding an incomplete sequence, so a
/// caller can distinguish "no more tokens" from "waiting for more input".
void testPendingReported() {
    supra_ansi_scanner scanner{};
    supra_ansi_token token{};

    const std::string first = "\x1b[31";
    const auto* bytes = reinterpret_cast<const std::uint8_t*>(first.data());
    while (supra_ansi_scan(&scanner, bytes, first.size(), SUPRA_ANSI_MORE, &token) == 1) {
        // Drain; the sequence is incomplete.
    }
    SUPRA_CHECK_EQ_MSG(supra_ansi_scanner_pending(&scanner), 1, "pending mid-sequence");

    const std::string second = "m";
    const auto* rest = reinterpret_cast<const std::uint8_t*>(second.data());
    SUPRA_CHECK_EQ(supra_ansi_scan(&scanner, rest, second.size(), SUPRA_ANSI_FINAL, &token), 1);
    SUPRA_CHECK_EQ_MSG(token.kind, SUPRA_ANSI_TOKEN_CSI, "sequence completes across the boundary");
    SUPRA_CHECK_EQ(token.params[0], 31);
    SUPRA_CHECK_EQ_MSG(supra_ansi_scanner_pending(&scanner), 0, "no longer pending");
}

/// A stream that simply stops mid-sequence must not leave the caller waiting
/// forever: FINAL converts the pending state into MALFORMED.
void testStreamEndsMidSequence() {
    supra_ansi_scanner scanner{};
    supra_ansi_token token{};

    const std::string chunk = "text\x1b[31";
    const auto* bytes = reinterpret_cast<const std::uint8_t*>(chunk.data());

    bool saw_text = false;
    while (supra_ansi_scan(&scanner, bytes, chunk.size(), SUPRA_ANSI_MORE, &token) == 1) {
        if (token.kind == SUPRA_ANSI_TOKEN_TEXT) {
            saw_text = true;
        }
    }
    SUPRA_CHECK_MSG(saw_text, "text before the split is delivered immediately");
    SUPRA_CHECK_EQ(supra_ansi_scanner_pending(&scanner), 1);

    // Nothing more arrives. FINAL with an empty buffer must resolve the pending
    // state rather than stall.
    SUPRA_CHECK_EQ_MSG(supra_ansi_scan(&scanner, nullptr, 0, SUPRA_ANSI_FINAL, &token), 1,
                       "FINAL resolves the pending sequence");
    SUPRA_CHECK_EQ(token.kind, SUPRA_ANSI_TOKEN_MALFORMED);
    SUPRA_CHECK_EQ_MSG(supra_ansi_scanner_pending(&scanner), 0, "state cleared after resolution");
}

/// Long OSC payloads split across chunks: the payload must accumulate, and the
/// true length must be reported even when the reported copy is capped.
void testLongPayloadAcrossChunks() {
    std::string input = "\x1b]0;";
    input.append(400, 'x');
    input += "\x1b\\";

    const auto chunked = scanChunked(input, 7);
    const auto tokens = nonText(chunked);
    SUPRA_CHECK_EQ(tokens.size(), std::size_t{1});
    SUPRA_CHECK_EQ(tokens[0].kind, SUPRA_ANSI_TOKEN_OSC);
    SUPRA_CHECK_EQ_MSG(tokens[0].payload.size(), std::size_t{SUPRA_ANSI_MAX_PAYLOAD},
                       "reported payload capped");

    const auto whole = nonText(scanChunked(input, input.size()));
    SUPRA_CHECK_EQ(whole.size(), std::size_t{1});
    SUPRA_CHECK_STR_EQ(tokens[0].payload, whole[0].payload,
                       "capped payload identical whether chunked or whole");
}

/// Folding a style across chunk boundaries must give the same result as folding
/// the same bytes whole - otherwise a colour split across two reads renders
/// differently from one delivered intact.
void testStyleFoldingAcrossChunks() {
    const std::string input = "\x1b[1;38;2;10;20;30;4:3mtext";

    for (std::size_t chunk = 1; chunk <= input.size(); ++chunk) {
        supra_ansi_style style = supra_ansi_style_default();
        supra_ansi_scanner scanner{};
        supra_ansi_token token{};

        std::size_t offset = 0;
        while (offset < input.size()) {
            const std::size_t len = std::min(chunk, input.size() - offset);
            const bool last = offset + len >= input.size();
            const auto* bytes = reinterpret_cast<const std::uint8_t*>(input.data() + offset);

            while (supra_ansi_scan(&scanner, bytes, len, last ? SUPRA_ANSI_FINAL : SUPRA_ANSI_MORE,
                                  &token) == 1) {
                supra_ansi_style_apply(&style, &token);
                supra_ansi_style_apply_osc(&style, &token);
            }
            offset += len;
        }

        SUPRA_CHECK_EQ_MSG((style.attrs & SUPRA_ANSI_ATTR_BOLD) != 0, true,
                           "bold folded at chunk " + std::to_string(chunk));
        SUPRA_CHECK_EQ_MSG(style.fg.kind, std::uint8_t{SUPRA_ANSI_COLOR_RGB},
                           "RGB folded at chunk " + std::to_string(chunk));
        SUPRA_CHECK_EQ_MSG(style.fg.r, std::uint8_t{10},
                           "red component at chunk " + std::to_string(chunk));
        SUPRA_CHECK_EQ_MSG(style.underline, std::uint8_t{SUPRA_ANSI_UNDERLINE_CURLY},
                           "curly underline at chunk " + std::to_string(chunk));
    }
}

}  // namespace

int main() {
    testChunkingIsInvariant();
    testSingleByteChunks();
    testChunkSplitBeforeC1RangeContinuation();
    testPendingReported();
    testStreamEndsMidSequence();
    testLongPayloadAcrossChunks();
    testStyleFoldingAcrossChunks();
    return supra::test::finish("streaming_test");
}
