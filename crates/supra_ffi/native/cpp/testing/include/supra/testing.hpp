// Minimal assertion helpers shared by the C++20 library test suites.
//
// No test framework enters the dependency graph: these are plain executables
// asserting on exit code. A framework would add a build dependency and a
// compile cost for behaviour four macros already cover.
//
// Extracted from cpp/libsupra_width/tests when libsupra_ansi became the second
// consumer. Header-only INTERFACE target, so it links nothing.

#ifndef SUPRA_TESTING_HPP
#define SUPRA_TESTING_HPP

#include <cstdint>
#include <cstdio>
#include <string>

namespace supra::test {

inline int g_failures = 0;
inline int g_checks = 0;

inline void report(bool ok, const char* file, int line, const std::string& detail) {
    ++g_checks;
    if (!ok) {
        ++g_failures;
        std::fprintf(stderr, "%s:%d: FAIL %s\n", file, line, detail.c_str());
    }
}

/// Render a byte string as escaped hex so failure output is diffable for
/// non-ASCII and escape-bearing input, where raw bytes would be unreadable in a
/// CI log - and where an ESC would reprogram the log viewer's own terminal.
inline std::string hex(const std::string& bytes) {
    static const char* digits = "0123456789ABCDEF";
    std::string out;
    out.reserve(bytes.size() * 4);
    for (const char signed_byte : bytes) {
        // Explicit: char is signed on this platform, and the shift below needs
        // the unsigned value.
        const auto byte = static_cast<unsigned char>(signed_byte);
        out.push_back('\\');
        out.push_back('x');
        out.push_back(digits[byte >> 4]);
        out.push_back(digits[byte & 0x0F]);
    }
    return out;
}

/// Render bytes readably: ESC as `\e`, other controls as hex, printable ASCII
/// as itself. Escape sequences are far easier to diagnose in this form than in
/// pure hex, and it still cannot emit a live ESC into the log.
inline std::string vis(const std::string& bytes) {
    static const char* digits = "0123456789ABCDEF";
    std::string out;
    out.reserve(bytes.size() * 2);
    for (const char signed_byte : bytes) {
        const auto byte = static_cast<unsigned char>(signed_byte);
        if (byte == 0x1B) {
            out += "\\e";
        } else if (byte >= 0x20 && byte < 0x7F) {
            out.push_back(static_cast<char>(byte));
        } else {
            out += "\\x";
            out.push_back(digits[byte >> 4]);
            out.push_back(digits[byte & 0x0F]);
        }
    }
    return out;
}

inline std::string codepoint(std::uint32_t cp) {
    char buffer[16];
    std::snprintf(buffer, sizeof buffer, "U+%04X", cp);
    return buffer;
}

/// Exit status for the process: 0 when every check passed.
inline int finish(const char* suite) {
    if (g_failures == 0) {
        std::fprintf(stderr, "%s: %d checks passed\n", suite, g_checks);
        return 0;
    }
    std::fprintf(stderr, "%s: %d of %d checks FAILED\n", suite, g_failures, g_checks);
    return 1;
}

}  // namespace supra::test

#define SUPRA_CHECK(cond)                                                      \
    ::supra::test::report((cond), __FILE__, __LINE__, "expected: " #cond)

#define SUPRA_CHECK_MSG(cond, msg)                                             \
    ::supra::test::report((cond), __FILE__, __LINE__, std::string(msg))

#define SUPRA_CHECK_EQ(actual, expected)                                       \
    do {                                                                       \
        const auto supra_actual_ = (actual);                                   \
        const auto supra_expected_ = (expected);                               \
        ::supra::test::report(                                                 \
            supra_actual_ == supra_expected_, __FILE__, __LINE__,              \
            std::string(#actual " == " #expected " (got ") +                   \
                std::to_string(supra_actual_) + ", want " +                    \
                std::to_string(supra_expected_) + ")");                        \
    } while (false)

#define SUPRA_CHECK_EQ_MSG(actual, expected, msg)                              \
    do {                                                                       \
        const auto supra_actual_ = (actual);                                   \
        const auto supra_expected_ = (expected);                               \
        ::supra::test::report(                                                 \
            supra_actual_ == supra_expected_, __FILE__, __LINE__,              \
            std::string(msg) + " (got " + std::to_string(supra_actual_) +      \
                ", want " + std::to_string(supra_expected_) + ")");            \
    } while (false)

#define SUPRA_CHECK_STR_EQ(actual, expected, msg)                              \
    do {                                                                       \
        const std::string supra_a_ = (actual);                                 \
        const std::string supra_b_ = (expected);                               \
        ::supra::test::report(supra_a_ == supra_b_, __FILE__, __LINE__,         \
                             std::string(msg) + " (got \"" +                   \
                                 ::supra::test::vis(supra_a_) + "\", want \"" + \
                                 ::supra::test::vis(supra_b_) + "\")");        \
    } while (false)

#endif  // SUPRA_TESTING_HPP
