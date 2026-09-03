#!/usr/bin/env python3
"""Fetch and trim the Unicode Character Database extracts libsupra_width needs.

Vendoring trimmed extracts rather than fetching at build time keeps the build
hermetic: `just build-cpp` must work offline, and a table regenerated from
different input than the committed one is a silent correctness change.

Only the property lines the generator consumes are kept, with the upstream
version header preserved for provenance. Run this to move to a new Unicode
version, then run gen_width_tables.py and review the diff.

Usage: python3 tools/fetch_ucd.py [--version 17.0.0]
"""

from __future__ import annotations

import argparse
import re
import sys
import urllib.request
from pathlib import Path

DATA_DIR = Path(__file__).resolve().parent.parent / "data"

# (remote path, local name, keep predicate)
#
# EastAsianWidth and GraphemeBreakProperty are kept whole: every line is a
# property assignment the generator needs. DerivedGeneralCategory and
# DerivedCoreProperties are large and mostly irrelevant, so they are filtered
# down to the categories that affect cell width and UAX #29 GB9c.
FILES = [
    ("EastAsianWidth.txt", "EastAsianWidth.txt", None),
    ("auxiliary/GraphemeBreakProperty.txt", "GraphemeBreakProperty.txt", None),
    ("auxiliary/GraphemeBreakTest.txt", "GraphemeBreakTest.txt", None),
    ("emoji/emoji-data.txt", "emoji-data.txt", None),
    ("emoji/emoji-variation-sequences.txt", "emoji-variation-sequences.txt", None),
    # Zero-width marks (Mn, Me) and format characters (Cf) plus the control
    # categories (Cc, Cs, Co, Cn) that get a non-printable width.
    (
        "extracted/DerivedGeneralCategory.txt",
        "DerivedGeneralCategory.txt",
        re.compile(r";\s*(Mn|Me|Cf|Cc|Cs|Co|Cn)\s*(#|$)"),
    ),
    # Indic conjunct break, for GB9c.
    ("DerivedCoreProperties.txt", "DerivedCoreProperties.txt", re.compile(r";\s*InCB;")),
]


def fetch(version: str, remote: str) -> str:
    url = f"https://www.unicode.org/Public/{version}/ucd/{remote}"
    with urllib.request.urlopen(url, timeout=60) as response:  # noqa: S310
        if response.status != 200:
            raise RuntimeError(f"{url}: HTTP {response.status}")
        return response.read().decode("utf-8")


def trim(text: str, keep: re.Pattern[str] | None) -> str:
    lines = text.splitlines()

    # Preserve the leading comment block: it carries the version and date that
    # make a regenerated table auditable.
    header: list[str] = []
    for line in lines:
        if not line.startswith("#"):
            break
        header.append(line)

    if keep is None:
        body = [ln for ln in lines[len(header) :] if ln.strip() and not ln.startswith("#")]
    else:
        body = [
            ln
            for ln in lines[len(header) :]
            if ln.strip() and not ln.startswith("#") and keep.search(ln)
        ]

    return "\n".join([*header, "", *body]) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", default="17.0.0", help="Unicode version (default: 17.0.0)")
    args = parser.parse_args()

    DATA_DIR.mkdir(parents=True, exist_ok=True)

    for remote, local, keep in FILES:
        print(f"fetching {remote}", file=sys.stderr)
        text = fetch(args.version, remote)
        trimmed = trim(text, keep)
        path = DATA_DIR / local
        path.write_text(trimmed, encoding="utf-8")
        line_count = trimmed.count("\n")
        print(f"  -> {path.relative_to(DATA_DIR.parent)} ({line_count} lines)", file=sys.stderr)

    (DATA_DIR / "VERSION").write_text(f"{args.version}\n", encoding="utf-8")
    print(f"\nUnicode {args.version} vendored. Next: python3 tools/gen_width_tables.py", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
