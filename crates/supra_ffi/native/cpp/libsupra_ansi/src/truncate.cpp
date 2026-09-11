// Measurement, stripping, and style-safe truncation.
//
// The truncation planner is the reason this library exists. Two failure modes
// motivate it, and both persist until the next full repaint rather than
// affecting only the line involved:
//
//   * Cutting inside an escape sequence sends the terminal a fragment it
//     interprets as a command over whatever follows.
//   * Leaving styling open bleeds colour into unrelated output.
//
// Cell widths come from libsupra_width; this file never decides a width itself.

#include <cstddef>
#include <cstdint>

#include "internal.hpp"
#include "supra/ansi.h"
#include "supra/width.h"

namespace {

using namespace supra::ansi::detail;

/// Walk tokens over a complete buffer.
///
/// Every caller here has the whole line in memory, so SUPRA_ANSI_FINAL is
/// correct: an unterminated sequence is malformed rather than pending, and the
/// loop cannot wait for a terminator that will never arrive.
template <typename OnText, typename OnToken>
void forEachToken(const std::uint8_t* bytes, std::size_t len, OnText&& on_text,
                  OnToken&& on_token) {
    supra_ansi_scanner scanner{};
    supra_ansi_token token{};

    while (supra_ansi_scan(&scanner, bytes, len, SUPRA_ANSI_FINAL, &token) == 1) {
        if (token.length == 0 && token.kind != SUPRA_ANSI_TOKEN_MALFORMED) {
            break;
        }
        if (token.kind == SUPRA_ANSI_TOKEN_TEXT) {
            on_text(token.offset, token.length);
        } else {
            on_token(token);
        }
    }
}

}  // namespace

extern "C" {

std::size_t supra_ansi_measure(const std::uint8_t* bytes, std::size_t len,
                               supra_width_ambiguous ambiguous) {
    if (bytes == nullptr || len == 0) {
        return 0;
    }

    std::size_t cells = 0;
    forEachToken(
        bytes, len,
        [&](std::size_t offset, std::size_t length) {
            cells += supra_wcswidth(bytes + offset, length, ambiguous);
        },
        // Escape sequences are state changes rather than content, so they
        // contribute nothing.
        [](const supra_ansi_token&) {});
    return cells;
}

std::size_t supra_ansi_strip(const std::uint8_t* bytes, std::size_t len, std::uint8_t* out,
                             std::size_t cap) {
    if (bytes == nullptr || len == 0) {
        return 0;
    }

    std::size_t needed = 0;
    forEachToken(
        bytes, len,
        [&](std::size_t offset, std::size_t length) {
            for (std::size_t i = 0; i < length; ++i) {
                if (needed < cap && out != nullptr) {
                    out[needed] = bytes[offset + i];
                }
                ++needed;
            }
        },
        [&](const supra_ansi_token& token) {
            // Tab and newline are layout and must survive; every other control
            // can move the cursor, and passing it through would let tool output
            // reposition the terminal.
            if (token.kind == SUPRA_ANSI_TOKEN_CONTROL && isTextControl(token.final_byte)) {
                if (needed < cap && out != nullptr) {
                    out[needed] = token.final_byte;
                }
                ++needed;
            }
        });

    return needed;
}

void supra_ansi_plan_truncate_from(const std::uint8_t* bytes, std::size_t len,
                                   std::size_t max_cells, supra_width_ambiguous ambiguous,
                                   const supra_ansi_style* initial, supra_ansi_truncation* out) {
    if (out == nullptr) {
        return;
    }

    *out = supra_ansi_truncation{};
    out->style_at_cut = initial != nullptr ? *initial : supra_ansi_style_default();

    if (bytes == nullptr || len == 0) {
        // Even an empty line may need a trailer: an inherited style has to be
        // closed, or it bleeds into whatever follows.
        out->trailer_len = static_cast<std::uint8_t>(supra_ansi_style_emit_reset(
            &out->style_at_cut, out->trailer, SUPRA_ANSI_MAX_TRAILER));
        return;
    }

    supra_ansi_style style = out->style_at_cut;
    supra_ansi_scanner scanner{};
    supra_ansi_token token{};

    std::size_t cells = 0;
    std::size_t prefix = 0;
    bool cut = false;

    while (!cut && supra_ansi_scan(&scanner, bytes, len, SUPRA_ANSI_FINAL, &token) == 1) {
        if (token.length == 0 && token.kind != SUPRA_ANSI_TOKEN_MALFORMED) {
            break;
        }

        if (token.kind == SUPRA_ANSI_TOKEN_TEXT) {
            // Walk the run cluster by cluster; a cluster is the smallest unit
            // that can be emitted without corrupting a cell. Text never changes
            // the style, so `style` stays correct as the prefix grows.
            std::size_t local = 0;
            while (local < token.length) {
                std::size_t cluster_len = 0;
                const int w = supra_grapheme_width(bytes + token.offset + local,
                                                   token.length - local, ambiguous, &cluster_len);
                if (cluster_len == 0) {
                    break;
                }
                const std::size_t cluster_cells = w > 0 ? static_cast<std::size_t>(w) : 0;

                // A two-cell cluster that would cross the limit is excluded
                // whole: half a wide glyph is a corrupted cell, not a narrow
                // glyph.
                if (cells + cluster_cells > max_cells) {
                    cut = true;
                    break;
                }

                cells += cluster_cells;
                local += cluster_len;
                prefix = token.offset + local;
            }
            continue;
        }

        // A non-text token. Escape sequences occupy no cells, so they are always
        // included - but only as whole sequences, never split.
        //
        // The style is folded in step with the prefix advancing, and the two must
        // stay together for the trailer to be trustworthy. A sequence inside the
        // prefix *is* emitted, so whatever it opened is live in the output and
        // the trailer has to close it. Tracking the style only at accepted text
        // would leave a trailing `ESC[31m` unclosed and bleed red into every
        // following line.
        supra_ansi_style_apply(&style, &token);
        supra_ansi_style_apply_osc(&style, &token);

        // PARTIAL cannot occur with SUPRA_ANSI_FINAL, and MALFORMED bytes are
        // dropped rather than forwarded: passing a broken sequence through would
        // hand the terminal a command fragment.
        if (token.kind != SUPRA_ANSI_TOKEN_MALFORMED &&
            token.kind != SUPRA_ANSI_TOKEN_PARTIAL) {
            prefix = token.offset + token.length;
        }
    }

    out->prefix_len = prefix;
    out->cells = cells;
    out->truncated = cut ? 1 : 0;
    out->style_at_cut = style;

    // Close whatever the prefix left open. A line with no styling gets an empty
    // trailer, so plain text costs no extra bytes.
    out->trailer_len = static_cast<std::uint8_t>(
        supra_ansi_style_emit_reset(&style, out->trailer, SUPRA_ANSI_MAX_TRAILER));
}

void supra_ansi_plan_truncate(const std::uint8_t* bytes, std::size_t len, std::size_t max_cells,
                              supra_width_ambiguous ambiguous, supra_ansi_truncation* out) {
    supra_ansi_plan_truncate_from(bytes, len, max_cells, ambiguous, nullptr, out);
}

}  // extern "C"
