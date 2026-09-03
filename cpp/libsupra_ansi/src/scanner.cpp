// Escape sequence scanner: a resumable state machine over the DEC STD 070 /
// VT500 grammar.
//
// Two properties drive the design.
//
// Resumable, because shell output arrives in chunks and an escape sequence can
// straddle a chunk boundary. A stateless matcher sees two fragments and mangles
// both, which corrupts exactly the output that matters - the progress line, the
// coloured error - at exactly the moment it is written.
//
// Total, because the input is untrusted. Every byte is consumed by some
// transition, every scan advances at least one byte, and no input is rejected.
// A parser that could stall would hang the renderer on adversarial tool output
// rather than merely mis-render it.

#include <cstddef>
#include <cstdint>
#include <cstring>

#include "internal.hpp"
#include "supra/ansi.h"

namespace {

using namespace supra::ansi::detail;

/// Reset the accumulators a new sequence needs, leaving `state` and `pos`
/// untouched.
void clearSequence(supra_ansi_scanner& sc) noexcept {
    sc.intermediate_count = 0;
    sc.intermediates[0] = 0;
    sc.intermediates[1] = 0;
    sc.param_count = 0;
    sc.params_dropped = 0;
    sc.param_has_digits = 0;
    sc.subparam_total = 0;
    sc.payload_len = 0;
    sc.payload_full = 0;
    for (std::size_t i = 0; i < SUPRA_ANSI_MAX_PARAMS; ++i) {
        sc.params[i] = 0;
        sc.subparam_index[i] = 0;
        sc.subparam_count[i] = 0;
    }
}

/// Open a parameter slot, defaulting to "omitted" until a digit arrives.
void beginParam(supra_ansi_scanner& sc) noexcept {
    if (sc.param_count < SUPRA_ANSI_MAX_PARAMS) {
        sc.params[sc.param_count] = kParamOmitted;
        sc.subparam_index[sc.param_count] = sc.subparam_total;
        sc.subparam_count[sc.param_count] = 0;
    }
    sc.param_has_digits = 0;
    sc.in_subparam = 0;
}

/// Finish the current parameter slot.
void endParam(supra_ansi_scanner& sc) noexcept {
    if (sc.param_count < SUPRA_ANSI_MAX_PARAMS) {
        ++sc.param_count;
    } else {
        sc.params_dropped = 1;
    }
    sc.param_has_digits = 0;
    sc.in_subparam = 0;
}

void accumulateDigit(supra_ansi_scanner& sc, std::uint8_t digit) noexcept {
    if (sc.param_count >= SUPRA_ANSI_MAX_PARAMS) {
        sc.params_dropped = 1;
        return;
    }
    std::int32_t value = sc.param_has_digits ? sc.params[sc.param_count] : 0;
    // Saturate rather than overflow: `ESC [ 99999999999999999999 m` must be
    // consumed correctly, and no sequence has a meaningful parameter this large.
    if (value <= kParamMax) {
        value = (value * 10) + static_cast<std::int32_t>(digit - '0');
        if (value > kParamMax) {
            value = kParamMax;
        }
    }
    sc.params[sc.param_count] = value;
    sc.param_has_digits = 1;
}

/// Begin a sub-parameter, as in `ESC [ 4:3 m` or `ESC [ 38:2::255:0:0 m`.
///
/// Sub-parameters are separated by `:` rather than `;`. A parser that splits
/// only on `;` reads `38:2::255:0:0` as one giant parameter and gets the colour
/// wrong.
///
/// An empty sub-parameter is legal and meaningful: the `::` in
/// `38:2::255:0:0` is the omitted colour-space id.
void beginSubparam(supra_ansi_scanner& sc) noexcept {
    if (sc.param_count < SUPRA_ANSI_MAX_PARAMS && sc.subparam_total < SUPRA_ANSI_MAX_SUBPARAMS) {
        sc.subparams[sc.subparam_total] = kParamOmitted;
        ++sc.subparam_total;
        ++sc.subparam_count[sc.param_count];
    }
    sc.param_has_digits = 0;
    sc.in_subparam = 1;
}

void accumulateSubparamDigit(supra_ansi_scanner& sc, std::uint8_t digit) noexcept {
    if (sc.subparam_total == 0) {
        return;
    }
    const std::size_t slot = sc.subparam_total - 1;
    std::int32_t value = sc.param_has_digits ? sc.subparams[slot] : 0;
    if (value <= kParamMax) {
        value = (value * 10) + static_cast<std::int32_t>(digit - '0');
        if (value > kParamMax) {
            value = kParamMax;
        }
    }
    sc.subparams[slot] = value;
    sc.param_has_digits = 1;
}

void appendPayload(supra_ansi_scanner& sc, std::uint8_t byte) noexcept {
    ++sc.payload_full;
    if (sc.payload_len < SUPRA_ANSI_MAX_PAYLOAD) {
        sc.payload[sc.payload_len] = byte;
        ++sc.payload_len;
    }
}

void addIntermediate(supra_ansi_scanner& sc, std::uint8_t byte) noexcept {
    if (sc.intermediate_count < 2) {
        sc.intermediates[sc.intermediate_count] = byte;
        ++sc.intermediate_count;
    }
}

/// Publish the accumulated state as a token.
void emit(const supra_ansi_scanner& sc, supra_ansi_token& out, supra_ansi_token_kind kind,
          std::size_t offset, std::size_t length, std::uint8_t final_byte) noexcept {
    out.kind = static_cast<std::uint8_t>(kind);
    out.offset = offset;
    out.length = length;
    out.final_byte = final_byte;

    out.intermediate_count = sc.intermediate_count;
    out.intermediates[0] = sc.intermediates[0];
    out.intermediates[1] = sc.intermediates[1];

    out.param_count = sc.param_count;
    out.params_dropped = sc.params_dropped;
    for (std::size_t i = 0; i < SUPRA_ANSI_MAX_PARAMS; ++i) {
        out.params[i] = sc.params[i];
        out.subparam_index[i] = sc.subparam_index[i];
        out.subparam_count[i] = sc.subparam_count[i];
    }
    for (std::size_t i = 0; i < SUPRA_ANSI_MAX_SUBPARAMS; ++i) {
        out.subparams[i] = sc.subparams[i];
    }

    out.payload_len = sc.payload_len;
    out.payload_full = sc.payload_full;
    if (sc.payload_len > 0) {
        std::memcpy(out.payload, sc.payload, sc.payload_len);
    }
}

void emitSimple(supra_ansi_token& out, supra_ansi_token_kind kind, std::size_t offset,
                std::size_t length, std::uint8_t final_byte) noexcept {
    out = supra_ansi_token{};
    out.kind = static_cast<std::uint8_t>(kind);
    out.offset = offset;
    out.length = length;
    out.final_byte = final_byte;
}

}  // namespace

extern "C" {

void supra_ansi_scanner_init(supra_ansi_scanner* scanner) {
    if (scanner == nullptr) {
        return;
    }
    *scanner = supra_ansi_scanner{};
}

int supra_ansi_scanner_pending(const supra_ansi_scanner* scanner) {
    if (scanner == nullptr) {
        return 0;
    }
    return static_cast<State>(scanner->state) == State::Ground ? 0 : 1;
}

int supra_ansi_scan(supra_ansi_scanner* scanner, const std::uint8_t* bytes, std::size_t len,
                    supra_ansi_eof eof, supra_ansi_token* out) {
    if (scanner == nullptr || out == nullptr) {
        return 0;
    }

    supra_ansi_scanner& sc = *scanner;

    // This buffer has been consumed. Two ways to get here: a previous call ran
    // to the end mid-sequence and set `buffer_done`, or a token ended exactly at
    // the last byte.
    //
    // Resetting `pos` here rather than at the bottom is what makes the caller's
    // `while (scan(...))` loop terminate: a call that reports a token must leave
    // the cursor at the end, so the next call falls into this branch and returns
    // 0 instead of rescanning bytes it already produced a token for.
    if (sc.buffer_done != 0 || bytes == nullptr || sc.pos >= len) {
        sc.buffer_done = 0;
        sc.pos = 0;
        // At end of stream an unterminated sequence becomes MALFORMED rather
        // than PARTIAL: a caller must not be left waiting for a terminator that
        // will never arrive.
        if (eof == SUPRA_ANSI_FINAL && static_cast<State>(sc.state) != State::Ground) {
            emit(sc, *out, SUPRA_ANSI_TOKEN_MALFORMED, 0, 0, 0);
            out->length = 0;
            sc.state = static_cast<std::uint8_t>(State::Ground);
            sc.carried = 0;
            sc.utf8_expect = 0;
            clearSequence(sc);
            return 1;
        }
        return 0;
    }

    const std::size_t start = sc.pos;

    // Resuming mid-sequence: the token's reported length covers only bytes from
    // this buffer, and `carried` records what earlier buffers already consumed.
    const bool resuming = static_cast<State>(sc.state) != State::Ground;
    const std::size_t token_start = resuming ? 0 : start;

    while (sc.pos < len) {
        const std::uint8_t byte = bytes[sc.pos];
        const State state = static_cast<State>(sc.state);

        // CAN and SUB abort any sequence in progress, in every state.
        if (isCancel(byte) && state != State::Ground) {
            ++sc.pos;
            sc.state = static_cast<std::uint8_t>(State::Ground);
            sc.carried = 0;
            emit(sc, *out, SUPRA_ANSI_TOKEN_MALFORMED, token_start, sc.pos - token_start, byte);
            clearSequence(sc);
            return 1;
        }

        switch (state) {
            case State::Ground: {
                // A printable run.
                //
                // Advancing must respect UTF-8 structure, because the C1 control
                // range (0x80..0x9F) sits inside the continuation-byte range
                // (0x80..0xBF). U+6587 encodes as `E6 96 87`, whose second byte
                // is the C1 code for START OF GUARDED AREA; U+1F600 contains
                // three such bytes. A byte-wise C1 test tears both apart.
                //
                // `utf8_expect` carries the count of continuation bytes still
                // owed, so the distinction survives a chunk boundary landing
                // mid-character.
                const bool mid_character = sc.utf8_expect > 0 && isContinuation(byte);
                if (byte != kEsc && byte != 0x7F && (mid_character || !isC1(byte)) &&
                    (byte >= 0x20 || isTextControl(byte))) {
                    std::size_t end = sc.pos;
                    while (end < len) {
                        const std::uint8_t b = bytes[end];

                        if (sc.utf8_expect > 0) {
                            if (!isContinuation(b)) {
                                // Truncated sequence; resynchronise here.
                                sc.utf8_expect = 0;
                                continue;
                            }
                            --sc.utf8_expect;
                            ++end;
                            continue;
                        }

                        if (b == kEsc || b == 0x7F || (b < 0x20 && !isTextControl(b))) {
                            break;
                        }
                        // At a scalar boundary, so a byte in this range really is
                        // a C1 control.
                        if (isC1(b)) {
                            break;
                        }

                        if (b >= 0xC2 && b <= 0xDF) {
                            sc.utf8_expect = 1;
                        } else if (b >= 0xE0 && b <= 0xEF) {
                            sc.utf8_expect = 2;
                        } else if (b >= 0xF0 && b <= 0xF4) {
                            sc.utf8_expect = 3;
                        }
                        // Anything else - ASCII, or an invalid lead - consumes one
                        // byte and expects no continuation.
                        ++end;
                    }
                    emitSimple(*out, SUPRA_ANSI_TOKEN_TEXT, sc.pos, end - sc.pos, 0);
                    sc.pos = end;
                    return 1;
                }

                if (byte == kEsc) {
                    ++sc.pos;
                    clearSequence(sc);
                    sc.state = static_cast<std::uint8_t>(State::Escape);
                    break;
                }

                // 8-bit C1 introducers: no ESC byte, which is why a regex-based
                // matcher never sees them.
                if (isC1(byte)) {
                    ++sc.pos;
                    clearSequence(sc);
                    switch (byte) {
                        case kC1Csi:
                            beginParam(sc);
                            sc.state = static_cast<std::uint8_t>(State::CsiEntry);
                            break;
                        case kC1Osc:
                            sc.state = static_cast<std::uint8_t>(State::OscString);
                            break;
                        case kC1Dcs:
                            beginParam(sc);
                            sc.state = static_cast<std::uint8_t>(State::DcsEntry);
                            break;
                        case kC1Sos:
                        case kC1Pm:
                        case kC1Apc:
                            sc.state = static_cast<std::uint8_t>(State::ApcString);
                            break;
                        default:
                            emitSimple(*out, SUPRA_ANSI_TOKEN_CONTROL, sc.pos - 1, 1, byte);
                            return 1;
                    }
                    break;
                }

                // A lone C0 control or DEL.
                ++sc.pos;
                emitSimple(*out, SUPRA_ANSI_TOKEN_CONTROL, sc.pos - 1, 1, byte);
                return 1;
            }

            case State::Escape: {
                ++sc.pos;
                if (byte == '[') {
                    beginParam(sc);
                    sc.state = static_cast<std::uint8_t>(State::CsiEntry);
                    break;
                }
                if (byte == ']') {
                    sc.state = static_cast<std::uint8_t>(State::OscString);
                    break;
                }
                if (byte == 'P') {
                    beginParam(sc);
                    sc.state = static_cast<std::uint8_t>(State::DcsEntry);
                    break;
                }
                if (byte == 'X' || byte == '^' || byte == '_') {
                    sc.state = static_cast<std::uint8_t>(State::ApcString);
                    break;
                }
                if (isIntermediateByte(byte)) {
                    addIntermediate(sc, byte);
                    sc.state = static_cast<std::uint8_t>(State::EscapeIntermediate);
                    break;
                }
                if (byte >= 0x30 && byte <= 0x7E) {
                    sc.state = static_cast<std::uint8_t>(State::Ground);
                    sc.carried = 0;
                    emit(sc, *out, SUPRA_ANSI_TOKEN_ESC, token_start, sc.pos - token_start, byte);
                    clearSequence(sc);
                    return 1;
                }
                // ESC followed by something outside the grammar.
                sc.state = static_cast<std::uint8_t>(State::Ground);
                sc.carried = 0;
                emit(sc, *out, SUPRA_ANSI_TOKEN_MALFORMED, token_start, sc.pos - token_start, byte);
                clearSequence(sc);
                return 1;
            }

            case State::EscapeIntermediate: {
                ++sc.pos;
                if (isIntermediateByte(byte)) {
                    addIntermediate(sc, byte);
                    break;
                }
                if (byte >= 0x30 && byte <= 0x7E) {
                    sc.state = static_cast<std::uint8_t>(State::Ground);
                    sc.carried = 0;
                    emit(sc, *out, SUPRA_ANSI_TOKEN_ESC, token_start, sc.pos - token_start, byte);
                    clearSequence(sc);
                    return 1;
                }
                sc.state = static_cast<std::uint8_t>(State::Ground);
                sc.carried = 0;
                emit(sc, *out, SUPRA_ANSI_TOKEN_MALFORMED, token_start, sc.pos - token_start, byte);
                clearSequence(sc);
                return 1;
            }

            case State::CsiEntry:
            case State::CsiParam: {
                ++sc.pos;
                if (byte >= '0' && byte <= '9') {
                    // Digits belong to whichever slot is open: a sub-parameter
                    // when the last separator was `:`, otherwise the parameter.
                    if (sc.in_subparam != 0) {
                        accumulateSubparamDigit(sc, byte);
                    } else {
                        accumulateDigit(sc, byte);
                    }
                    sc.state = static_cast<std::uint8_t>(State::CsiParam);
                    break;
                }
                if (byte == ';') {
                    endParam(sc);
                    beginParam(sc);
                    sc.state = static_cast<std::uint8_t>(State::CsiParam);
                    break;
                }
                if (byte == ':') {
                    beginSubparam(sc);
                    sc.state = static_cast<std::uint8_t>(State::CsiParam);
                    break;
                }
                // Private-mode markers `< = > ?`, as in `ESC [ ? 25 h`.
                if (byte >= 0x3C && byte <= 0x3F) {
                    addIntermediate(sc, byte);
                    sc.state = static_cast<std::uint8_t>(State::CsiParam);
                    break;
                }
                if (isIntermediateByte(byte)) {
                    endParam(sc);
                    addIntermediate(sc, byte);
                    sc.state = static_cast<std::uint8_t>(State::CsiIntermediate);
                    break;
                }
                if (isFinalByte(byte)) {
                    endParam(sc);
                    sc.state = static_cast<std::uint8_t>(State::Ground);
                    sc.carried = 0;
                    emit(sc, *out, SUPRA_ANSI_TOKEN_CSI, token_start, sc.pos - token_start, byte);
                    clearSequence(sc);
                    return 1;
                }
                sc.state = static_cast<std::uint8_t>(State::CsiIgnore);
                break;
            }

            case State::CsiIntermediate: {
                ++sc.pos;
                if (isIntermediateByte(byte)) {
                    addIntermediate(sc, byte);
                    break;
                }
                if (isFinalByte(byte)) {
                    sc.state = static_cast<std::uint8_t>(State::Ground);
                    sc.carried = 0;
                    emit(sc, *out, SUPRA_ANSI_TOKEN_CSI, token_start, sc.pos - token_start, byte);
                    clearSequence(sc);
                    return 1;
                }
                sc.state = static_cast<std::uint8_t>(State::CsiIgnore);
                break;
            }

            case State::CsiIgnore: {
                ++sc.pos;
                if (isFinalByte(byte)) {
                    sc.state = static_cast<std::uint8_t>(State::Ground);
                    sc.carried = 0;
                    emit(sc, *out, SUPRA_ANSI_TOKEN_MALFORMED, token_start, sc.pos - token_start,
                         byte);
                    clearSequence(sc);
                    return 1;
                }
                break;
            }

            case State::OscString: {
                ++sc.pos;
                // BEL is a legal OSC terminator alongside ESC \. A parser that
                // knows only ST runs past the end of a hyperlink and eats the
                // text after it.
                if (byte == kBel) {
                    sc.state = static_cast<std::uint8_t>(State::Ground);
                    sc.carried = 0;
                    emit(sc, *out, SUPRA_ANSI_TOKEN_OSC, token_start, sc.pos - token_start, kBel);
                    clearSequence(sc);
                    return 1;
                }
                if (byte == kEsc) {
                    sc.state = static_cast<std::uint8_t>(State::OscEsc);
                    break;
                }
                if (byte == kC1St) {
                    sc.state = static_cast<std::uint8_t>(State::Ground);
                    sc.carried = 0;
                    emit(sc, *out, SUPRA_ANSI_TOKEN_OSC, token_start, sc.pos - token_start, kC1St);
                    clearSequence(sc);
                    return 1;
                }
                appendPayload(sc, byte);
                break;
            }

            case State::OscEsc: {
                ++sc.pos;
                if (byte == '\\') {
                    sc.state = static_cast<std::uint8_t>(State::Ground);
                    sc.carried = 0;
                    emit(sc, *out, SUPRA_ANSI_TOKEN_OSC, token_start, sc.pos - token_start, kC1St);
                    clearSequence(sc);
                    return 1;
                }
                // ESC inside the payload that was not a terminator: the ESC is
                // payload, and so is this byte.
                appendPayload(sc, kEsc);
                appendPayload(sc, byte);
                sc.state = static_cast<std::uint8_t>(State::OscString);
                break;
            }

            case State::DcsEntry:
            case State::DcsParam: {
                ++sc.pos;
                if (byte >= '0' && byte <= '9') {
                    accumulateDigit(sc, byte);
                    sc.state = static_cast<std::uint8_t>(State::DcsParam);
                    break;
                }
                if (byte == ';') {
                    endParam(sc);
                    beginParam(sc);
                    sc.state = static_cast<std::uint8_t>(State::DcsParam);
                    break;
                }
                if (byte >= 0x3C && byte <= 0x3F) {
                    addIntermediate(sc, byte);
                    sc.state = static_cast<std::uint8_t>(State::DcsParam);
                    break;
                }
                if (isIntermediateByte(byte)) {
                    endParam(sc);
                    addIntermediate(sc, byte);
                    sc.state = static_cast<std::uint8_t>(State::DcsIntermediate);
                    break;
                }
                if (isFinalByte(byte)) {
                    endParam(sc);
                    sc.intermediates[1] = byte;  // remember the final for the token
                    sc.state = static_cast<std::uint8_t>(State::DcsPassthrough);
                    break;
                }
                sc.state = static_cast<std::uint8_t>(State::DcsIgnore);
                break;
            }

            case State::DcsIntermediate: {
                ++sc.pos;
                if (isIntermediateByte(byte)) {
                    addIntermediate(sc, byte);
                    break;
                }
                if (isFinalByte(byte)) {
                    sc.intermediates[1] = byte;
                    sc.state = static_cast<std::uint8_t>(State::DcsPassthrough);
                    break;
                }
                sc.state = static_cast<std::uint8_t>(State::DcsIgnore);
                break;
            }

            case State::DcsPassthrough: {
                ++sc.pos;
                if (byte == kEsc) {
                    sc.state = static_cast<std::uint8_t>(State::DcsEsc);
                    break;
                }
                if (byte == kC1St) {
                    sc.state = static_cast<std::uint8_t>(State::Ground);
                    sc.carried = 0;
                    emit(sc, *out, SUPRA_ANSI_TOKEN_DCS, token_start, sc.pos - token_start,
                         sc.intermediates[1]);
                    clearSequence(sc);
                    return 1;
                }
                appendPayload(sc, byte);
                break;
            }

            case State::DcsEsc: {
                ++sc.pos;
                if (byte == '\\') {
                    sc.state = static_cast<std::uint8_t>(State::Ground);
                    sc.carried = 0;
                    emit(sc, *out, SUPRA_ANSI_TOKEN_DCS, token_start, sc.pos - token_start,
                         sc.intermediates[1]);
                    clearSequence(sc);
                    return 1;
                }
                appendPayload(sc, kEsc);
                appendPayload(sc, byte);
                sc.state = static_cast<std::uint8_t>(State::DcsPassthrough);
                break;
            }

            case State::DcsIgnore: {
                ++sc.pos;
                if (byte == kC1St) {
                    sc.state = static_cast<std::uint8_t>(State::Ground);
                    sc.carried = 0;
                    emit(sc, *out, SUPRA_ANSI_TOKEN_MALFORMED, token_start, sc.pos - token_start,
                         0);
                    clearSequence(sc);
                    return 1;
                }
                if (byte == kEsc) {
                    sc.state = static_cast<std::uint8_t>(State::DcsEsc);
                }
                break;
            }

            case State::ApcString: {
                ++sc.pos;
                if (byte == kBel) {
                    sc.state = static_cast<std::uint8_t>(State::Ground);
                    sc.carried = 0;
                    emit(sc, *out, SUPRA_ANSI_TOKEN_APC, token_start, sc.pos - token_start, kBel);
                    clearSequence(sc);
                    return 1;
                }
                if (byte == kEsc) {
                    sc.state = static_cast<std::uint8_t>(State::ApcEsc);
                    break;
                }
                if (byte == kC1St) {
                    sc.state = static_cast<std::uint8_t>(State::Ground);
                    sc.carried = 0;
                    emit(sc, *out, SUPRA_ANSI_TOKEN_APC, token_start, sc.pos - token_start, kC1St);
                    clearSequence(sc);
                    return 1;
                }
                appendPayload(sc, byte);
                break;
            }

            case State::ApcEsc: {
                ++sc.pos;
                if (byte == '\\') {
                    sc.state = static_cast<std::uint8_t>(State::Ground);
                    sc.carried = 0;
                    emit(sc, *out, SUPRA_ANSI_TOKEN_APC, token_start, sc.pos - token_start, kC1St);
                    clearSequence(sc);
                    return 1;
                }
                appendPayload(sc, kEsc);
                appendPayload(sc, byte);
                sc.state = static_cast<std::uint8_t>(State::ApcString);
                break;
            }
        }
    }

    // Buffer ended mid-sequence.
    //
    // `pos` stays at `len` and `buffer_done` is set rather than resetting the
    // cursor here. Resetting now would be wrong in two ways: the caller's drain
    // loop would rescan this buffer forever, and if the next chunk were longer
    // than this one, a stale non-zero cursor would skip its leading bytes.
    const std::size_t consumed = len - token_start;
    sc.buffer_done = 1;

    if (eof == SUPRA_ANSI_FINAL) {
        // No more input will arrive, so this is malformed rather than pending.
        emit(sc, *out, SUPRA_ANSI_TOKEN_MALFORMED, token_start, consumed, 0);
        sc.state = static_cast<std::uint8_t>(State::Ground);
        sc.carried = 0;
        sc.utf8_expect = 0;
        clearSequence(sc);
        return 1;
    }

    // Report what was consumed and keep the state for the next chunk.
    sc.carried += consumed;
    emitSimple(*out, SUPRA_ANSI_TOKEN_PARTIAL, token_start, consumed, 0);
    return 1;
}

}  // extern "C"
