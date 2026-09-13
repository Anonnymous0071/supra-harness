// UAX #29 extended grapheme cluster segmentation.
//
// Implemented as a forward scan applying the break rules in order. The state a
// correct implementation needs is small but not obvious:
//
//   * a regional-indicator parity count, for GB12/GB13 (flags pair up, so the
//     third indicator starts a new cluster);
//   * whether the run since the last non-Extend base has seen a ZWJ preceded by
//     Extended_Pictographic, for GB11 (emoji ZWJ sequences);
//   * whether an InCB Linker has appeared between two InCB Consonants, for
//     GB9c (Indic conjuncts).
//
// Getting GB12/GB13 wrong is the classic flag bug: two regional indicators are
// one flag, four are two flags, three are a flag plus a stray letter.

#include <cstddef>
#include <cstdint>

#include "internal.hpp"
#include "supra/width.h"

namespace {

using supra::width::detail::decode;
using supra::width::detail::Decoded;
using supra::width::detail::Gcb;
using supra::width::detail::gcb;
using supra::width::detail::isExtendedPictographic;
using supra::width::detail::isIncbConsonant;
using supra::width::detail::isIncbExtend;
using supra::width::detail::isIncbLinker;
using supra::width::detail::kZwj;

/// Tracks the cross-code-point state the break rules depend on.
struct ClusterState {
    /// Regional indicators seen in the current run, for GB12/GB13.
    std::size_t ri_count = 0;
    /// GB11: an Extended_Pictographic followed by Extend* ZWJ is pending.
    bool pictographic_zwj_pending = false;
    /// GB9c: an InCB Consonant has been seen, and a Linker after it.
    bool incb_consonant_seen = false;
    bool incb_linker_seen = false;
};

/// Decide whether a break occurs between `prev` and `next`.
///
/// Rule numbers refer to UAX #29 Table 1c. GB1/GB2 (start and end of text) are
/// handled by the caller's loop bounds.
[[nodiscard]] bool breaksBetween(std::uint32_t prev, std::uint32_t next,
                                 const ClusterState& state) noexcept {
    const Gcb a = gcb(prev);
    const Gcb b = gcb(next);

    // GB3: CR x LF - never break inside a CRLF pair.
    if (a == Gcb::CR && b == Gcb::LF) {
        return false;
    }
    // GB4: (Control | CR | LF) divide - a control always ends a cluster.
    if (a == Gcb::Control || a == Gcb::CR || a == Gcb::LF) {
        return true;
    }
    // GB5: divide (Control | CR | LF).
    if (b == Gcb::Control || b == Gcb::CR || b == Gcb::LF) {
        return true;
    }

    // GB6/GB7/GB8: Hangul jamo sequences compose into one syllable.
    if (a == Gcb::HangulL &&
        (b == Gcb::HangulL || b == Gcb::HangulV || b == Gcb::HangulLV || b == Gcb::HangulLVT)) {
        return false;
    }
    if ((a == Gcb::HangulLV || a == Gcb::HangulV) && (b == Gcb::HangulV || b == Gcb::HangulT)) {
        return false;
    }
    if ((a == Gcb::HangulLVT || a == Gcb::HangulT) && b == Gcb::HangulT) {
        return false;
    }

    // GB9: x (Extend | ZWJ) - marks and joiners attach to what precedes them.
    if (b == Gcb::Extend || b == Gcb::ZWJ) {
        return false;
    }
    // GB9a: x SpacingMark.
    if (b == Gcb::SpacingMark) {
        return false;
    }
    // GB9b: Prepend x.
    if (a == Gcb::Prepend) {
        return false;
    }

    // GB9c: Indic conjunct. Consonant [Extend | Linker]* Linker
    // [Extend | Linker]* x Consonant. The linker must have appeared after the
    // consonant, which is why this needs carried state rather than a lookback
    // of one.
    if (state.incb_consonant_seen && state.incb_linker_seen && isIncbConsonant(next)) {
        return false;
    }

    // GB11: Extended_Pictographic Extend* ZWJ x Extended_Pictographic. This is
    // what keeps a family emoji or a profession sequence as one cluster.
    if (a == Gcb::ZWJ && state.pictographic_zwj_pending && isExtendedPictographic(next)) {
        return false;
    }

    // GB12/GB13: pair regional indicators. An odd count means the run is
    // mid-flag and the next indicator completes it; an even count means the
    // next indicator starts a new flag.
    if (a == Gcb::RegionalIndicator && b == Gcb::RegionalIndicator) {
        return state.ri_count % 2 == 0;
    }

    // GB999: break everywhere else.
    return true;
}

/// Fold one code point into the carried state.
void advanceState(ClusterState& state, std::uint32_t cp) noexcept {
    const Gcb cls = gcb(cp);

    if (cls == Gcb::RegionalIndicator) {
        ++state.ri_count;
    } else if (cls != Gcb::Extend && cls != Gcb::ZWJ) {
        // Any other base resets flag pairing.
        state.ri_count = 0;
    }

    // GB11 bookkeeping: a pictographic arms the pending flag, Extend preserves
    // it, ZWJ carries it to the next code point, anything else clears it.
    if (isExtendedPictographic(cp)) {
        state.pictographic_zwj_pending = true;
    } else if (cls == Gcb::Extend || cp == kZwj) {
        // Preserve across Extend* ZWJ.
    } else {
        state.pictographic_zwj_pending = false;
    }

    // GB9c bookkeeping.
    if (isIncbConsonant(cp)) {
        state.incb_consonant_seen = true;
        state.incb_linker_seen = false;
    } else if (isIncbLinker(cp)) {
        if (state.incb_consonant_seen) {
            state.incb_linker_seen = true;
        }
    } else if (!isIncbExtend(cp)) {
        state.incb_consonant_seen = false;
        state.incb_linker_seen = false;
    }
}

}  // namespace

namespace supra::width::detail {

/// Byte length of the cluster at `bytes`, shared with the measurement code so
/// segmentation has exactly one implementation.
std::size_t clusterLength(const std::uint8_t* bytes, std::size_t len) noexcept {
    if (bytes == nullptr || len == 0) {
        return 0;
    }

    Decoded current = decode(bytes, len);
    std::size_t offset = current.len;

    ClusterState state;
    advanceState(state, current.cp);

    while (offset < len) {
        const Decoded next = decode(bytes + offset, len - offset);
        if (breaksBetween(current.cp, next.cp, state)) {
            break;
        }
        advanceState(state, next.cp);
        current = next;
        offset += next.len;
    }

    return offset;
}

}  // namespace supra::width::detail

extern "C" {

std::size_t supra_grapheme_next(const std::uint8_t* bytes, std::size_t len) {
    return supra::width::detail::clusterLength(bytes, len);
}

}  // extern "C"
