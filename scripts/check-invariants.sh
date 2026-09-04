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

# ---------------------------------------------------------------------------
# T7 - configuration
# ---------------------------------------------------------------------------
config=crates/supra_config/src

if [ -d "$config" ]; then
    # The thinking budget is frozen per session (section 6), which is enforced by
    # `Config` having no way to be mutated. A setter or a `&mut` accessor would turn
    # the freeze back into a convention.
    hits=$(scan "$config/resolve.rs" \
        'fn[[:space:]]+set_[a-z_]+|&mut[[:space:]]+self|impl[^;{]*DerefMut')
    if [ -n "$hits" ]; then
        fail "the resolved Config gained a mutation path" "$hits" \
            "the thinking budget is frozen per session; see ARCHITECTURE.md section 6"
    fi

    # A configuration file must not be able to hold a credential. Only the *names* of
    # an environment variable or a keyring entry are accepted, so a declared field
    # that takes a key would reopen the leak the schema closes.
    hits=$(scan "$config/layer.rs" \
        'pub[[:space:]]+api_key:[[:space:]]*Option<String>|pub[[:space:]]+auth_token:[[:space:]]*Option<String>|pub[[:space:]]+secret')
    if [ -n "$hits" ]; then
        fail "a config field can hold a literal credential" "$hits" \
            "only api_key_env and api_key_keyring are accepted; see the crate docs"
    fi

    # The `toml` crate's Display prints the offending source line, which put a pasted
    # credential straight into the message refusing it. Errors report the location and
    # the cause, never the content.
    #
    # Checked structurally rather than by grepping for `error.to_string()`: that
    # pattern depends on a variable name, and a probe showed it misses
    # `e.to_string()`. Instead, `describe_toml_error` must be the single place that
    # sees a `toml::de::Error` at all, and `parse` must route through it.
    if ! scan "$config/layer.rs" 'describe_toml_error' | grep -q 'toml::from_str' &&
        ! grep -A6 'pub fn parse(' "$config/layer.rs" | grep -q 'describe_toml_error'; then
        fail "ConfigLayer::parse no longer routes through describe_toml_error" \
            "a raw parser message echoes the file line, and may echo a pasted credential"
    fi

    mentions=$(scan "$config/layer.rs" 'toml::de::Error' | wc -l | tr -d ' ')
    if [ "$mentions" != "1" ]; then
        fail "toml::de::Error is handled in $mentions places, expected 1" \
            "$(scan "$config/layer.rs" 'toml::de::Error')" \
            "only describe_toml_error may see a parser error, so no other path can \
render one verbatim"
    fi

    # Unknown keys must be refused, or a typo is silently ignored.
    for shape in ConfigLayer ThinkingLayer CohortLayer PermissionLayer PromptLayer ProviderLayer; do
        if ! grep -B4 "pub struct $shape" "$config/layer.rs" | grep -q 'deny_unknown_fields'; then
            fail "$shape does not deny unknown fields" \
                "a misspelled key would be silently ignored"
        fi
    done
fi

# ---------------------------------------------------------------------------
# Internal dependency versions track the workspace version
#
# A path dependency needs an explicit `version` too, or Cargo records `*` - which
# cargo-deny's wildcard ban rejects and which cannot be published. Cargo has no
# `version.workspace = true` inside a dependency spec, so the literal is unavoidable;
# what is avoidable is it drifting silently when the workspace version is bumped.
# ---------------------------------------------------------------------------
workspace_version=$(sed -n '/^\[workspace\.package\]/,/^\[/p' Cargo.toml |
    sed -n 's/^version = "\(.*\)"/\1/p' | head -1)

if [ -z "$workspace_version" ]; then
    fail "could not read workspace.package.version from Cargo.toml" \
        "the internal dependency version check cannot run"
else
    internal=$(grep -E '^[a-z_]+ = \{ path = "crates/' Cargo.toml || true)
    if [ -n "$internal" ]; then
        while IFS= read -r line; do
            crate=${line%% =*}
            declared=$(printf '%s' "$line" | sed -n 's/.*version = "\([^"]*\)".*/\1/p')
            if [ -z "$declared" ]; then
                fail "internal dependency $crate has a path but no version" "$line" \
                    "Cargo records that as \`*\`, which cannot be published"
            elif [ "$declared" != "$workspace_version" ]; then
                fail "internal dependency $crate is pinned to $declared, workspace is $workspace_version" \
                    "$line" \
                    "bump the [workspace.dependencies] entry alongside workspace.package.version"
            fi
        done <<EOF
$internal
EOF
    fi
fi

if [ "$status" -eq 0 ]; then
    echo "invariants: prompt ledger, ephemeral state, authority, quorum, unsafe confinement,"
    echo "            the configuration schema, and internal versions all hold"
fi
exit "$status"
