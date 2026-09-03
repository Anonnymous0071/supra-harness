// UTF-8 decoder: well-formed sequences, and every malformed class.
//
// The forward-progress guarantee is the load-bearing property. Every scan loop
// in the TUI relies on `consumed >= 1`, so a decoder that returns 0 on some
// byte would hang the renderer on adversarial tool output rather than
// mis-render it.

#include <cstdint>
#include <string>

#include "supra/width.h"
#include "test_assert.hpp"

namespace {

using supra::test::hex;

struct Decoded {
    std::uint32_t cp;
    std::size_t consumed;
    int ok;
};

Decoded decode(const std::string& bytes) {
    Decoded out{};
    out.ok = supra_utf8_decode(reinterpret_cast<const std::uint8_t*>(bytes.data()), bytes.size(),
                               &out.cp, &out.consumed);
    return out;
}

void expectValid(const std::string& bytes, std::uint32_t want_cp, std::size_t want_len) {
    const auto got = decode(bytes);
    SUPRA_CHECK_EQ_MSG(got.ok, 1, "valid " + hex(bytes));
    SUPRA_CHECK_EQ_MSG(got.cp, want_cp, "cp for " + hex(bytes));
    SUPRA_CHECK_EQ_MSG(got.consumed, want_len, "len for " + hex(bytes));
}

void expectMalformed(const std::string& bytes, const char* why) {
    const auto got = decode(bytes);
    SUPRA_CHECK_EQ_MSG(got.ok, 0, std::string(why) + ": " + hex(bytes));
    SUPRA_CHECK_EQ_MSG(got.cp, SUPRA_WIDTH_REPLACEMENT, std::string(why) + " yields U+FFFD");
    // One byte exactly: enough to make progress, few enough not to swallow a
    // valid sequence that happens to follow.
    SUPRA_CHECK_EQ_MSG(got.consumed, std::size_t{1}, std::string(why) + " consumes 1 byte");
}

void testWellFormed() {
    // Explicit length: std::string from a const char* stops at the NUL, so
    // std::string("\x00") is empty rather than a one-byte NUL.
    expectValid(std::string("\x00", 1), 0x0000, 1);
    expectValid("A", 0x0041, 1);
    expectValid("\x7F", 0x007F, 1);
    expectValid("\xC2\x80", 0x0080, 2);
    expectValid("\xC3\xA9", 0x00E9, 2);  // e-acute
    expectValid("\xDF\xBF", 0x07FF, 2);
    expectValid("\xE0\xA0\x80", 0x0800, 3);
    expectValid("\xE4\xB8\xAD", 0x4E2D, 3);  // CJK
    expectValid("\xEF\xBF\xBD", 0xFFFD, 3);  // replacement itself is valid input
    expectValid("\xEF\xBF\xBF", 0xFFFF, 3);
    expectValid("\xF0\x90\x80\x80", 0x10000, 4);
    expectValid("\xF0\x9F\x98\x80", 0x1F600, 4);  // grinning face
    expectValid("\xF4\x8F\xBF\xBF", 0x10FFFF, 4);
}

void testMalformed() {
    // Continuation byte with no lead.
    expectMalformed("\x80", "stray continuation");
    expectMalformed("\xBF", "stray continuation");

    // Overlong forms: C0/C1 can only ever encode a value expressible in fewer
    // bytes, so they are rejected on the lead byte alone.
    expectMalformed("\xC0\x80", "overlong NUL");
    expectMalformed("\xC1\xBF", "overlong 0x7F");
    expectMalformed("\xE0\x80\x80", "overlong 3-byte");
    expectMalformed("\xF0\x80\x80\x80", "overlong 4-byte");

    // Surrogate halves are not scalars; CESU-8 and WTF-8 input must not slip
    // through, because a surrogate reaching a terminal is undefined output.
    expectMalformed("\xED\xA0\x80", "high surrogate U+D800");
    expectMalformed("\xED\xBF\xBF", "low surrogate U+DFFF");

    // Beyond U+10FFFF.
    expectMalformed("\xF5\x80\x80\x80", "above U+10FFFF");
    expectMalformed("\xF7\xBF\xBF\xBF", "above U+10FFFF");
    expectMalformed("\xF8", "5-byte lead");
    expectMalformed("\xFF", "invalid lead");

    // Truncated sequences: a partial read must not consume the whole remainder,
    // or a resumed stream would lose bytes.
    expectMalformed("\xC2", "truncated 2-byte");
    expectMalformed("\xE4\xB8", "truncated 3-byte");
    expectMalformed("\xF0\x9F\x98", "truncated 4-byte");

    // Lead byte followed by a non-continuation.
    expectMalformed("\xC2\x41", "bad continuation");
    expectMalformed("\xE4\x41\x41", "bad continuation");
}

void testForwardProgress() {
    // Every single byte must yield consumed >= 1. This is the property that
    // makes every caller's loop terminate; exhaustive over the byte space
    // because a single exception is a hang.
    for (int byte = 0; byte <= 0xFF; ++byte) {
        const std::string input(1, static_cast<char>(byte));
        std::uint32_t cp = 0;
        std::size_t consumed = 0;
        supra_utf8_decode(reinterpret_cast<const std::uint8_t*>(input.data()), input.size(), &cp,
                          &consumed);
        SUPRA_CHECK_EQ_MSG(consumed, std::size_t{1},
                           "single byte 0x" + std::to_string(byte) + " consumes 1");
    }

    // A malformed prefix must not swallow the valid sequence behind it.
    const std::string mixed = "\x80"
                              "A";
    std::uint32_t cp = 0;
    std::size_t consumed = 0;
    supra_utf8_decode(reinterpret_cast<const std::uint8_t*>(mixed.data()), mixed.size(), &cp,
                      &consumed);
    SUPRA_CHECK_EQ(consumed, std::size_t{1});
    const auto next = decode("A");
    SUPRA_CHECK_EQ(next.cp, std::uint32_t{0x41});
}

void testEdges() {
    // Empty input: no scalar, nothing consumed, and the loop guard reports it.
    std::uint32_t cp = 0;
    std::size_t consumed = 0;
    SUPRA_CHECK_EQ(supra_utf8_decode(nullptr, 0, &cp, &consumed), 0);
    SUPRA_CHECK_EQ(consumed, std::size_t{0});

    // NULL output pointers are a caller bug. Report failure rather than write
    // through them.
    const std::uint8_t byte = 'A';
    SUPRA_CHECK_EQ(supra_utf8_decode(&byte, 1, nullptr, &consumed), 0);
    SUPRA_CHECK_EQ(supra_utf8_decode(&byte, 1, &cp, nullptr), 0);
}

void testEncodedLen() {
    SUPRA_CHECK_EQ(supra_utf8_encoded_len(0x0000), std::size_t{1});
    SUPRA_CHECK_EQ(supra_utf8_encoded_len(0x007F), std::size_t{1});
    SUPRA_CHECK_EQ(supra_utf8_encoded_len(0x0080), std::size_t{2});
    SUPRA_CHECK_EQ(supra_utf8_encoded_len(0x07FF), std::size_t{2});
    SUPRA_CHECK_EQ(supra_utf8_encoded_len(0x0800), std::size_t{3});
    SUPRA_CHECK_EQ(supra_utf8_encoded_len(0xFFFF), std::size_t{3});
    SUPRA_CHECK_EQ(supra_utf8_encoded_len(0x10000), std::size_t{4});
    SUPRA_CHECK_EQ(supra_utf8_encoded_len(0x10FFFF), std::size_t{4});

    // Not scalars, so not encodable.
    SUPRA_CHECK_EQ(supra_utf8_encoded_len(0xD800), std::size_t{0});
    SUPRA_CHECK_EQ(supra_utf8_encoded_len(0xDFFF), std::size_t{0});
    SUPRA_CHECK_EQ(supra_utf8_encoded_len(0x110000), std::size_t{0});
    SUPRA_CHECK_EQ(supra_utf8_encoded_len(0xFFFFFFFF), std::size_t{0});
}

/// Round-trip every scalar: encoded length must match what the decoder
/// consumes. Exhaustive over the whole space, since a single disagreement
/// between encoder and decoder desynchronises any stream containing it.
void testRoundTrip() {
    std::size_t mismatches = 0;
    for (std::uint32_t cp = 0; cp <= 0x10FFFF; ++cp) {
        if (cp >= 0xD800 && cp <= 0xDFFF) {
            continue;
        }
        const std::size_t want = supra_utf8_encoded_len(cp);
        if (want == 0) {
            ++mismatches;
            continue;
        }

        // Encode by hand: the library intentionally exposes no encoder, since
        // callers hold UTF-8 already.
        std::uint8_t buffer[4];
        std::size_t n = 0;
        if (cp < 0x80) {
            buffer[n++] = static_cast<std::uint8_t>(cp);
        } else if (cp < 0x800) {
            buffer[n++] = static_cast<std::uint8_t>(0xC0 | (cp >> 6));
            buffer[n++] = static_cast<std::uint8_t>(0x80 | (cp & 0x3F));
        } else if (cp < 0x10000) {
            buffer[n++] = static_cast<std::uint8_t>(0xE0 | (cp >> 12));
            buffer[n++] = static_cast<std::uint8_t>(0x80 | ((cp >> 6) & 0x3F));
            buffer[n++] = static_cast<std::uint8_t>(0x80 | (cp & 0x3F));
        } else {
            buffer[n++] = static_cast<std::uint8_t>(0xF0 | (cp >> 18));
            buffer[n++] = static_cast<std::uint8_t>(0x80 | ((cp >> 12) & 0x3F));
            buffer[n++] = static_cast<std::uint8_t>(0x80 | ((cp >> 6) & 0x3F));
            buffer[n++] = static_cast<std::uint8_t>(0x80 | (cp & 0x3F));
        }

        std::uint32_t got_cp = 0;
        std::size_t got_len = 0;
        const int ok = supra_utf8_decode(buffer, n, &got_cp, &got_len);
        if (ok != 1 || got_cp != cp || got_len != want) {
            ++mismatches;
        }
    }
    SUPRA_CHECK_EQ_MSG(mismatches, std::size_t{0}, "round-trip mismatches across all scalars");
}

}  // namespace

int main() {
    testWellFormed();
    testMalformed();
    testForwardProgress();
    testEdges();
    testEncodedLen();
    testRoundTrip();
    return supra::test::finish("utf8_test");
}
