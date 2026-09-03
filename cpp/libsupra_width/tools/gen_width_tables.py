#!/usr/bin/env python3
"""Generate the static width and grapheme tables for libsupra_width.

Reads the vendored UCD extracts in data/ and emits src/tables_generated.cpp:
sorted, non-overlapping, coalesced range tables that the runtime binary-searches.

Design notes that matter for correctness:

* Every table is emitted sorted and coalesced, and the generator asserts both
  properties before writing. Binary search over an unsorted table returns
  plausible wrong answers rather than failing, so this is checked here rather
  than debugged later.

* East Asian Ambiguous is emitted as its own table, not folded into Wide. The
  runtime resolves it from a locale flag, because the same code point is one
  cell in a Latin locale and two in a CJK locale. Every terminal UI bug caused
  by a "nice" glyph comes from this class.

* Zero-width covers Mn/Me/Cf plus the explicit UAX #29 Extend class. They
  overlap heavily but neither is a subset of the other.

Run after tools/fetch_ucd.py. Review the diff: a table change is a rendering
change.

Usage: python3 tools/gen_width_tables.py
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DATA = ROOT / "data"
OUT = ROOT / "src" / "tables_generated.cpp"

Range = tuple[int, int]

RANGE_RE = re.compile(
    r"^([0-9A-Fa-f]{4,6})(?:\.\.([0-9A-Fa-f]{4,6}))?\s*;\s*([^#]*?)\s*(?:#.*)?$"
)


def parse(path: Path, wanted: str, field: int = 0) -> list[Range]:
    """Collect ranges whose semicolon-separated property field equals `wanted`."""
    out: list[Range] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        match = RANGE_RE.match(line)
        if not match:
            continue
        lo = int(match.group(1), 16)
        hi = int(match.group(2), 16) if match.group(2) else lo
        fields = [f.strip() for f in match.group(3).split(";")]
        if field >= len(fields):
            continue
        if fields[field] == wanted:
            out.append((lo, hi))
    return out


def coalesce(ranges: list[Range]) -> list[Range]:
    """Sort and merge adjacent or overlapping ranges."""
    if not ranges:
        return []
    ranges = sorted(ranges)
    merged = [ranges[0]]
    for lo, hi in ranges[1:]:
        prev_lo, prev_hi = merged[-1]
        if lo <= prev_hi + 1:
            merged[-1] = (prev_lo, max(prev_hi, hi))
        else:
            merged.append((lo, hi))
    return merged


def check(name: str, ranges: list[Range]) -> None:
    """Assert the invariants the runtime binary search depends on."""
    for i, (lo, hi) in enumerate(ranges):
        if lo > hi:
            raise AssertionError(f"{name}[{i}]: inverted range U+{lo:04X}..U+{hi:04X}")
        if hi > 0x10FFFF:
            raise AssertionError(f"{name}[{i}]: beyond Unicode range U+{hi:04X}")
        if i and ranges[i - 1][1] + 1 >= lo:
            raise AssertionError(f"{name}[{i}]: not coalesced after U+{ranges[i - 1][1]:04X}")


def emit(name: str, ranges: list[Range]) -> str:
    check(name, ranges)
    lines = [
        f"// {len(ranges)} ranges",
        f"const Range k{name}[] = {{",
    ]
    for i in range(0, len(ranges), 4):
        chunk = ranges[i : i + 4]
        cells = ", ".join(f"{{0x{lo:05X}, 0x{hi:05X}}}" for lo, hi in chunk)
        lines.append(f"    {cells},")
    lines.append("};")
    lines.append(f"const std::size_t k{name}Count = {len(ranges)};")
    return "\n".join(lines)


# Hangul syllable block, from UAX #29 and the Unicode core spec.
HANGUL_SBASE = 0xAC00
HANGUL_SCOUNT = 11172
HANGUL_TCOUNT = 28


def verify_hangul_arithmetic(lv: list[Range], lvt: list[Range]) -> None:
    """Confirm the modulo derivation reproduces the UCD LV/LVT assignments.

    The runtime derives LV/LVT arithmetically instead of carrying 798 table
    ranges. That is only sound if the derivation matches the data exactly, so
    it is checked here on every regeneration rather than assumed to hold in a
    future Unicode version.
    """
    expanded_lv = {cp for lo, hi in lv for cp in range(lo, hi + 1)}
    expanded_lvt = {cp for lo, hi in lvt for cp in range(lo, hi + 1)}

    derived_lv: set[int] = set()
    derived_lvt: set[int] = set()
    for cp in range(HANGUL_SBASE, HANGUL_SBASE + HANGUL_SCOUNT):
        if (cp - HANGUL_SBASE) % HANGUL_TCOUNT == 0:
            derived_lv.add(cp)
        else:
            derived_lvt.add(cp)

    if derived_lv != expanded_lv:
        raise AssertionError(
            f"Hangul LV derivation diverged from UCD: "
            f"{len(derived_lv ^ expanded_lv)} code points differ"
        )
    if derived_lvt != expanded_lvt:
        raise AssertionError(
            f"Hangul LVT derivation diverged from UCD: "
            f"{len(derived_lvt ^ expanded_lvt)} code points differ"
        )


def main() -> int:
    version = (DATA / "VERSION").read_text(encoding="utf-8").strip()

    eaw = DATA / "EastAsianWidth.txt"
    gcb = DATA / "GraphemeBreakProperty.txt"
    emoji = DATA / "emoji-data.txt"
    gc = DATA / "DerivedGeneralCategory.txt"
    dcp = DATA / "DerivedCoreProperties.txt"

    # Width classes -----------------------------------------------------------
    # W and F are unconditionally two cells. A is resolved at runtime.
    wide = coalesce(parse(eaw, "W") + parse(eaw, "F"))
    ambiguous = coalesce(parse(eaw, "A"))

    # Zero width: nonspacing and enclosing marks, format characters, and the
    # UAX #29 Extend class. Overlapping but mutually non-subsuming.
    zero = coalesce(
        parse(gc, "Mn") + parse(gc, "Me") + parse(gc, "Cf") + parse(gcb, "Extend")
    )

    # Emoji ------------------------------------------------------------------
    # Emoji_Presentation is default-emoji and therefore two cells. Extended
    # Pictographic is needed for the GB11 ZWJ rule, and Emoji_Modifier for the
    # skin-tone sequences that must not add width.
    emoji_presentation = coalesce(parse(emoji, "Emoji_Presentation"))
    extended_pictographic = coalesce(parse(emoji, "Extended_Pictographic"))
    emoji_modifier = coalesce(parse(emoji, "Emoji_Modifier"))
    emoji_base = coalesce(parse(emoji, "Emoji"))

    # Grapheme cluster break classes (UAX #29) --------------------------------
    prepend = coalesce(parse(gcb, "Prepend"))
    spacing_mark = coalesce(parse(gcb, "SpacingMark"))
    regional = coalesce(parse(gcb, "Regional_Indicator"))
    hangul_l = coalesce(parse(gcb, "L"))
    hangul_v = coalesce(parse(gcb, "V"))
    hangul_t = coalesce(parse(gcb, "T"))

    # LV and LVT are deliberately NOT emitted as tables. The 11 172 precomposed
    # syllables occupy one contiguous block (AC00..D7A3) with a regular period
    # of 28, so the runtime derives both classes arithmetically:
    #
    #     s = cp - 0xAC00;  LV when s % 28 == 0, LVT otherwise
    #
    # As tables they cost 798 ranges - 31% of the total - for information a
    # modulo already carries. tests/hangul_arithmetic_test.cpp checks the
    # derivation against these UCD assignments so the shortcut stays honest.
    hangul_lv_reference = coalesce(parse(gcb, "LV"))
    hangul_lvt_reference = coalesce(parse(gcb, "LVT"))
    verify_hangul_arithmetic(hangul_lv_reference, hangul_lvt_reference)

    # Indic conjunct break, for GB9c. Field 1 because lines read
    # "<range> ; InCB; <value>".
    incb_linker = coalesce(parse(dcp, "Linker", field=1))
    incb_consonant = coalesce(parse(dcp, "Consonant", field=1))
    incb_extend = coalesce(parse(dcp, "Extend", field=1))

    tables = [
        ("Wide", wide),
        ("Ambiguous", ambiguous),
        ("ZeroWidth", zero),
        ("EmojiPresentation", emoji_presentation),
        ("ExtendedPictographic", extended_pictographic),
        ("EmojiModifier", emoji_modifier),
        ("Emoji", emoji_base),
        ("Prepend", prepend),
        ("SpacingMark", spacing_mark),
        ("RegionalIndicator", regional),
        ("HangulL", hangul_l),
        ("HangulV", hangul_v),
        ("HangulT", hangul_t),
        ("IncbLinker", incb_linker),
        ("IncbConsonant", incb_consonant),
        ("IncbExtend", incb_extend),
    ]

    body = "\n\n".join(emit(name, ranges) for name, ranges in tables)

    OUT.write_text(
        f"""// GENERATED FILE - DO NOT EDIT.
//
// Source: Unicode Character Database {version} (vendored under data/).
// Regenerate: python3 tools/gen_width_tables.py
//
// Every table is sorted, non-overlapping, and coalesced; the generator asserts
// all three before writing, because binary search over an unsorted table
// returns a plausible wrong answer instead of failing.
//
// Hangul LV/LVT are absent by design: the runtime derives them arithmetically
// from the AC00..D7A3 block, and the generator verifies that derivation
// against the UCD on every regeneration.

#include "tables.hpp"

namespace supra::width::tables {{

const char* kUnicodeVersion = "{version}";

{body}

}}  // namespace supra::width::tables
""",
        encoding="utf-8",
    )

    total = sum(len(r) for _, r in tables)
    print(f"wrote {OUT.relative_to(ROOT)}", file=sys.stderr)
    print(f"  Unicode {version}, {len(tables)} tables, {total} ranges", file=sys.stderr)
    for name, ranges in tables:
        print(f"  {name:22s} {len(ranges):5d}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
