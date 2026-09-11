// SGR state: folding tokens into a style, and serialising a style back out.
//
// Two directions, and the round trip has to hold: whatever the scanner folds in
// must come back out as a sequence producing the same terminal state.
// Truncation depends on it - the trailer is generated from the folded style, and
// if serialisation disagreed with folding the trailer would fail to close what
// the prefix opened.

#include <cstddef>
#include <cstdint>

#include "internal.hpp"
#include "supra/ansi.h"

namespace {

using namespace supra::ansi::detail;

/// Append a byte, tracking the required length even once the buffer is full.
///
/// Callers ask for the length first with a null buffer and allocate exactly
/// once, so a truncated write must still report the true size.
struct Writer {
    std::uint8_t* out;
    std::size_t cap;
    std::size_t written = 0;
    bool overflow = false;

    void byte(std::uint8_t value) noexcept {
        if (written < cap && out != nullptr) {
            out[written] = value;
        } else {
            overflow = true;
        }
        ++written;
    }

    void literal(const char* text) noexcept {
        for (const char* p = text; *p != '\0'; ++p) {
            byte(static_cast<std::uint8_t>(*p));
        }
    }

    /// Decimal, no padding. Only ever called with values <= 255.
    void number(std::uint32_t value) noexcept {
        if (value >= 100) {
            byte(static_cast<std::uint8_t>('0' + (value / 100)));
            byte(static_cast<std::uint8_t>('0' + ((value / 10) % 10)));
            byte(static_cast<std::uint8_t>('0' + (value % 10)));
        } else if (value >= 10) {
            byte(static_cast<std::uint8_t>('0' + (value / 10)));
            byte(static_cast<std::uint8_t>('0' + (value % 10)));
        } else {
            byte(static_cast<std::uint8_t>('0' + value));
        }
    }
};

[[nodiscard]] bool colorEqual(const supra_ansi_color& a, const supra_ansi_color& b) noexcept {
    if (a.kind != b.kind) {
        return false;
    }
    switch (a.kind) {
        case SUPRA_ANSI_COLOR_INDEXED:
            return a.index == b.index;
        case SUPRA_ANSI_COLOR_RGB:
            return a.r == b.r && a.g == b.g && a.b == b.b;
        default:
            return true;
    }
}

/// Resolve a parameter, substituting the SGR default of 0 when omitted.
[[nodiscard]] std::int32_t paramOr0(const supra_ansi_token& token, std::size_t index) noexcept {
    if (index >= token.param_count) {
        return 0;
    }
    const std::int32_t value = token.params[index];
    return value == kParamOmitted ? 0 : value;
}

/// Read an extended colour starting at parameter `index`.
///
/// Two forms exist and both appear in the wild:
///
///   legacy:  38 ; 2 ; r ; g ; b        and  38 ; 5 ; n
///   ITU-T:   38 : 2 : cs : r : g : b   and  38 : 5 : n
///
/// The colon form carries a colour-space id that is almost always empty, which
/// is why `38:2::255:0:0` has a doubled colon. A parser handling only the
/// semicolon form silently renders the wrong colour rather than failing.
///
/// @param consumed Receives how many *parameters* were used.
[[nodiscard]] bool readExtendedColor(const supra_ansi_token& token, std::size_t index,
                                     supra_ansi_color& out, std::size_t& consumed) noexcept {
    consumed = 1;
    if (index >= token.param_count) {
        return false;
    }

    const std::uint8_t sub_count = token.subparam_count[index];

    if (sub_count > 0) {
        // Colon form: everything lives in this parameter's sub-parameters.
        const std::uint8_t base = token.subparam_index[index];
        const std::int32_t mode = token.subparams[base];

        if (mode == 5 && sub_count >= 2) {
            const std::int32_t n = token.subparams[base + 1];
            out.kind = SUPRA_ANSI_COLOR_INDEXED;
            out.index = static_cast<std::uint8_t>(n < 0 ? 0 : (n > 255 ? 255 : n));
            return true;
        }
        if (mode == 2) {
            // The colour-space id is optional, so the components sit at either
            // offset 2 (absent) or 3 (present).
            const std::uint8_t offset = sub_count >= 5 ? 2 : 1;
            if (sub_count >= static_cast<std::uint8_t>(offset + 3)) {
                const auto clamp = [](std::int32_t v) noexcept -> std::uint8_t {
                    return static_cast<std::uint8_t>(v < 0 ? 0 : (v > 255 ? 255 : v));
                };
                out.kind = SUPRA_ANSI_COLOR_RGB;
                out.r = clamp(token.subparams[base + offset]);
                out.g = clamp(token.subparams[base + offset + 1]);
                out.b = clamp(token.subparams[base + offset + 2]);
                return true;
            }
        }
        return false;
    }

    // Semicolon form: the mode and components are separate parameters.
    if (index + 1 >= token.param_count) {
        return false;
    }
    const std::int32_t mode = paramOr0(token, index + 1);

    if (mode == 5) {
        if (index + 2 >= token.param_count) {
            consumed = 2;
            return false;
        }
        const std::int32_t n = paramOr0(token, index + 2);
        out.kind = SUPRA_ANSI_COLOR_INDEXED;
        out.index = static_cast<std::uint8_t>(n < 0 ? 0 : (n > 255 ? 255 : n));
        consumed = 3;
        return true;
    }

    if (mode == 2) {
        if (index + 4 >= token.param_count) {
            consumed = token.param_count - index;
            return false;
        }
        const auto clamp = [](std::int32_t v) noexcept -> std::uint8_t {
            return static_cast<std::uint8_t>(v < 0 ? 0 : (v > 255 ? 255 : v));
        };
        out.kind = SUPRA_ANSI_COLOR_RGB;
        out.r = clamp(paramOr0(token, index + 2));
        out.g = clamp(paramOr0(token, index + 3));
        out.b = clamp(paramOr0(token, index + 4));
        consumed = 5;
        return true;
    }

    consumed = 2;
    return false;
}

void emitColor(Writer& w, const supra_ansi_color& color, bool foreground) noexcept {
    switch (color.kind) {
        case SUPRA_ANSI_COLOR_INDEXED:
            // The 0-7 and 8-15 shortcuts are shorter and universally supported;
            // this runs per line per frame, so the bytes are worth saving.
            if (color.index < 8) {
                w.number(static_cast<std::uint32_t>((foreground ? 30 : 40) + color.index));
            } else if (color.index < 16) {
                w.number(static_cast<std::uint32_t>((foreground ? 90 : 100) + color.index - 8));
            } else {
                w.number(foreground ? 38 : 48);
                w.literal(";5;");
                w.number(color.index);
            }
            break;
        case SUPRA_ANSI_COLOR_RGB:
            w.number(foreground ? 38 : 48);
            w.literal(";2;");
            w.number(color.r);
            w.byte(';');
            w.number(color.g);
            w.byte(';');
            w.number(color.b);
            break;
        default:
            w.number(foreground ? 39 : 49);
            break;
    }
}

const std::uint32_t kAttrParams[] = {1, 2, 3, 5, 7, 8, 9, 53};
const std::uint32_t kAttrBits[] = {
    SUPRA_ANSI_ATTR_BOLD,   SUPRA_ANSI_ATTR_DIM,    SUPRA_ANSI_ATTR_ITALIC,
    SUPRA_ANSI_ATTR_BLINK,  SUPRA_ANSI_ATTR_INVERSE, SUPRA_ANSI_ATTR_HIDDEN,
    SUPRA_ANSI_ATTR_STRIKE, SUPRA_ANSI_ATTR_OVERLINE,
};

/// Complement of an attribute mask, as an unsigned value.
///
/// The enum constants promote to `int`, so a bare `~SUPRA_ANSI_ATTR_BOLD` is
/// signed and assigning it back into `attrs` is a signedness conversion.
/// Routing through this keeps -Wsign-conversion meaningful instead of silencing
/// it at each use site.
[[nodiscard]] constexpr std::uint32_t without(std::uint32_t bits) noexcept {
    return ~bits;
}

}  // namespace

extern "C" {

supra_ansi_style supra_ansi_style_default(void) {
    supra_ansi_style style{};
    style.fg.kind = SUPRA_ANSI_COLOR_DEFAULT;
    style.bg.kind = SUPRA_ANSI_COLOR_DEFAULT;
    style.underline_color.kind = SUPRA_ANSI_COLOR_DEFAULT;
    style.underline = SUPRA_ANSI_UNDERLINE_NONE;
    style.hyperlink_open = 0;
    style.attrs = 0;
    return style;
}

int supra_ansi_style_is_default(const supra_ansi_style* style) {
    if (style == nullptr) {
        return 1;
    }
    const supra_ansi_style def = supra_ansi_style_default();
    return style->attrs == def.attrs && style->underline == def.underline &&
                   style->hyperlink_open == def.hyperlink_open &&
                   colorEqual(style->fg, def.fg) && colorEqual(style->bg, def.bg) &&
                   colorEqual(style->underline_color, def.underline_color)
               ? 1
               : 0;
}

int supra_ansi_style_apply(supra_ansi_style* style, const supra_ansi_token* token) {
    if (style == nullptr || token == nullptr) {
        return 0;
    }
    // SGR only: final byte `m` with no intermediates. A private-mode marker
    // makes it a different sequence entirely.
    if (token->kind != SUPRA_ANSI_TOKEN_CSI || token->final_byte != 'm' ||
        token->intermediate_count != 0) {
        return 0;
    }

    const supra_ansi_style before = *style;

    // `ESC [ m` with no parameters is a full reset.
    if (token->param_count == 0) {
        const std::uint8_t link = style->hyperlink_open;
        *style = supra_ansi_style_default();
        // SGR does not affect hyperlinks; only OSC 8 opens or closes one.
        style->hyperlink_open = link;
        return supra_ansi_style_is_default(&before) == 0 ? 1 : 0;
    }

    std::size_t i = 0;
    while (i < token->param_count) {
        const std::int32_t p = paramOr0(*token, i);
        std::size_t step = 1;

        switch (p) {
            case 0: {
                const std::uint8_t link = style->hyperlink_open;
                *style = supra_ansi_style_default();
                style->hyperlink_open = link;
                break;
            }

            case 1:
                style->attrs |= SUPRA_ANSI_ATTR_BOLD;
                break;
            case 2:
                style->attrs |= SUPRA_ANSI_ATTR_DIM;
                break;
            case 3:
                style->attrs |= SUPRA_ANSI_ATTR_ITALIC;
                break;

            case 4: {
                // SGR 4 takes a sub-parameter: 4:0 off, 4:1 single, 4:2 double,
                // 4:3 curly, 4:4 dotted, 4:5 dashed.
                if (token->subparam_count[i] > 0) {
                    const std::int32_t sub = token->subparams[token->subparam_index[i]];
                    const std::int32_t kind = sub == kParamOmitted ? 1 : sub;
                    style->underline = static_cast<std::uint8_t>(
                        kind >= 0 && kind <= SUPRA_ANSI_UNDERLINE_DASHED ? kind
                                                                         : SUPRA_ANSI_UNDERLINE_SINGLE);
                } else {
                    style->underline = SUPRA_ANSI_UNDERLINE_SINGLE;
                }
                break;
            }

            case 5:
            case 6:
                // 6 is rapid blink; folded into blink because no terminal in use
                // distinguishes them.
                style->attrs |= SUPRA_ANSI_ATTR_BLINK;
                break;
            case 7:
                style->attrs |= SUPRA_ANSI_ATTR_INVERSE;
                break;
            case 8:
                style->attrs |= SUPRA_ANSI_ATTR_HIDDEN;
                break;
            case 9:
                style->attrs |= SUPRA_ANSI_ATTR_STRIKE;
                break;

            case 21:
                style->underline = SUPRA_ANSI_UNDERLINE_DOUBLE;
                break;

            case 22:
                style->attrs &= without(SUPRA_ANSI_ATTR_BOLD | SUPRA_ANSI_ATTR_DIM);
                break;
            case 23:
                style->attrs &= without(SUPRA_ANSI_ATTR_ITALIC);
                break;
            case 24:
                style->underline = SUPRA_ANSI_UNDERLINE_NONE;
                break;
            case 25:
                style->attrs &= without(SUPRA_ANSI_ATTR_BLINK);
                break;
            case 27:
                style->attrs &= without(SUPRA_ANSI_ATTR_INVERSE);
                break;
            case 28:
                style->attrs &= without(SUPRA_ANSI_ATTR_HIDDEN);
                break;
            case 29:
                style->attrs &= without(SUPRA_ANSI_ATTR_STRIKE);
                break;

            case 38: {
                supra_ansi_color color{};
                if (readExtendedColor(*token, i, color, step)) {
                    style->fg = color;
                }
                break;
            }
            case 39:
                style->fg = supra_ansi_color{};
                style->fg.kind = SUPRA_ANSI_COLOR_DEFAULT;
                break;

            case 48: {
                supra_ansi_color color{};
                if (readExtendedColor(*token, i, color, step)) {
                    style->bg = color;
                }
                break;
            }
            case 49:
                style->bg = supra_ansi_color{};
                style->bg.kind = SUPRA_ANSI_COLOR_DEFAULT;
                break;

            case 53:
                style->attrs |= SUPRA_ANSI_ATTR_OVERLINE;
                break;
            case 55:
                style->attrs &= without(SUPRA_ANSI_ATTR_OVERLINE);
                break;

            case 58: {
                // Underline colour, Kitty and VTE extension.
                supra_ansi_color color{};
                if (readExtendedColor(*token, i, color, step)) {
                    style->underline_color = color;
                }
                break;
            }
            case 59:
                style->underline_color = supra_ansi_color{};
                style->underline_color.kind = SUPRA_ANSI_COLOR_DEFAULT;
                break;

            default: {
                if (p >= 30 && p <= 37) {
                    style->fg.kind = SUPRA_ANSI_COLOR_INDEXED;
                    style->fg.index = static_cast<std::uint8_t>(p - 30);
                } else if (p >= 40 && p <= 47) {
                    style->bg.kind = SUPRA_ANSI_COLOR_INDEXED;
                    style->bg.index = static_cast<std::uint8_t>(p - 40);
                } else if (p >= 90 && p <= 97) {
                    style->fg.kind = SUPRA_ANSI_COLOR_INDEXED;
                    style->fg.index = static_cast<std::uint8_t>(p - 90 + 8);
                } else if (p >= 100 && p <= 107) {
                    style->bg.kind = SUPRA_ANSI_COLOR_INDEXED;
                    style->bg.index = static_cast<std::uint8_t>(p - 100 + 8);
                }
                // Unknown parameters are ignored rather than treated as an
                // error: terminals keep adding them, and a strict parser would
                // discard the whole sequence over one unrecognised value.
                break;
            }
        }

        i += step == 0 ? 1 : step;
    }

    const bool changed = before.attrs != style->attrs || before.underline != style->underline ||
                         !colorEqual(before.fg, style->fg) || !colorEqual(before.bg, style->bg) ||
                         !colorEqual(before.underline_color, style->underline_color);
    return changed ? 1 : 0;
}

int supra_ansi_style_apply_osc(supra_ansi_style* style, const supra_ansi_token* token) {
    if (style == nullptr || token == nullptr || token->kind != SUPRA_ANSI_TOKEN_OSC) {
        return 0;
    }

    // OSC 8 ; params ; uri  - an empty URI closes the link.
    if (token->payload_len < 2 || token->payload[0] != '8' || token->payload[1] != ';') {
        return 0;
    }

    // Find the second `;`, after which the URI begins.
    std::size_t uri_start = 0;
    std::size_t semicolons = 0;
    for (std::size_t i = 0; i < token->payload_len; ++i) {
        if (token->payload[i] == ';') {
            ++semicolons;
            if (semicolons == 2) {
                uri_start = i + 1;
                break;
            }
        }
    }

    if (semicolons < 2) {
        return 0;
    }

    const std::uint8_t was_open = style->hyperlink_open;
    style->hyperlink_open = uri_start < token->payload_len ? 1 : 0;
    return was_open != style->hyperlink_open ? 1 : 0;
}

std::size_t supra_ansi_style_emit(const supra_ansi_style* style, std::uint8_t* out,
                                  std::size_t cap) {
    if (style == nullptr) {
        return 0;
    }
    // A default style needs no sequence. Emitting a reset anyway would cost 4
    // bytes per line per frame for no effect.
    if (supra_ansi_style_is_default(style) != 0) {
        return 0;
    }

    Writer w{out, cap};
    w.literal("\x1b[0");

    for (std::size_t i = 0; i < sizeof(kAttrBits) / sizeof(kAttrBits[0]); ++i) {
        if ((style->attrs & kAttrBits[i]) != 0) {
            w.byte(';');
            w.number(kAttrParams[i]);
        }
    }

    if (style->underline != SUPRA_ANSI_UNDERLINE_NONE) {
        w.byte(';');
        if (style->underline == SUPRA_ANSI_UNDERLINE_SINGLE) {
            w.number(4);
        } else {
            // Non-single styles need the sub-parameter form.
            w.literal("4:");
            w.number(style->underline);
        }
    }

    if (style->fg.kind != SUPRA_ANSI_COLOR_DEFAULT) {
        w.byte(';');
        emitColor(w, style->fg, true);
    }
    if (style->bg.kind != SUPRA_ANSI_COLOR_DEFAULT) {
        w.byte(';');
        emitColor(w, style->bg, false);
    }
    if (style->underline_color.kind != SUPRA_ANSI_COLOR_DEFAULT) {
        w.byte(';');
        w.number(58);
        if (style->underline_color.kind == SUPRA_ANSI_COLOR_INDEXED) {
            w.literal(";5;");
            w.number(style->underline_color.index);
        } else {
            w.literal(";2;");
            w.number(style->underline_color.r);
            w.byte(';');
            w.number(style->underline_color.g);
            w.byte(';');
            w.number(style->underline_color.b);
        }
    }

    w.byte('m');
    return w.written;
}

std::size_t supra_ansi_style_emit_reset(const supra_ansi_style* style, std::uint8_t* out,
                                        std::size_t cap) {
    if (style == nullptr) {
        return 0;
    }

    Writer w{out, cap};

    // A hyperlink left open makes every subsequent cell clickable, so it must be
    // closed before the SGR reset.
    if (style->hyperlink_open != 0) {
        w.literal("\x1b]8;;\x1b\\");
    }

    const bool needs_sgr_reset = style->attrs != 0 ||
                                 style->underline != SUPRA_ANSI_UNDERLINE_NONE ||
                                 style->fg.kind != SUPRA_ANSI_COLOR_DEFAULT ||
                                 style->bg.kind != SUPRA_ANSI_COLOR_DEFAULT ||
                                 style->underline_color.kind != SUPRA_ANSI_COLOR_DEFAULT;
    if (needs_sgr_reset) {
        w.literal("\x1b[0m");
    }

    return w.written;
}

}  // extern "C"
