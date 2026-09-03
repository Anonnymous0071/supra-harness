// UAX #29 conformance against the official GraphemeBreakTest.txt.
//
// This is the test that matters most in T2. Hand-written cluster cases check
// what the author thought of; the UCD conformance file checks what the standard
// actually requires, including the rule interactions nobody enumerates by hand
// (GB9c Indic conjuncts, GB11 pictographic ZWJ chains, GB12/GB13 flag parity).
//
// The fixture is the vendored data/GraphemeBreakTest.txt, so this suite runs
// offline and pins to the same Unicode version the tables were generated from.
//
// Format, per line:
//
//     ÷ 000D × 000A ÷ # comment
//
// U+00F7 DIVISION SIGN marks a cluster boundary, U+00D7 MULTIPLICATION SIGN
// marks a prohibited break. Every line begins and ends with a boundary.

#include <cstdint>
#include <cstdio>
#include <fstream>
#include <string>
#include <vector>

#include "supra/width.h"
#include "test_assert.hpp"

namespace {

/// One conformance case: the code points, and the offsets where a break must
/// occur.
struct Case {
    std::vector<std::uint32_t> code_points;
    std::vector<std::size_t> break_offsets;  // code point indices, ascending
    std::string source;
    int line_number = 0;
};

constexpr const char* kDivision = "\xC3\xB7";        // U+00F7
constexpr const char* kMultiplication = "\xC3\x97";  // U+00D7

std::string encodeUtf8(std::uint32_t cp) {
    std::string out;
    if (cp < 0x80) {
        out.push_back(static_cast<char>(cp));
    } else if (cp < 0x800) {
        out.push_back(static_cast<char>(0xC0 | (cp >> 6)));
        out.push_back(static_cast<char>(0x80 | (cp & 0x3F)));
    } else if (cp < 0x10000) {
        out.push_back(static_cast<char>(0xE0 | (cp >> 12)));
        out.push_back(static_cast<char>(0x80 | ((cp >> 6) & 0x3F)));
        out.push_back(static_cast<char>(0x80 | (cp & 0x3F)));
    } else {
        out.push_back(static_cast<char>(0xF0 | (cp >> 18)));
        out.push_back(static_cast<char>(0x80 | ((cp >> 12) & 0x3F)));
        out.push_back(static_cast<char>(0x80 | ((cp >> 6) & 0x3F)));
        out.push_back(static_cast<char>(0x80 | (cp & 0x3F)));
    }
    return out;
}

/// Parse one line into a Case. Returns false for comments and blanks.
bool parseLine(const std::string& raw, int line_number, Case& out) {
    const std::size_t hash = raw.find('#');
    const std::string body = hash == std::string::npos ? raw : raw.substr(0, hash);

    std::vector<std::string> tokens;
    std::size_t pos = 0;
    while (pos < body.size()) {
        while (pos < body.size() && (body[pos] == ' ' || body[pos] == '\t' || body[pos] == '\r')) {
            ++pos;
        }
        if (pos >= body.size()) {
            break;
        }
        const std::size_t start = pos;
        while (pos < body.size() && body[pos] != ' ' && body[pos] != '\t' && body[pos] != '\r') {
            ++pos;
        }
        tokens.push_back(body.substr(start, pos - start));
    }

    if (tokens.empty()) {
        return false;
    }

    out = Case{};
    out.line_number = line_number;
    out.source = body;

    // Tokens alternate marker, code point, marker, ... starting and ending with
    // a marker. A break marker before code point i means a cluster boundary at
    // index i.
    for (const auto& token : tokens) {
        if (token == kDivision) {
            out.break_offsets.push_back(out.code_points.size());
        } else if (token == kMultiplication) {
            // Prohibited break; nothing to record.
        } else {
            char* end = nullptr;
            const auto cp = static_cast<std::uint32_t>(std::strtoul(token.c_str(), &end, 16));
            if (end == token.c_str()) {
                return false;  // Not a hex token: malformed line, skip it.
            }
            out.code_points.push_back(cp);
        }
    }

    return !out.code_points.empty();
}

/// Segment `text` with supra_grapheme_next and report the boundary offsets in
/// code point indices, matching how the fixture expresses them.
std::vector<std::size_t> segment(const std::vector<std::uint32_t>& code_points) {
    // Byte offset of each code point, so byte boundaries can be mapped back to
    // code point indices.
    std::string text;
    std::vector<std::size_t> byte_to_index;
    for (std::size_t i = 0; i < code_points.size(); ++i) {
        const std::string encoded = encodeUtf8(code_points[i]);
        for (std::size_t b = 0; b < encoded.size(); ++b) {
            byte_to_index.push_back(i);
        }
        text += encoded;
    }
    byte_to_index.push_back(code_points.size());  // sentinel for end-of-text

    std::vector<std::size_t> boundaries;
    const auto* bytes = reinterpret_cast<const std::uint8_t*>(text.data());

    std::size_t offset = 0;
    boundaries.push_back(0);  // GB1: break at start of text
    while (offset < text.size()) {
        const std::size_t cluster_len = supra_grapheme_next(bytes + offset, text.size() - offset);
        if (cluster_len == 0) {
            break;  // Defensive; the contract guarantees >= 1.
        }
        offset += cluster_len;
        boundaries.push_back(byte_to_index[offset]);
    }

    return boundaries;
}

std::string describe(const Case& test_case, const std::vector<std::size_t>& got) {
    std::string out = "line " + std::to_string(test_case.line_number) + ": cps=[";
    for (std::size_t i = 0; i < test_case.code_points.size(); ++i) {
        if (i != 0) {
            out += " ";
        }
        out += supra::test::codepoint(test_case.code_points[i]);
    }
    out += "] want breaks=[";
    for (std::size_t i = 0; i < test_case.break_offsets.size(); ++i) {
        if (i != 0) {
            out += " ";
        }
        out += std::to_string(test_case.break_offsets[i]);
    }
    out += "] got=[";
    for (std::size_t i = 0; i < got.size(); ++i) {
        if (i != 0) {
            out += " ";
        }
        out += std::to_string(got[i]);
    }
    out += "]";
    return out;
}

}  // namespace

int main(int argc, char** argv) {
    // CMake passes the vendored fixture path so the test does not depend on the
    // working directory.
    if (argc < 2) {
        std::fprintf(stderr, "usage: grapheme_conformance_test <GraphemeBreakTest.txt>\n");
        return 2;
    }

    std::ifstream input(argv[1]);
    if (!input) {
        std::fprintf(stderr, "cannot open fixture: %s\n", argv[1]);
        return 2;
    }

    std::size_t executed = 0;
    std::size_t failed = 0;
    std::string line;
    int line_number = 0;

    while (std::getline(input, line)) {
        ++line_number;
        Case test_case;
        if (!parseLine(line, line_number, test_case)) {
            continue;
        }

        ++executed;
        const auto got = segment(test_case.code_points);

        if (got != test_case.break_offsets) {
            ++failed;
            // Cap the noise: a table regression fails hundreds of cases at
            // once, and the first few are enough to diagnose it.
            if (failed <= 10) {
                std::fprintf(stderr, "FAIL %s\n", describe(test_case, got).c_str());
            }
        }
    }

    // The fixture is vendored, so an empty parse means the file moved or the
    // parser broke - not that the implementation is correct.
    if (executed < 500) {
        std::fprintf(stderr,
                     "only %zu cases parsed from %s; expected 500+. "
                     "Fixture or parser is broken.\n",
                     executed, argv[1]);
        return 2;
    }

    if (failed != 0) {
        std::fprintf(stderr, "grapheme_conformance_test: %zu of %zu UCD cases FAILED\n", failed,
                     executed);
        return 1;
    }

    std::fprintf(stderr, "grapheme_conformance_test: %zu UCD cases passed\n", executed);
    return 0;
}
