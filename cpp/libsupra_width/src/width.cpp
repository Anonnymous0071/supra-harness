// Cell width: per code point, per grapheme cluster, and per string.

#include <cstddef>
#include <cstdint>

#include "internal.hpp"
#include "supra/width.h"
#include "tables.hpp"

namespace supra::width::detail {
// Defined in grapheme.cpp.
std::size_t clusterLength(const std::uint8_t* bytes, std::size_t len) noexcept;
}  // namespace supra::width::detail

namespace {

using supra::width::detail::clusterLength;
using supra::width::detail::codePointWidth;
using supra::width::detail::decode;
using supra::width::detail::kMaxCodePoint;
using supra::width::detail::kSurrogateHi;
using supra::width::detail::kSurrogateLo;
using supra::width::detail::kVs15;
using supra::width::detail::kVs16;

/// Width of one grapheme cluster.
///
/// A cluster occupies the width of its widest constituent, not the sum: an
/// emoji ZWJ sequence of four code points renders in two cells, and a base plus
/// combining mark is the width of the base.
///
/// Variation selectors override presentation rather than adding width: U+FE0F
/// forces emoji presentation (two cells) and U+FE0E forces text presentation
/// (one). They are applied after the scan so their order within the cluster
/// does not change the outcome.
[[nodiscard]] int clusterWidth(const std::uint8_t* bytes, std::size_t cluster_len,
                               supra_width_ambiguous ambiguous) noexcept {
    int widest = 0;
    bool saw_printable = false;
    bool forced_emoji = false;
    bool forced_text = false;

    std::size_t offset = 0;
    while (offset < cluster_len) {
        const auto decoded = decode(bytes + offset, cluster_len - offset);
        offset += decoded.len;

        if (decoded.cp == kVs16) {
            forced_emoji = true;
            continue;
        }
        if (decoded.cp == kVs15) {
            forced_text = true;
            continue;
        }

        const int w = codePointWidth(decoded.cp, ambiguous);
        if (w == SUPRA_WIDTH_NONPRINTABLE) {
            continue;
        }
        saw_printable = true;
        if (w > widest) {
            widest = w;
        }
    }

    if (!saw_printable) {
        return SUPRA_WIDTH_NONPRINTABLE;
    }

    // VS15 wins over VS16 when both appear: text presentation is the more
    // conservative choice, and a one-cell assumption that renders wide is a
    // recoverable overflow, whereas a two-cell assumption that renders narrow
    // leaves a permanent gap.
    if (forced_text) {
        return widest > 1 ? 1 : widest;
    }
    if (forced_emoji) {
        return 2;
    }
    return widest;
}

}  // namespace

extern "C" {

int supra_wcwidth(std::uint32_t cp, supra_width_ambiguous ambiguous) {
    return codePointWidth(cp, ambiguous);
}

int supra_grapheme_width(const std::uint8_t* bytes, std::size_t len,
                         supra_width_ambiguous ambiguous, std::size_t* out_len) {
    if (bytes == nullptr || len == 0) {
        if (out_len != nullptr) {
            *out_len = 0;
        }
        return SUPRA_WIDTH_NONPRINTABLE;
    }

    const std::size_t cluster_len = clusterLength(bytes, len);
    if (out_len != nullptr) {
        *out_len = cluster_len;
    }
    return clusterWidth(bytes, cluster_len, ambiguous);
}

std::size_t supra_wcswidth(const std::uint8_t* bytes, std::size_t len,
                           supra_width_ambiguous ambiguous) {
    return supra_width_measure(bytes, len, ambiguous, nullptr);
}

std::size_t supra_width_measure(const std::uint8_t* bytes, std::size_t len,
                                supra_width_ambiguous ambiguous, std::size_t* out_clusters) {
    std::size_t total = 0;
    std::size_t clusters = 0;

    if (bytes != nullptr) {
        std::size_t offset = 0;
        while (offset < len) {
            const std::size_t cluster_len = clusterLength(bytes + offset, len - offset);
            if (cluster_len == 0) {
                break;  // Defensive: clusterLength guarantees >= 1 for len > 0.
            }
            const int w = clusterWidth(bytes + offset, cluster_len, ambiguous);
            // Non-printable contributes nothing: this measures the room text
            // needs, and a control character needs none. Callers that must
            // reject such text use supra_width_validate.
            if (w > 0) {
                total += static_cast<std::size_t>(w);
            }
            ++clusters;
            offset += cluster_len;
        }
    }

    if (out_clusters != nullptr) {
        *out_clusters = clusters;
    }
    return total;
}

std::size_t supra_width_truncate(const std::uint8_t* bytes, std::size_t len, std::size_t max_cells,
                                 supra_width_ambiguous ambiguous, std::size_t* out_cells) {
    std::size_t consumed_bytes = 0;
    std::size_t consumed_cells = 0;

    if (bytes != nullptr) {
        std::size_t offset = 0;
        while (offset < len) {
            const std::size_t cluster_len = clusterLength(bytes + offset, len - offset);
            if (cluster_len == 0) {
                break;
            }
            const int w = clusterWidth(bytes + offset, cluster_len, ambiguous);
            const std::size_t cells = w > 0 ? static_cast<std::size_t>(w) : 0;

            // A two-cell cluster that would cross the limit is excluded whole:
            // half a wide glyph is a corrupted cell, not a narrow glyph.
            if (consumed_cells + cells > max_cells) {
                break;
            }

            consumed_cells += cells;
            offset += cluster_len;
            consumed_bytes = offset;
        }
    }

    if (out_cells != nullptr) {
        *out_cells = consumed_cells;
    }
    return consumed_bytes;
}

supra_width_validity supra_width_validate(const std::uint8_t* bytes, std::size_t len,
                                          std::size_t* out_offset) {
    if (bytes == nullptr || len == 0) {
        if (out_offset != nullptr) {
            *out_offset = 0;
        }
        return SUPRA_WIDTH_VALID;
    }

    std::size_t offset = 0;
    while (offset < len) {
        const auto decoded = decode(bytes + offset, len - offset);
        if (!decoded.valid) {
            if (out_offset != nullptr) {
                *out_offset = offset;
            }
            return SUPRA_WIDTH_INVALID_UTF8;
        }
        if (codePointWidth(decoded.cp, SUPRA_WIDTH_AMBIGUOUS_NARROW) == SUPRA_WIDTH_NONPRINTABLE) {
            if (out_offset != nullptr) {
                *out_offset = offset;
            }
            return SUPRA_WIDTH_HAS_NONPRINTABLE;
        }
        offset += decoded.len;
    }

    if (out_offset != nullptr) {
        *out_offset = len;
    }
    return SUPRA_WIDTH_VALID;
}

int supra_width_probe(const char* utf8, supra_width_ambiguous ambiguous) {
    if (utf8 == nullptr) {
        return SUPRA_WIDTH_NONPRINTABLE;
    }

    std::size_t len = 0;
    while (utf8[len] != '\0') {
        ++len;
    }
    if (len == 0) {
        return SUPRA_WIDTH_NONPRINTABLE;
    }

    const auto* bytes = reinterpret_cast<const std::uint8_t*>(utf8);

    // Reject malformed input outright: a probe that silently measures U+FFFD
    // would report a plausible width for a glyph the terminal cannot render.
    std::size_t scan = 0;
    while (scan < len) {
        const auto decoded = decode(bytes + scan, len - scan);
        if (!decoded.valid) {
            return SUPRA_WIDTH_NONPRINTABLE;
        }
        scan += decoded.len;
    }

    // Must be exactly one cluster. Measuring a multi-cluster string here would
    // let the caller conclude that a two-glyph fallback "fits" in one cell.
    const std::size_t cluster_len = clusterLength(bytes, len);
    if (cluster_len != len) {
        return SUPRA_WIDTH_NONPRINTABLE;
    }

    return clusterWidth(bytes, cluster_len, ambiguous);
}

const char* supra_width_unicode_version(void) {
    return supra::width::tables::kUnicodeVersion;
}

}  // extern "C"
