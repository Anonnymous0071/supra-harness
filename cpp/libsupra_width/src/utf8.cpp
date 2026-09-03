// UTF-8 decoding and encoded-length queries.

#include <cstddef>
#include <cstdint>

#include "internal.hpp"
#include "supra/width.h"

namespace {
using supra::width::detail::decode;
using supra::width::detail::kMaxCodePoint;
using supra::width::detail::kSurrogateLo;
using supra::width::detail::kSurrogateHi;
}  // namespace

extern "C" {

int supra_utf8_decode(const std::uint8_t* bytes, std::size_t len, std::uint32_t* out_cp,
                      std::size_t* consumed) {
    // Output pointers are contractual; a NULL here is a caller bug that would
    // otherwise corrupt memory. Report failure rather than write through it.
    if (out_cp == nullptr || consumed == nullptr) {
        return 0;
    }

    if (bytes == nullptr || len == 0) {
        *out_cp = SUPRA_WIDTH_REPLACEMENT;
        *consumed = 0;
        return 0;
    }

    const auto decoded = decode(bytes, len);
    *out_cp = decoded.cp;
    *consumed = decoded.len;
    return decoded.valid ? 1 : 0;
}

std::size_t supra_utf8_encoded_len(std::uint32_t cp) {
    if (cp >= kSurrogateLo && cp <= kSurrogateHi) {
        return 0;
    }
    if (cp > kMaxCodePoint) {
        return 0;
    }
    if (cp < 0x80) {
        return 1;
    }
    if (cp < 0x800) {
        return 2;
    }
    if (cp < 0x10000) {
        return 3;
    }
    return 4;
}

}  // extern "C"
