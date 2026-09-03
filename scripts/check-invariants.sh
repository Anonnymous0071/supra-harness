#!/usr/bin/env bash
# Structural invariant checks that the compiler cannot express.
#
# docs/ARCHITECTURE.md section 2: invariants are "enforced by types and by CI, not
# by convention". Most of that enforcement is in the type system - `Sealed` has no
# mutable accessor, its bound on `T` makes `Sealed<EphemeralBlock>` unnameable, and
# deserialisation revalidates what was stored. But an *absence* cannot be asserted
# at runtime, and a derived impl or a new method can reintroduce one silently.
#
# Accuracy matters more than reach: a guard that cries wolf gets commented out, and
# a guard with a blind spot is worse than none because it certifies. Every check
# therefore runs against *shipped code only* - the whole file minus the bodies of
# `#[cfg(test)]` items, minus comments, minus string literals.
#
# Excluding test bodies is necessary rather than convenient. cohort.rs contains a
# test that deliberately reproduces the float quorum trap, and sealed.rs's tests
# construct the tampered values they reject; scanning them would make this script
# fail on its own subject matter.
#
# Excluding them by *tracking brace depth* rather than by truncating at the first
# `#[cfg(test)]` is the correctness-critical part. An earlier version stopped
# reading at that marker, which left every line below the test module unscanned -
# and a mutation probe showed it missed seven violations out of eight.

set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root"

status=0

fail() {
    printf 'INVARIANT %s\n' "$1" >&2
    shift
    while [ $# -gt 0 ]; do printf '  %s\n' "$1" >&2; shift; done
    status=1
}

# Emit a file's shipped code as `path:line:text`, skipping cfg(test) bodies,
# comments, and string literal contents.
production_lines() {
    python3 - "$1" <<'PY'
import re
import sys

path = sys.argv[1]

# Order matters: strings before line comments, so a "//" inside a literal is not
# mistaken for a comment, and block comments before both.
STRING = re.compile(r'"(?:[^"\\]|\\.)*"')
CHAR = re.compile(r"'(?:[^'\\]|\\.)'")
LINE_COMMENT = re.compile(r"//.*$")


def strip(line: str) -> str:
    """Remove literals and comments so braces and identifiers are code only."""
    line = STRING.sub('""', line)
    line = CHAR.sub("''", line)
    return LINE_COMMENT.sub("", line)


CFG_TEST = re.compile(r"#\[cfg(_attr)?\([^)]*\btest\b")

depth = 0
skip_until_depth = None  # set while inside a cfg(test) item
pending_skip = False
in_block_comment = False

with open(path, encoding="utf-8") as handle:
    for number, raw in enumerate(handle, start=1):
        line = raw.rstrip("\n")

        if in_block_comment:
            if "*/" in line:
                line = line.split("*/", 1)[1]
                in_block_comment = False
            else:
                continue
        if "/*" in strip(line):
            head, _, tail = line.partition("/*")
            if "*/" in tail:
                line = head + tail.split("*/", 1)[1]
            else:
                line = head
                in_block_comment = True

        code = strip(line)
        opens = code.count("{")
        closes = code.count("}")

        emit = skip_until_depth is None

        if pending_skip and code.strip():
            # The attribute's item starts here; skip until depth returns.
            skip_until_depth = depth
            pending_skip = False
            emit = False
        elif CFG_TEST.search(line):
            pending_skip = True
            emit = False

        depth += opens - closes

        if skip_until_depth is not None:
            if depth <= skip_until_depth:
                # A `#[cfg(test)] use ...;` line never opens a brace, so the item
                # ends on the same line it began.
                skip_until_depth = None
            continue

        if emit and code.strip():
            print(f"{path}:{number}:{code.rstrip()}")
PY
}

# Report matches of an extended regex against a file's shipped code.
scan() {
    local file=$1 pattern=$2
    production_lines "$file" | grep -E "$pattern" || true
}

crate=crates/supra_types/src

# ---------------------------------------------------------------------------
# I1 - the prompt is never rewritten
#
# `Sealed` must expose no mutation path: no `DerefMut`, no `get_mut`/`as_mut`, no
# `into_inner`. The last one matters most - moving the value out allows
# edit-and-reseal at the same sequence number, which is a rewrite wearing an
# append's clothes.
# ---------------------------------------------------------------------------
hits=$(scan "$crate/sealed.rs" \
    'DerefMut|BorrowMut|fn[[:space:]]+(get_mut|as_mut|into_inner|deref_mut|value_mut|inner_mut)[[:space:]]*\(')
if [ -n "$hits" ]; then
    fail "I1: Sealed gained a mutation path" "$hits" \
        "the prompt ledger must be append-only; see ARCHITECTURE.md invariant I1"
fi

# ---------------------------------------------------------------------------
# I2 - volatile data never enters the prefix
#
# `EphemeralBlock` must not implement `Sealable`, or `Sealed<EphemeralBlock>`
# becomes constructible and a state block becomes perpetual (the 202k-token
# arithmetic in ephemeral.rs). Scanned across the crate so the impl cannot hide in
# a sibling module.
# ---------------------------------------------------------------------------
for file in "$crate"/*.rs; do
    hits=$(scan "$file" 'impl.*Sealable.*for.*Ephemeral|Sealed<[[:space:]]*Ephemeral')
    if [ -n "$hits" ]; then
        fail "I2: EphemeralBlock became sealable" "$hits" \
            "volatile state would become perpetual; see ARCHITECTURE.md invariant I2"
    fi
done

# A prompt segment must not reference ephemeral state either - that is the same
# invariant reached through the back door.
hits=$(scan "$crate/segment.rs" 'Ephemeral')
if [ -n "$hits" ]; then
    fail "I2: a prompt segment references ephemeral state" "$hits" \
        "see ARCHITECTURE.md invariant I2"
fi

# ---------------------------------------------------------------------------
# Authority is never relaxable (section 6)
#
# A function on the authority axis that accepts a `Mode` would let a mode widen
# authority, which is the conflation the two-axis model exists to prevent. The free
# `decide` function takes both, and consults the class first.
# ---------------------------------------------------------------------------
hits=$(scan "$crate/permission.rs" 'fn[[:space:]]+[a-z_]+[^)]*\bMode\b' |
    grep -vE 'fn[[:space:]]+decide' || true)
if [ -n "$hits" ]; then
    fail "authority axis: a function other than decide() takes a Mode" "$hits" \
        "modes relax consent, never authority; see ARCHITECTURE.md section 6"
fi

# ---------------------------------------------------------------------------
# Quorum is rational, not floating point (section 4)
#
# `(0.67 * k).ceil()` is 3 at k=3 and would demand unanimity from a three-peer
# cohort. Shipped code in this module must contain no float at all.
# ---------------------------------------------------------------------------
hits=$(scan "$crate/cohort.rs" '\bf32\b|\bf64\b|[0-9]\.[0-9]')
if [ -n "$hits" ]; then
    fail "quorum arithmetic: a float reached shipped code" "$hits" \
        "the float spelling is wrong at k=3; see ARCHITECTURE.md section 4"
fi

# ---------------------------------------------------------------------------
# unsafe stays in supra_ffi (section 3)
#
# `#![forbid(unsafe_code)]` already enforces this at compile time; the grep catches
# a violation landing in the same change that removes the attribute.
# ---------------------------------------------------------------------------
for file in "$crate"/*.rs; do
    hits=$(scan "$file" '\bunsafe\b')
    if [ -n "$hits" ]; then
        fail "unsafe appeared outside supra_ffi" "$hits" "see ARCHITECTURE.md section 3"
    fi
done

if ! grep -q 'forbid(unsafe_code)' "$crate/lib.rs"; then
    fail "supra_types dropped #![forbid(unsafe_code)]" \
        "the compile-time half of the unsafe confinement is gone"
fi

if [ "$status" -eq 0 ]; then
    echo "invariants: I1, I2, authority, quorum, and unsafe confinement all hold"
fi
exit "$status"
