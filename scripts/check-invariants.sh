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
    python3 - "$1" "${2:-blank-strings}" <<'PY'
import re
import sys

path = sys.argv[1]
# "blank-strings" (the default) erases literal contents, which is what an identifier check
# wants. "keep-strings" leaves them, which is what a check on embedded SQL needs: there the
# literal is the subject, and blanking it made the float check blind.
keep_strings = len(sys.argv) > 2 and sys.argv[2] == "keep-strings"

# Order matters: strings before line comments, so a "//" inside a literal is not
# mistaken for a comment, and block comments before both.
STRING = re.compile(r'"(?:[^"\\]|\\.)*"')
CHAR = re.compile(r"'(?:[^'\\]|\\.)'")
LINE_COMMENT = re.compile(r"//.*$")
# An SQL comment inside a kept literal. Only stripped in keep-strings mode, which exists for
# checks whose subject is embedded SQL - and there a `--` comment is prose, not enforcement.
#
# A probe found this: two constraint checks passed after the constraint had been replaced,
# because the migration's own `-- Exactly one row, pinned by CHECK (id = 1)` comment satisfied
# the search. The guard was reading the schema's commentary as if it were the schema.
SQL_COMMENT = re.compile(r"--.*$")


def strip(line: str) -> str:
    """Remove comments, and literals unless the caller asked to keep them."""
    if keep_strings:
        # SQL comments apply only outside string literals: a `--` inside a
        # kept Rust literal (a clippy flag like `--quiet`, a `-->` the
        # cargo parser matches) is content, not commentary. Strip the line
        # segment by segment, between the strings this mode keeps.
        out = []
        rest = line
        while True:
            match = STRING.search(rest)
            if match is None:
                out.append(SQL_COMMENT.sub("", rest))
                break
            out.append(SQL_COMMENT.sub("", rest[:match.start()]))
            out.append(match.group(0))
            rest = rest[match.end():]
        line = "".join(out)
    else:
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

# Report matches of an extended regex against a file's shipped code, with string literals
# blanked. Right for identifiers, wrong for embedded SQL.
scan() {
    local file=$1 pattern=$2
    production_lines "$file" | grep -E "$pattern" || true
}

# As `scan`, but keeping string contents. For checks on embedded SQL, where the literal is
# the subject rather than a place a banned word might innocently appear.
scan_sql() {
    local file=$1 pattern=$2
    production_lines "$file" keep-strings | grep -E "$pattern" || true
}

# Whether a fixed string appears in a file's shipped code, with literals kept and comments
# stripped. For a check whose subject is a literal containing regex metacharacters - an SQL
# `CHECK (...)` clause, say.
#
# The comment stripping is the point. A probe caught a constraint check passing after the
# constraint had been deleted, because the module documentation quoted it: the guard was
# testing that the codebase still contained the words, not that the schema still enforced them.
#
# The output is collected before it is searched rather than piped into `grep -q`. Under
# `pipefail`, `grep -q` exits on its first match and closes the pipe, the Python producer dies
# of `BrokenPipeError`, and the pipeline reports that failure - so every check would have failed
# on a file that satisfied it. This is why `scan` and `scan_sql` end in `|| true`.
has_sql() {
    local file=$1 needle=$2 body
    body=$(production_lines "$file" keep-strings || true)
    printf '%s\n' "$body" | grep -qF "$needle"
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

    # A loopback exemption must parse an address, never match a prefix. `127.evil.example`
    # begins with the right digits and is an attacker-controlled hostname, and a bracketed
    # IPv6 literal must not be unwrapped without checking what follows the `]`.
    if ! grep -q 'is_loopback()' "$config/layer.rs"; then
        fail "the loopback exemption no longer parses an address" \
            "a prefix match would accept 127.evil.example and serve it plaintext"
    fi
    if ! grep -q 'if !port_is_well_formed' "$config/layer.rs"; then
        fail "the bracketed-IPv6 authority is no longer refused when malformed" \
            "unwrapping [::1] and ignoring the remainder accepted [::1].evil.example"
    fi

    # Unknown keys must be refused, or a typo is silently ignored.
    for shape in ConfigLayer ThinkingLayer CohortLayer PermissionLayer PromptLayer ProviderLayer; do
        if ! grep -B4 "pub struct $shape" "$config/layer.rs" | grep -q 'deny_unknown_fields'; then
            fail "$shape does not deny unknown fields" \
                "a misspelled key would be silently ignored"
        fi
    done

    # Credential resolution returns `SecretString`, never a bare `String`. A bare string out
    # of `provider_secret` would put the credential back into exactly the position the wrapper
    # exists to prevent: printable, loggable, and unwiped.
    body=$(sed -n '/pub fn provider_secret/,/^    }/p' "$config/resolve.rs")
    if ! printf '%s' "$body" | grep -q 'Result<supra_secrets::SecretString, ConfigError>'; then
        fail "provider_secret no longer returns a Secret" "$body" \
            "the credential must stay wrapped from resolution to the HTTP layer"
    fi
    if printf '%s' "$body" | grep -q -e '-> Result<String, ConfigError>'; then
        fail "provider_secret returns a bare String" "$body" \
            "a bare credential is printable, loggable, and unwiped"
    fi
fi

# ---------------------------------------------------------------------------
# T13 - providers
#
# Three properties the compiler cannot see: the canonicaliser actually sorts (its
# output must not depend on a transitive serde_json feature), the thinking floor is
# checked at construction (not at the first request), and the credential never sits
# in a field (it arrives per request, used once).
# ---------------------------------------------------------------------------
llm=crates/supra_llm/src

if [ -d "$llm" ]; then
    # The sort must be an explicit call in `emit`, not an inherited property of the
    # container. `serde_json::Map` without `preserve_order` is a BTreeMap (sorted),
    # so deleting `keys.sort()` changes nothing today - and the day a transitive
    # feature enables `preserve_order`, the canonicaliser silently emits insertion
    # order. A probe deleting the sort must fail.
    if ! scan "$llm/canonical.rs" 'keys\.sort\(\)' | grep -q 'keys.sort()'; then
        fail "the canonicaliser no longer sorts keys explicitly" \
            "ordering would depend on serde_json's preserve_order feature flag"
    fi

    # `CanonicalJson::from_canonical` must be called from exactly one place: the
    # canonicaliser. A second producer is a second implementation of canonicity, and
    # two implementations can disagree - which is an invisible cache break.
    producers=$(scan "$llm/canonical.rs" 'from_canonical' | wc -l | tr -d ' ')
    if [ "$producers" != "2" ]; then
        fail "expected from_canonical at 2 sites in canonical.rs, found $producers" \
            "$(scan "$llm/canonical.rs" 'from_canonical')" \
            "T13's serialiser is the only sanctioned producer of CanonicalJson"
    fi
    # And no producer outside the canonicaliser. `scan` takes a file, so walk the
    # crate; the count above covers canonical.rs, this covers everywhere else.
    hits=$(for file in "$llm"/*.rs; do
        [ "$(basename "$file")" = "canonical.rs" ] && continue
        scan "$file" 'from_canonical'
    done)
    if [ -n "$hits" ]; then
        fail "a second CanonicalJson producer exists outside the canonicaliser" "$hits" \
            "two implementations of canonicity can disagree; that is a cache break"
    fi

    # The thinking floor is checked in `Client::new`, not at the first request.
    # `budget_tokens` is rendered into the prompt, so a below-floor budget invalidates
    # every cache breakpoint from the first turn - surfacing it late means the first
    # turn paid full price before anyone learned the configuration was wrong.
    body=$(sed -n '/pub fn new(/,/^    }/p' "$llm/client.rs")
    if ! printf '%s' "$body" | grep -q 'ThinkingBudget'; then
        fail "Client::new no longer checks the thinking floor" "$body" \
            "T7 requires the check at startup, not at the first request"
    fi

    # The credential arrives per request, never as a field. A `Client` holding a
    # `SecretString` keeps the credential alive for the session's lifetime and widens
    # every clone, log, and unwind path into a leak vector.
    #
    # A field declaration is `name: Type,` at struct depth; the sanctioned parameter
    # `credential: &supra_secrets::SecretString,` carries a `&` the field never would.
    # Match the field spelling (no `&`) anchored to end-of-line, so the parameter
    # cannot satisfy it.
    hits=$(scan "$llm/client.rs" 'credential:[[:space:]]*Option<|credential:[[:space:]]*supra_secrets|secret:[[:space:]]*$')
    if [ -n "$hits" ]; then
        fail "the client holds a credential" "$hits" \
            "it arrives per send(), used once, never stored"
    fi
    # The positive form: `send` must take the credential as a parameter.
    if ! grep -q 'credential: &supra_secrets::SecretString' "$llm/client.rs"; then
        fail "Client::send no longer takes the credential per request" \
            "the credential must arrive per call, used once, never stored"
    fi

    # Retries are the caller's, not the client's. Only RateLimited carries a delay,
    # and Unauthorized must never be retried (a bad credential retried is how a typo
    # becomes a lockout). `retry_after_ms` (the delay *value*) and `RateLimited` (the
    # variant that carries it) are the sanctioned vocabulary; a loop, a sleep, or a
    # backoff around `send` is the violation.
    hits=$(scan "$llm/client.rs" 'tokio::time::sleep|for .* in .*retry|while .*retry|backoff')
    if [ -n "$hits" ]; then
        fail "the client retries" "$hits" \
            "backoff belongs to the turn loop, which knows how many peers wait"
    fi
fi

# ---------------------------------------------------------------------------
# T8 - diagnostics
# ---------------------------------------------------------------------------
log=crates/supra_log/src

if [ -d "$log" ]; then
    # supra_log is the only crate that may write to stderr, and only through the sink.
    # A `print!`/`eprint!` anywhere else in the crate bypasses the TUI suppression that
    # keeps a stray line from tearing the frame.
    hits=$(for file in "$log"/*.rs; do
        [ "$(basename "$file")" = "sink.rs" ] && continue
        scan "$file" '\bprintln!|\beprintln!|\bprint!|\beprint!|io::stderr|io::stdout'
    done)
    if [ -n "$hits" ]; then
        fail "a diagnostic bypasses the sink" "$hits" \
            "only sink.rs may touch stderr, so TUI suppression cannot be sidestepped"
    fi

    # The redactor's fast path must be derived from its tables, never restate them. The
    # first version listed its own literals and got one wrong - `github_pat_` does not
    # contain `gh` - so every credential with that prefix passed through silently.
    if ! sed -n '/fn might_contain_secret/,/^}/p' "$log/redact.rs" | grep -q 'SHAPES.iter()'; then
        fail "the redaction fast path no longer reads SHAPES" \
            "a second copy of the matcher's literals will drift, and it drifts silently"
    fi
    if ! sed -n '/fn might_contain_secret/,/^}/p' "$log/redact.rs" | grep -q 'SECRET_FIELDS.iter()'; then
        fail "the redaction fast path no longer reads SECRET_FIELDS" \
            "a second copy of the matcher's field names will drift"
    fi

    # The field-name rule must match exactly. A prefix match would eat `api_key_env`,
    # whose value T7 defines as the NAME of an environment variable and which a reader
    # chasing a credential problem needs to see.
    if grep -qE 'SECRET_FIELDS[^;]*starts_with\(field\)|field[^;]*is_prefix' "$log/redact.rs"; then
        fail "the secret-field rule became a prefix match" \
            "api_key_env is a signpost, not a secret; the difference is four characters"
    fi

    # An open that can block for ever must be pre-flighted. T7 guards its read path and
    # T8 initially did not, so pointing the log at a FIFO hung startup before any UI
    # existed to explain it. The lesson generalises, so both are checked here.
    if ! sed -n '/^fn open_append/,/^}/p' "$log/sink.rs" | grep -q 'refuse_blocking_target'; then
        fail "open_append no longer calls the blocking-target pre-flight" \
            "opening a FIFO for writing blocks until a reader appears, which hangs startup"
    fi
    if ! grep -q 'is_fifo()' "$log/sink.rs"; then
        fail "the log sink's pre-flight no longer tests for a FIFO" \
            "that is the one file type whose open blocks; /dev/null must stay usable"
    fi

    # The log file is owner-only, for the same reason T7 holds config to 0600.
    if ! grep -q 'mode(0o600)' "$log/sink.rs"; then
        fail "the log file is no longer created owner-only" \
            "a log beside a 0600 config at 0644 makes the config mode pointless"
    fi
fi

# ---------------------------------------------------------------------------
# T9 - the event bus
# ---------------------------------------------------------------------------
eventbus=crates/supra_eventbus/src

if [ -d "$eventbus" ]; then
    # Publishing must not be able to wait on a consumer. A `Result`, an `async fn`, or a
    # blocking send in `publish` would let the slowest subscriber set turn latency.
    publish=$(sed -n '/pub fn publish/,/^    }/p' "$eventbus/bus.rs")
    if printf '%s' "$publish" | grep -qE 'async|\.await|-> *Result|blocking'; then
        fail "publish can wait on a consumer" "$publish" \
            "the publisher is the turn loop; backpressure belongs in the subscriber's ring"
    fi
    if ! grep -qE 'pub fn publish\(&self, event: Event\) -> EventSeq' "$eventbus/bus.rs"; then
        fail "publish no longer has its infallible signature" \
            "it must be sync, take an owned Event, and return only the sequence number"
    fi

    # The bus must not depend on an async runtime; waiting is the subscriber's business.
    if grep -qE '^tokio|^futures' crates/supra_eventbus/Cargo.toml; then
        fail "the event bus gained an async runtime dependency" \
            "publishing cannot wait, so it needs none; an adapter belongs to its consumer"
    fi

    # A ring that drops an event must carry that event's own gap count forward, or a
    # consumer summing `missed_before` under-reports while `missed_total` stays right.
    if ! grep -q 'dropped.missed_before + 1' "$eventbus/bus.rs"; then
        fail "an evicted delivery's gap count is no longer carried forward" \
            "the two ways of asking about loss would disagree; see the invariant test"
    fi

    # The bitset width must be read from the topic list, not written as a literal.
    if ! grep -q 'pub const WIDTH: usize = Topic::ALL.len()' "$eventbus/topic.rs"; then
        fail "the topic bitset width is no longer derived from Topic::ALL" \
            "a topic that does not fit would be silently undeliverable"
    fi
fi

# ---------------------------------------------------------------------------
# T10 - durable storage
# ---------------------------------------------------------------------------
store=crates/supra_store/src

if [ -d "$store" ]; then
    # Invariant I4 promises a recalled turn comes back byte-identical. Recall must verify
    # the digest and must NOT return the bytes when it fails: a caller cannot tell altered
    # content from the real thing once it has it.
    recall=$(sed -n '/pub fn recall_turn/,/^    }/p' "$store/turns.rs")
    if ! printf '%s' "$recall" | grep -q 'StoreError::Corrupt'; then
        fail "recall_turn no longer verifies the stored digest" \
            "invariant I4 promises byte-identical recall; unverified bytes are a silent lie"
    fi
    if ! printf '%s' "$recall" | grep -q 'if computed != stored'; then
        fail "recall_turn no longer compares the digests" \
            "see ARCHITECTURE.md invariant I4"
    fi

    # Turn bodies are stored as bytes. A TEXT column would invite an encoding assumption
    # between write and read, which is exactly what byte-identical rules out.
    if ! has_sql "$store/schema.rs" 'body       BLOB    NOT NULL'; then
        fail "the turn body column is no longer a BLOB" \
            "TEXT invites an encoding assumption; I4 promises these bytes back unchanged"
    fi

    # STRICT alone does not protect a TEXT column - it accepts an integer and converts it.
    # The invariants the code depends on are CHECK constraints for that reason.
    #
    # Through `has_sql` rather than a plain grep. The same check written against the whole file
    # was satisfied by this module's own documentation, which quotes `byte_len = length(body)`
    # while explaining why the constraint exists - so it would have passed with the constraint
    # deleted. Found by probing the equivalent check in T11.
    for constraint in 'CHECK (length(turn_id) = 26)' 'CHECK (length(body_hash) = 32)' \
        'CHECK (byte_len = length(body))' 'CHECK (stored_at >= 0)'; do
        if ! has_sql "$store/schema.rs" "$constraint"; then
            fail "the schema lost its CHECK on: $constraint" \
                "STRICT does not cover this; a bad row would surface at recall instead"
        fi
    done

    # A store written by a newer supra must be refused, never read with older code. Scoped
    # to `migrate` itself: a probe showed a file-wide grep satisfied by the mention in the
    # test module after the real check had been removed.
    if ! sed -n '/pub fn migrate/,/^}/p' "$store/schema.rs" | grep -q 'SchemaTooNew'; then
        fail "migrate no longer refuses a newer schema" \
            "a later schema may reuse a name with different meaning; reading it misinterprets"
    fi

    # Durability is not a preference here: this store holds turns the prefix has dropped.
    if ! grep -q 'Self::Full => "FULL"' "$store/lib.rs"; then
        fail "the durable synchronous setting is gone" \
            "in WAL, NORMAL can lose the last commits, and a lost commit is a lost turn"
    fi
    if ! grep -qE 'pub enum Synchronous \{' "$store/lib.rs" ||
        ! sed -n '/pub enum Synchronous/,/^}/p' "$store/lib.rs" | grep -q '#\[default\]'; then
        fail "Synchronous no longer has a default" \
            "FULL must be what a caller gets without asking"
    fi

    # No floats. `total()` returns a REAL, which is how this was found. Uses `scan_sql`
    # because the query lives in a string literal, and `scan` blanks those - a probe caught
    # this check passing on code that had `total(byte_len)` back.
    hits=$(scan_sql "$store/turns.rs" 'total\(byte_len\)|\bf32\b|\bf64\b')
    if [ -n "$hits" ]; then
        fail "a float reached the store's size accounting" "$hits" \
            "total() returns REAL and loses whole bytes past 2^53; use coalesce(sum(...), 0)"
    fi
fi

# ---------------------------------------------------------------------------
# T11 - hybrid retrieval
# ---------------------------------------------------------------------------
vector=crates/supra_vector/src

if [ -d "$vector" ]; then
    # `bm25()` is negative and a better match is MORE negative. Ordering descending, or
    # taking an absolute value, inverts the lexical lane - and inverts it silently, because
    # every query still returns the number of results it was asked for.
    lexical=$(sed -n '/pub fn search_lexical/,/^    }/p' "$vector/index.rs")
    if ! printf '%s' "$lexical" | grep -q 'ORDER BY bm25(vector_text)'; then
        fail "the lexical lane no longer ranks by bm25 ascending" \
            "bm25() is negative; without this the lane returns its worst matches first"
    fi
    if printf '%s' "$lexical" | grep -qiE 'bm25\(vector_text\) DESC|abs\(bm25'; then
        fail "the lexical lane inverted its bm25 ordering" \
            "a better match is more negative; DESC and abs() both reverse the ranking"
    fi

    # The candidate stage cannot separate near-duplicates: one bit per dimension records a
    # direction, not a magnitude. Measured, ranking by Hamming distance alone recovers 0.54
    # of the exhaustive top-10 where the exact rerank recovers all of it - and nothing fails.
    semantic=$(sed -n '/pub fn search_semantic/,/^    }/p' "$vector/index.rs")
    if ! printf '%s' "$semantic" | grep -q 'codes::similarity'; then
        fail "the semantic lane no longer reranks with exact vectors" \
            "codes only choose candidates; without the rerank recall@10 falls to about 0.54"
    fi

    # A stale exact vector under a live slot reranks the new code against the old vector, and
    # the answer looks ordinary. Both write paths must drop it.
    for path in upsert remove; do
        body=$(sed -n "/pub fn $path(/,/^    }/p" "$vector/index.rs")
        if ! printf '%s' "$body" | grep -q 'cache.invalidate'; then
            fail "$path no longer invalidates the exact-vector cache" \
                "a stale vector under a live slot ranks wrongly and reports nothing"
        fi
    done

    # An FTS5 row is keyed by rowid, so inserting over one without deleting first leaves two
    # postings lists for the same entry - and a renamed symbol matches its old name for ever.
    upsert=$(sed -n '/pub fn upsert(/,/^    }/p' "$vector/index.rs")
    delete_at=$(printf '%s\n' "$upsert" | grep -n 'DELETE FROM vector_text' | head -1 | cut -d: -f1)
    insert_at=$(printf '%s\n' "$upsert" | grep -n 'INSERT INTO vector_text' | head -1 | cut -d: -f1)
    if [ -z "$delete_at" ] || [ -z "$insert_at" ]; then
        fail "upsert no longer replaces the entry's indexed text" \
            "it must DELETE then INSERT the fts5 row, or the old text keeps matching"
    elif [ "$delete_at" -gt "$insert_at" ]; then
        fail "upsert inserts its fts5 row before deleting the old one" \
            "the delete must come first, or both postings lists survive"
    fi

    # The resident tier is a cache of what is durable. Updating it before the commit would
    # leave it describing a write that failed, and every later query would trust it.
    commit_at=$(printf '%s\n' "$upsert" | grep -n 'with_transaction' | head -1 | cut -d: -f1)
    resident_at=$(printf '%s\n' "$upsert" | grep -n 'codes.upsert' | head -1 | cut -d: -f1)
    if [ -z "$commit_at" ] || [ -z "$resident_at" ]; then
        fail "upsert no longer writes through a transaction into the resident tier" \
            "both must happen, in that order"
    elif [ "$resident_at" -lt "$commit_at" ]; then
        fail "upsert updates the resident tier before the transaction commits" \
            "a failed write would leave the tier describing a row that does not exist"
    fi

    # The binarisation threshold is frozen. If it moves, every code written before the move
    # answers a different question from every code written after, and the scan ranks them
    # against each other without failing.
    hits=$(scan_sql "$vector/index.rs" 'UPDATE vector_meta')
    if [ -n "$hits" ]; then
        fail "the frozen index configuration is being updated in place" "$hits" \
            "moving the threshold silently invalidates every code already written"
    fi
    if ! printf '%s' "$(sed -n '/pub fn open(/,/^    }/p' "$vector/index.rs")" |
        grep -q 'FrozenThresholdMismatch'; then
        fail "open no longer refuses a changed threshold" \
            "silently keeping the stored one leaves the caller wrong about its own encoding"
    fi

    # The two blobs in a row must describe the same width. A CHECK cannot reach vector_meta,
    # but dims*4 against dims/8 is a fixed ratio for every dims.
    #
    # Through `has_sql`, which strips comments: a probe caught the plain `grep -F` version
    # passing after the constraint had been deleted, satisfied by the module documentation that
    # quotes it. The guard was testing vocabulary rather than enforcement.
    for constraint in 'CHECK (length(embedding) = length(code) * 32)' 'CHECK (id = 1)' \
        'CHECK (length(threshold) = dims * 4)' 'contentless_delete=1'; do
        if ! has_sql "$vector/schema.rs" "$constraint"; then
            fail "the vector schema lost: $constraint" \
                "each of these is an invariant no code path can otherwise be stopped from breaking"
        fi
    done

    # MAX_DIMS is stated twice - as a constant and as a literal inside the migration, which
    # cannot reference it. A drift between them accepts a width the scan cannot meet its
    # budget at, or refuses one it can.
    declared=$(sed -n 's/^pub const MAX_DIMS: usize = \([0-9_]*\);/\1/p' "$vector/schema.rs" |
        tr -d '_')
    in_sql=$(grep -oE 'dims <= [0-9]+' "$vector/schema.rs" | grep -oE '[0-9]+' | head -1)
    if [ -z "$declared" ] || [ -z "$in_sql" ]; then
        fail "could not read MAX_DIMS from both the constant and the migration" \
            "the agreement between them is what this check is about"
    elif [ "$declared" != "$in_sql" ]; then
        fail "MAX_DIMS is $declared but the migration allows $in_sql" \
            "the CHECK cannot reference the constant, so the two must be edited together"
    fi

    # `user_version` is one slot and T10's core schema owns it. A second writer there would
    # make each stage believe the other had regressed.
    #
    # Scanned through `scan_sql`, which keeps string literals - a `PRAGMA user_version` lives in
    # one - but strips comments. A plain recursive grep fired on the module documentation that
    # explains why this crate uses the component ledger instead, which is the difference between
    # checking enforcement and checking vocabulary.
    for file in "$vector"/*.rs; do
        hits=$(scan_sql "$file" 'user_version')
        if [ -n "$hits" ]; then
            fail "supra_vector touches the file's user_version" "$hits" \
                "it owns a row in schema_component; user_version belongs to T10's core schema"
        fi
    done

    # The fused score is an integer. Normalising two lanes' scores onto a shared range makes
    # the fused ranking depend on corpus size, which is why fusion is over ranks.
    if ! grep -q 'pub score: u64' "$vector/search.rs"; then
        fail "the fused score is no longer an integer" \
            "rank fusion is integer arithmetic so the same inputs fuse identically everywhere"
    fi
fi

# ---------------------------------------------------------------------------
# The store's connection stays behind its mutex
#
# `with_connection` and `with_transaction` are the only doors for a crate that owns
# tables in T10's file. Handing out the connection itself would let a caller take a
# lock the `Store` is responsible for, or hold it across an await.
# ---------------------------------------------------------------------------
if [ -f "crates/supra_store/src/lib.rs" ]; then
    if ! grep -qE 'pub\(crate\) fn connection\(' crates/supra_store/src/lib.rs; then
        fail "Store::connection is no longer crate-private" \
            "a caller holding the connection can take a lock the Store is responsible for"
    fi
    hits=$(scan crates/supra_store/src/lib.rs 'pub fn connection\(')
    if [ -n "$hits" ]; then
        fail "Store::connection was made public" "$hits" \
            "use with_connection or with_transaction, which keep the mutex inside"
    fi
fi

# ---------------------------------------------------------------------------
# T12 - secrets
#
# The wrapper is the mechanism and the redactor is the net: a `Secret` that becomes
# printable, cloneable, or serialisable reopens every leak the wrapper closes, and no
# test would fail - the value would simply start appearing in logs.
# ---------------------------------------------------------------------------
secrets=crates/supra_secrets/src

if [ -d "$secrets" ]; then
    # Debug and Display must reveal nothing. A derived or echoed impl would dump the
    # credential into any diagnostic that formats the value.
    for trait in Debug Display; do
        body=$(sed -n "/impl<T: Zeroize> fmt::$trait for Secret<T>/,/^}/p" "$secrets/secret.rs")
        if [ -z "$body" ]; then
            fail "Secret lost its manual fmt::$trait impl" \
                "a derived $trait would print the credential into diagnostics"
        elif ! printf '%s' "$body" | grep -q '\[REDACTED\]'; then
            fail "Secret's fmt::$trait no longer redacts" "$body" \
                "the placeholder is what keeps a diagnostic from becoming a leak"
        fi
    done

    # Clone, Eq, and Serialize must stay absent. Each is a leak vector: a duplicate doubles
    # the places a secret can escape from, comparison holds two secrets together, and
    # serialisation is how secrets reach logs, files, and wires. The absence is structural -
    # a `#[derive(Clone)]` on a struct holding a Secret fails to compile - and the guard
    # keeps it from being added here directly.
    hits=$(scan "$secrets/secret.rs" 'impl.*Clone.*for.*Secret|impl.*PartialEq.*for.*Secret|impl.*Eq.*for.*Secret|impl.*Serialize.*for.*Secret')
    if [ -n "$hits" ]; then
        fail "Secret gained Clone, Eq, or Serialize" "$hits" \
            "each is a leak vector the wrapper exists to refuse"
    fi
    # A derive on the struct is the other spelling - and the dangerous one, because a
    # derived Debug dumps the credential while the manual impl below keeps compiling
    # untouched beside it. Grep the struct definition's own attribute block: the lines
    # directly above `pub struct Secret`, which is where a derive would sit.
    struct_block=$(grep -B6 '^pub struct Secret<T' "$secrets/secret.rs" | head -8)
    if printf '%s' "$struct_block" | grep -q '#\[derive'; then
        fail "Secret carries a derive macro" "$struct_block" \
            "a derived Debug would print the credential while the manual impl compiles beside it"
    fi

    # The KDF floor must agree with the production constant. A floor below the constant
    # certifies; a floor above it breaks the build. Both are caught by the same comparison
    # the code itself asserts at compile time, restated here so the script and the source
    # cannot drift apart unnoticed.
    declared=$(sed -n 's/^const PBKDF2_ITERATIONS: u32 = \([0-9_]*\);/\1/p' "$secrets/file_store.rs" |
        head -1 | tr -d '_')
    floor=$(sed -n 's/^const MIN_PRODUCTION_ROUNDS: u32 = \([0-9_]*\);/\1/p' "$secrets/file_store.rs" |
        head -1 | tr -d '_')
    if [ -z "$declared" ] || [ -z "$floor" ]; then
        fail "could not read PBKDF2_ITERATIONS or MIN_PRODUCTION_ROUNDS" \
            "the agreement between them is what this check is about"
    elif [ "$declared" != "$floor" ]; then
        fail "PBKDF2_ITERATIONS is $declared but the floor is $floor" \
            "a floor that disagrees with the constant it guards is worse than no floor"
    fi

    # The vault file must be 0600 before the first secret byte lands, not after. A chmod
    # after the write leaves a window where the file is world-readable; the temp file must
    # also carry the mode, because rename preserves it.
    #
    # Two `set_permissions` calls exist (temp file before the write, destination after the
    # rename), so the check extracts only the segment between `File::create` and the first
    # `write_all`: exactly the region where ordering matters. A probe that deleted the
    # first chmod while leaving the second must fail - the second covers the destination,
    # not the window.
    segment=$(sed -n '/fn save(/,/^    }/p' "$secrets/file_store.rs" |
        sed -n '/File::create/,/write_all/p')
    if ! printf '%s\n' "$segment" | grep -q 'set_permissions'; then
        fail "the vault file is not restricted before the first write" "$segment" \
            "chmod must precede write_all, or the secret has a world-readable window"
    fi
    if ! printf '%s\n' "$segment" | grep -q 'from_mode(0o600)'; then
        fail "the pre-write restriction is not 0600" "$segment" \
            "the mode is the guarantee, not just the presence of a chmod call"
    fi

    # The passphrase must never come from an interactive prompt inside the library. A
    # blocking read hangs every non-interactive caller - a test runner, a daemon, a tool
    # harness - on a question nobody asked. The CLI supplies the prompt through the
    # provider callback; the library only resolves memory, environment, provider.
    #
    # Two spellings are checked because the violation has two: naming a prompt mechanism
    # (`rpassword`, `/dev/tty`, ...) and *using* an input handle. A probe smuggling in
    # `std::io::stdin()` tripped only the second; a probe reading `/dev/tty` as a path
    # tripped only the first.
    #
    # `scan` blanks string literals, so `"/dev/tty"` is invisible to it - by design, since a
    # literal could otherwise be a doc example. The path spelling is therefore checked with
    # `scan_sql`, which keeps literals. Either is a library that blocks on a question
    # nobody asked.
    hits=$(scan "$secrets/file_store.rs" 'rpassword|prompt_password|read_passphrase|hidden_input|stdin\(\)|Stdio::stdin|io::stdin')
    hits="$hits$(scan_sql "$secrets/file_store.rs" '/dev/tty')"
    if [ -n "$hits" ]; then
        fail "the file store reads interactive input" "$hits" \
            "a library that blocks on input hangs every non-interactive caller behind it"
    fi

    # The production binary must never take the test-only KDF. `cfg(test)` is set by
    # the compiler for the crate under test and by nothing else; a feature flag would be one
    # `cargo add --features` away from any build that copies the line.
    #
    # Checked as a property, not a name: the production `PBKDF2_ITERATIONS` must be a
    # `#[cfg(not(test))]` item, and no second `PBKDF2_ITERATIONS` may exist under any other
    # gate. A probe widening the test gate to `cfg(any(test, feature = "test-kdf"))` - the
    # exact weakening this guards against - changes the gate text, which a name search for
    # `test-kdf` only catches after the feature is also referenced somewhere.
    prod_gate=$(grep -B1 '^const PBKDF2_ITERATIONS' "$secrets/file_store.rs" | head -2)
    if ! printf '%s' "$prod_gate" | grep -q '#\[cfg(not(test))\]'; then
        fail "the production KDF constant is not cfg(not(test))-gated" "$prod_gate" \
            "any wider gate lets a downstream build select the weak KDF"
    fi
    gates=$(grep -c '^const PBKDF2_ITERATIONS' "$secrets/file_store.rs")
    if [ "$gates" != "2" ]; then
        fail "expected exactly two PBKDF2_ITERATIONS definitions, found $gates" \
            "one production (cfg(not(test))), one test-only (cfg(test)); any other shape is unreviewed"
    fi
    if grep -A1 'cfg(any(test' "$secrets/file_store.rs" | grep -q 'PBKDF2_ITERATIONS'; then
        fail "the test KDF gate was widened beyond cfg(test)" \
            "a feature gate makes the weak KDF selectable by any downstream build"
    fi

    # `StoreTooNew` must not exist without a version field to compare. The encrypted file
    # format is four fields with no version slot, so a "too new" variant can never fire -
    # and dead error variants are misleading API: they promise a case the code cannot detect.
    hits=$(scan "$secrets/error.rs" 'StoreTooNew')
    if [ -n "$hits" ]; then
        fail "a StoreTooNew variant exists without a version to compare" "$hits" \
            "the vault format has no version field, so the variant could never fire"
    fi
fi

# ---------------------------------------------------------------------------
# T12.5 - anti-self-spawn guard
#
# Seven layers, no off switch. Each check below names the layer it protects; a probe
# per check deletes the enforcement while leaving the name, because a guard that
# tests vocabulary ("does L4 appear somewhere") certifies broken code.
# ---------------------------------------------------------------------------
guard=crates/supra_guard/src

if [ -d "$guard" ]; then
    # L1/L2 share the NoIdentity variant with a detail that names the missing half, and
    # `layer_of` maps the detail back to 1 or 2. A probe replacing the detail parse with
    # a constant 1 makes L2's refusal report as L1 - the operator investigates the wrong
    # half. The check asserts the mapping reads the detail, not the variant alone.
    body=$(sed -n '/pub fn layer_of/,/^}/p' "$guard/layers.rs")
    if ! printf '%s' "$body" | grep -q 'marker key'; then
        fail "layer_of no longer distinguishes L1 from L2" "$body" \
            "both share NoIdentity; the detail text is what maps back to 1 or 2"
    fi
    if printf '%s' "$body" | grep -q 'Refusal::NoIdentity[^a-z]*=>[^0-9]*1,'; then
        fail "layer_of maps every NoIdentity to L1" "$body" \
            "L2's refusal would report as L1 and misdirect the investigation"
    fi

    # Every layer must run even after one refuses. Short-circuiting on the first refusal
    # hides what the other layers saw: "refused by L3" says what happened, "refused by
    # L3, L4, and L5" says what was attempted. A probe inserting an early return after
    # the first push must fail.
    body=$(sed -n '/pub fn judge/,/^}/p' "$guard/layers.rs")
    if printf '%s' "$body" | grep -qE 'if !refusals.is_empty\(\)|if refusals.len\(\)|return Verdict \{ refusals \}|return Verdict\{'; then
        fail "judge short-circuits on the first refusal" "$body" \
            "every layer runs unconditionally; the verdict carries all refusals"
    fi
    # The positive form: all seven layer sections must be present in `judge`. A probe
    # deleting one layer's block while leaving its helper intact must fail.
    for marker in 'L1: identity' 'L2: marker key' 'L3: the command' 'L4: the command' 'L5: the marker' 'L6: the lineage' 'L7: no self-voting'; do
        if ! printf '%s' "$body" | grep -qF -- "$marker"; then
            fail "judge lost its $marker section" "$body" \
                "deleting a layer while leaving its helper compiles and certifies"
        fi
    done

    # L4 must compare (device, inode) through supra_ffi, not a path string. A path
    # comparison passes a symlink with an innocent name and fails a hardlink with a
    # guilty one - both wrong.
    #
    # Two halves, because the call site and the definition live in different crates:
    # `judge` must call `is_own_file` (the (device, inode) comparison in identity.rs,
    # which itself calls `supra_ffi::sandbox::file_identity`), and a probe replacing the
    # call with a path comparison must fail. Checking only that the *name* appears
    # somewhere is vocabulary; checking the call site is enforcement.
    hits=$(scan "$guard/layers.rs" 'is_own_file')
    if [ -z "$hits" ]; then
        fail "L4 no longer calls is_own_file at the judgement site" \
            "a path-string comparison passes a symlink with an innocent name"
    fi
    if ! scan "$guard/identity.rs" 'file_identity' | grep -q .; then
        fail "L4's identity comparison no longer resolves through supra_ffi" \
            "names lie, inodes do not"
    fi

    # L5 must verify through HMAC, constant-time. The check targets the call site in
    # `verify`, not the doc comment that names the primitive: a probe replacing the call
    # while leaving the comment must fail, and a probe *adding* a `==` fast path next to
    # the real check must fail too, because the fast path leaks timing.
    #
    # `scan` (not grep) so a doc example mentioning `verify_slice` does not satisfy it -
    # the call must be in shipped code. Same lesson as T11's constraint checks.
    if ! scan "$guard/marker.rs" 'verify_slice' | grep -q 'mac.verify_slice'; then
        fail "L5 no longer calls verify_slice at the verification site" \
            "hmac::Mac::verify_slice is the primitive that does this correctly"
    fi
    hits=$(scan "$guard/marker.rs" 'expected ==|== *expected|presented ==|== *presented|\.result\(\)')
    if [ -n "$hits" ]; then
        fail "L5 compares tags outside verify_slice" "$hits" \
            "a byte-wise early exit lets a local process time the match byte by byte"
    fi
    # The tag helper is called exactly once outside its definition: when issuing.
    # Verification recomputes nothing - it checks the presented tag against the key via
    # `verify_slice`. A second call site is a shadow verification path, and a shadow path
    # is where a `==` fast path hides.
    calls=$(scan "$guard/marker.rs" '(^|[^_:a-zA-Z])tag\(' | wc -l | tr -d ' ')
    if [ "$calls" != "2" ]; then
        fail "expected tag() at 2 sites (def, issue), found $calls" \
            "$(scan "$guard/marker.rs" '(^|[^_:a-zA-Z])tag\(')" \
            "a new call site is a shadow verification path until proven otherwise"
    fi

    # The marker key must be zeroized, not merely dropped. A plain `[u8; 32]` leaves both
    # key generations in freed heap; `Zeroizing` wipes on drop and on rotation. Checked at
    # the declaration site (`static KEY`), not by counting mentions: rotation helpers also
    # name the type, so a whole-file grep for `Zeroizing` passes with the static replaced.
    if ! scan "$guard/marker.rs" 'static KEY' | grep -q 'Zeroizing'; then
        fail "the marker key static is not zeroized" \
            "a plain array leaves key generations in freed heap"
    fi

    # Entropy failure must be a returned Refusal, never a panic. `expect` on a getrandom
    # or HMAC constructor turns a kernel state into an abort, and takes the degraded-run
    # decision away from startup. T12's file store maps the same failure to Crypto; here
    # it is NoEntropy, because the remedy differs.
    hits=$(scan "$guard/marker.rs" 'expect\(')
    if [ -n "$hits" ]; then
        fail "marker keying can panic" "$hits" \
            "map entropy and constructor failures to Refusal::NoEntropy instead"
    fi

    # No off switch: no flag, mode, or configuration may skip a layer. `yolo` relaxes
    # consent (T16.7); it does not touch authority. A probe adding a `skip_l2: bool` or a
    # `Mode` parameter to `judge`/`SpawnRequest` must fail.
    hits=$(scan "$guard/layers.rs" 'skip_|bypass|disable|Mode|yolo')
    if [ -n "$hits" ]; then
        fail "the guard grew an off switch" "$hits" \
            "layers L1-L7 have no off switch; yolo relaxes consent, not authority"
    fi
    hits=$(scan "$guard/lib.rs" 'skip_|bypass|disable')
    if [ -n "$hits" ]; then
        fail "the guard's public surface grew an off switch" "$hits" \
            "see above"
    fi

    # The event taxonomy already promises layer numbers 1-7 (`GuardLayerTriggered.layer`
    # documents T12.5's seven layers). `layer_of` must stay within that range: an 8 would
    # be a layer the taxonomy cannot name, and a 0 would be a refusal no layer owns.
    body=$(sed -n '/pub fn layer_of/,/^}/p' "$guard/layers.rs")
    for n in 0 8 9; do
        if printf '%s' "$body" | grep -q "=> $n,"; then
            fail "layer_of returns $n, outside the taxonomy's 1-7" "$body" \
                "GuardLayerTriggered.layer documents seven layers; the mapping must match"
        fi
    done
fi

# ---------------------------------------------------------------------------
# T14 - prompt ledger
#
# Four properties the compiler cannot see: append-only (no mutation path on the
# ledger), store-before-drop (eviction commits before the prefix forgets), thinking
# disposition per T13.5 (tool turns keep, prose turns drop), and generation
# verification (a rewrite proves it moved content rather than rewording it).
# ---------------------------------------------------------------------------
prompt=crates/supra_prompt/src

if [ -d "$prompt" ]; then
    # I1: the ledger exposes append and read-only views, never mutation. A `&mut`
    # accessor, a `remove`, a `clear` beyond the rewrite path, or an `into_inner`
    # would be a rewrite wearing an append's clothes. `clear_for_rewrite` is the
    # one sanctioned clearing, and it is checked separately below.
    hits=$(scan "$prompt/ledger.rs" '&mut Sealed|&mut self\.entries|-> &mut |fn remove|fn clear\b|into_inner|DerefMut|as_mut|get_mut')
    if [ -n "$hits" ]; then
        fail "the ledger grew a mutation path" "$hits" \
            "I1 is append-only; the only clearing is clear_for_rewrite"
    fi
    # The positive form: append takes an unsealed Segment, never a sealed one. If
    # callers sealed their own entries they could reserve, skip, or reuse positions -
    # gap-free sequencing is the ledger's act, not the caller's.
    if ! grep -q 'pub fn append(&mut self, segment: Segment)' "$prompt/ledger.rs"; then
        fail "ledger append no longer takes an unsealed Segment" \
            "sealing is the ledger's act; callers must not reserve positions"
    fi

    # T10-before-prefix: eviction commits to the store first, and only on success
    # returns the index entry. A probe swapping the order - dropping the turn, then
    # writing - must fail. Checked as data flow: a store commit must appear in
    # evict.rs. The call spans two lines (`store` newline `.evict_turn`), so match
    # the method name rather than the receiver spelling.
    if ! scan "$prompt/evict.rs" '\.evict_turn\(' | grep -q .; then
        fail "eviction no longer commits to the store" \
            "T10 must commit before the prefix drops the turn; no setting makes the other order safe"
    fi

    # T13.5 disposition: tool turns keep thinking verbatim, prose turns drop it. A
    # probe deleting either arm must fail - keeping everything wastes context the API
    # declares omissible; dropping everything earns a 400 on tool turns.
    body=$(sed -n '/pub fn render_body/,/^}/p' "$prompt/evict.rs")
    if ! printf '%s' "$body" | grep -q 'ThinkingDisposition::Keep'; then
        fail "eviction lost its Keep arm" "$body" \
            "tool turns must keep thinking verbatim (signature included) or the API 400s"
    fi
    if ! printf '%s' "$body" | grep -q 'ThinkingDisposition::Drop'; then
        fail "eviction lost its Drop arm" "$body" \
            "prose turns may drop thinking; keeping everything wastes context"
    fi
    # The decision must key on ToolUse presence across the whole turn, not on role,
    # position, or a flag. `blocks.first()` matching ToolUse passes a tool turn whose
    # call sits after prose - and `Segment::new` only forbids prose *between* tool
    # blocks, so leading prose is legal. Anything narrower than `.any()` lets a
    # caller smuggle thinking out of a tool turn.
    if ! scan "$prompt/evict.rs" 'blocks\.iter\(\)\.any' | grep -q .; then
        fail "the thinking disposition no longer scans the whole turn for ToolUse" \
            "T13.5 rule 1 is about presence anywhere, not first position"
    fi

    # Generation rewrite preserves order: verify_rewrite compares segment sequences,
    # and a rewrite that reorders must fail verification. A probe deleting the
    # comparison must fail.
    if ! scan "$prompt/generation.rs" 'before\.segments != after\.segments' | grep -q .; then
        fail "rewrite verification no longer compares segment sequences" \
            "a rewrite that reorders would certify as faithful"
    fi
    # The 92% threshold is a number in the document, not a mood. A probe lowering it
    # to 80 must fail.
    if ! grep -q 'usage_percent >= 92' "$prompt/generation.rs"; then
        fail "the rewrite threshold is no longer 92%" \
            "every avoided rewrite is one full 1-hour-TTL write not paid for"
    fi

    # The prefix hash covers (SeqNo, ContentHash) pairs, not content alone. A
    # position-blind hash equates two generations with different cache lifetimes -
    # and a rewrite re-seals every segment at new positions, so that equation would
    # verify a rewrite against the wrong generation.
    if ! scan "$prompt/ledger.rs" 'entry\.seq\(\)' | grep -q .; then
        fail "the prefix hash no longer covers sequence positions" \
            "content alone cannot tell resealed-same from same-positions"
    fi
fi

# ---------------------------------------------------------------------------
# T15 - repo digest
#
# Five properties the compiler cannot see: zero LLM calls (no provider
# dependency), lossless error handling (broken files contribute nothing, stale
# entries die with them), refused budgets (ten pointers, ~300 tokens, never
# truncated), pool hygiene (scratch entries die with the retrieval), and blast
# radius that follows internal edges only.
# ---------------------------------------------------------------------------
digest=crates/supra_digest/src

if [ -d "$digest" ]; then
    # Zero LLM calls is structural: the manifest must not name the provider
    # crate, and no module may import it. A probe adding the dependency must
    # fail - retrieval stays on the per-turn overhead budget, not the bill.
    if grep -q 'supra_llm' crates/supra_digest/Cargo.toml; then
        fail "supra_digest depends on supra_llm" \
            "orientation is zero LLM calls; gists are deterministic strings"
    fi
    hits=""
    # One file per scan: passing the directory made `production_lines` open a
    # directory and die with IsADirectoryError - the `|| true` in `scan` then
    # masked it as "no hits", so the guard certified what it could not see.
    while IFS= read -r -d '' file; do
        found=$(scan "$file" 'supra_llm::|use supra_llm')
        if [ -n "$found" ]; then
            hits="$hits$found
"
        fi
    done < <(find "$digest" -name '*.rs' -print0 | sort -z)
    if [ -n "$hits" ]; then
        fail "supra_digest reaches for a provider client" "$hits" \
            "ranks are integer arithmetic over T11's lanes, not completions"
    fi

    # Broken files contribute nothing: the error-node gate must precede the
    # harvest. A probe deleting the gate indexes guesses as facts - ranges
    # around the error are unknown, and an anchor pointing at them misdirects.
    body=$(sed -n '/pub fn parse_file/,/^}/p' "$digest/parse.rs")
    harvest_at=$(printf '%s\n' "$body" | grep -n 'harvest(' | head -1 | cut -d: -f1)
    gate_at=$(printf '%s\n' "$body" | grep -n 'has_error' | head -1 | cut -d: -f1)
    if [ -z "$harvest_at" ] || [ -z "$gate_at" ]; then
        fail "parse_file no longer gates the harvest on error nodes" \
            "one of the two halves is gone"
    elif [ "$gate_at" -gt "$harvest_at" ]; then
        fail "the error-node gate runs after the harvest" \
            "guesses indexed as facts misdirect anchors"
    fi

    # Budgets are refused, never truncated. A probe deleting either bound must
    # fail - an eleven-pointer suffix is a listing, not orientation, and tokens
    # past 300 silently exceed the window the suffix was sized for. The token
    # bound is checked as a comparison against SUFFIX_TOKENS (the `tokens >`
    # spelling), not by the constant's mere presence: the constant also names
    # the budget in the error, so presence alone proves nothing.
    body=$(sed -n '/pub fn check_budget/,/^}/p' "$digest/anchors.rs")
    if ! printf '%s' "$body" | grep -q 'MAX_ANCHORS'; then
        fail "the anchor-count bound is gone from check_budget" "$body" \
            "ten is a readability bound as well as a token bound"
    fi
    if ! printf '%s' "$body" | grep -q 'tokens > SUFFIX_TOKENS'; then
        fail "the suffix-token comparison is gone from check_budget" "$body" \
            "the constant naming the budget in the error does not enforce it"
    fi

    # Pool hygiene: every retrieve exit drains the scratch namespace. A pool
    # entry that survives its retrieval ranks deleted symbols in the next one.
    # A probe deleting a drain_pool call must fail.
    drains=$(scan "$digest/digest.rs" 'drain_pool' | wc -l | tr -d ' ')
    if [ "$drains" -lt "3" ]; then
        fail "expected drain_pool at 3 sites (def + 2 exits), found $drains" \
            "$(scan "$digest/digest.rs" 'drain_pool')" \
            "a leaked pool entry ranks deleted symbols"
    fi

    # Blast radius follows internal edges only. External targets resolve to no
    # file; walking them would join every importer of std into one radius.
    if ! scan "$digest/graph.rs" 'if !edge\.internal' | grep -q .; then
        fail "the blast-radius walk no longer skips external edges" \
            "every importer of std would join one radius"
    fi
fi

# ---------------------------------------------------------------------------
# T15.7 - structural edits
#
# Six properties the compiler cannot see: no duplicate harvest (T15 owns the
# grammar table), the reparse gate's three checks in order (exact range, clean
# result, same kind), back-to-front splice order, reachability before text in
# queries, shadow refusal before any splice in renames, and semantic:false on
# every syntactic result.
# ---------------------------------------------------------------------------
ast=crates/supra_ast/src

if [ -d "$ast" ]; then
    # One harvest, in T15: a second grammar table here would give two harvests
    # that can disagree about what a file declares. T15 owns grammars,
    # Language detection, and Symbol; this crate reuses all three.
    # One file at a time: `scan` reads a single file, and a directory argument
    # fails - which `|| true` downstream would mask as a pass (the T15.5
    # lesson: a guard that cannot see the violation certifies it).
    hits=$(for file in "$ast"/*.rs; do scan "$file" 'supra_digest::parse_file|supra_digest::ParseOutcome'; done)
    if [ -z "$hits" ]; then
        fail "supra_ast no longer reuses T15's harvest" \
            "a second harvest would disagree about what a file declares"
    fi
    # ...but reuse must be load-bearing, not decorative: an import that names
    # the function without calling it satisfies the check above while a local
    # copy does the work. The call site below is the load-bearing half - and
    # `scan` (not grep) so a doc comment naming the call does not satisfy it.
    if ! scan "$ast/query.rs" 'supra_digest::parse_file' | grep -q .; then
        fail "supra_ast names T15's harvest without calling it" \
            "a decorative import beside a local copy disagrees silently"
    fi
    dups=$(for file in "$ast"/*.rs; do scan "$file" 'fn declaration_kind|fn node_name|fn impl_target|fn harvest'; done)
    if [ -n "$dups" ]; then
        fail "supra_ast duplicates T15's harvest" "$dups" \
            "one harvest, in T15; this crate calls it"
    fi

    # The gate's three checks in order: exact range, clean result, same kind.
    # A probe deleting any one must fail - each refuses a different wrong edit.
    # Matched with just enough context to be unique, via `scan` (literals
    # blanked): `find_exact` is called in two places and `has_error` in two,
    # so the bare names would pass with one deleted. The context names the
    # decisive use of each, including the `else` that makes the range check a
    # refusal rather than a fallback.
    for check in 'find_exact\([^)]*root, start, end. else' 'has_error..after' 'node.kind.. != before_kind'; do
        if ! scan "$ast/splice.rs" "$check" | grep -q .; then
            fail "the reparse gate lost its $check check" \
                "each check refuses a different wrong edit"
        fi
    done

    # Back-to-front splice order: earlier offsets stay valid while later bytes
    # move. A probe deleting the descending sort must fail.
    if ! scan "$ast/rename.rs" 'sort_unstable_by' | grep -q .; then
        fail "rename no longer splices back to front" \
            "forward order shifts later offsets and the gate refuses them"
    fi

    # Reachability before text: a textual match outside the import graph is
    # coincidence, and coincidence renamed is corruption.
    if ! scan "$ast/query.rs" 'if !reachable' | grep -q .; then
        fail "query no longer gates on reachability" \
            "text without an import edge is coincidence"
    fi

    # Shadow refusal before any splice: silently creating a shadow rebinds every
    # existing reference. A probe deleting the WouldShadow check must fail.
    # Matched with context (`if let Some...find...*new`), not the bare variant
    # name: the variant also appears in docs and error definitions, which would
    # satisfy a name-only check with the enforcement deleted.
    if ! scan "$ast/rename.rs" 'outline\.iter\(\)\.find.*== \*new' | grep -q .; then
        fail "rename no longer refuses shadowing" \
            "a silent shadow rebinds every existing reference"
    fi

    # semantic:false on every syntactic result. The architecture document states
    # syntactic rename can be wrong under shadowing or overloading, so the flag
    # is a field the caller must read, not a footnote to remember.
    if ! scan "$ast/query.rs" 'semantic: false' | grep -q .; then
        fail "query results no longer carry semantic:false" \
            "the limitation must be read, not remembered"
    fi
    if ! scan "$ast/rename.rs" 'semantic: false' | grep -q .; then
        fail "rename results no longer carry semantic:false" \
            "the limitation must be read, not remembered"
    fi
fi

# ---------------------------------------------------------------------------
# T15.5 - cohort estimation
#
# Five properties the compiler cannot see: zero LLM calls (no provider
# dependency, enforced twice), integer-only scoring (no float), max-composition
# (order-independent rules), saturation (failures stop at 2), and the turn
# loop's escalation contract (unreachable now, blind mismatch skips E1).
# ---------------------------------------------------------------------------
cohort=crates/supra_cohort/src

if [ -d "$cohort" ]; then
    # Zero LLM calls is structural, twice over: the manifest must not name the
    # provider crate (the build fails first), and no module may import it (the
    # negative test fails second). A probe adding the dependency must fail.
    if grep -q 'supra_llm' crates/supra_cohort/Cargo.toml; then
        fail "supra_cohort depends on supra_llm" \
            "estimation is zero LLM calls; signals are integers, not completions"
    fi
    hits=$(scan "$cohort/score.rs" 'supra_llm::|use supra_llm|Client::new|reqwest::')
    if [ -n "$hits" ]; then
        fail "supra_cohort reaches for a provider client" "$hits" \
            "estimation reads integers and returns a tier"
    fi

    # Scoring is integer-only. A float confidence would invite averaging and
    # thresholding that reads as rigour while resting on a model's self-report -
    # and it would be the only float in the estimation path (T6's rule).
    # One file at a time: `scan` reads a single file, and a directory argument
    # makes production_lines fail - which `|| true` downstream would mask as a
    # pass. A guard that cannot see the violation certifies it.
    hits=$(scan "$cohort/signals.rs" '\bf32\b|\bf64\b|[0-9]\.[0-9]')
    hits="$hits$(scan "$cohort/score.rs" '\bf32\b|\bf64\b|[0-9]\.[0-9]')"
    hits="$hits$(scan "$cohort/admit.rs" '\bf32\b|\bf64\b|[0-9]\.[0-9]')"
    hits="$hits$(scan "$cohort/profile.rs" '\bf32\b|\bf64\b|[0-9]\.[0-9]')"
    if [ -n "$hits" ]; then
        fail "a float reached cohort estimation" "$hits" \
            "bands are ordinals, not magnitudes; see ARCHITECTURE.md section 4"
    fi

    # The ladder composes by maximum, not first match. A probe replacing
    # `tier.max` with early returns makes the outcome depend on rule order -
    # and rule order is the thing most likely to be edited casually.
    # Counted, not merely present: one surviving `tier.max` beside a new early
    # return still leaves the early return deciding overlapping evidence.
    maxes=$(scan "$cohort/score.rs" 'tier\.max\(Tier::E' | wc -l | tr -d ' ')
    if [ "$maxes" != "5" ]; then
        fail "expected tier.max at 5 ladder rules, found $maxes" \
            "$(scan "$cohort/score.rs" 'tier\.max\(Tier::E|return Tier::E')" \
            "first-match-wins makes overlapping evidence order-dependent"
    fi

    # Failures saturate at 2: the third failure teaches nothing the second did
    # not, and an unbounded counter pins every future task at E5. A probe
    # removing the saturation must fail.
    if ! scan "$cohort/profile.rs" 'repeat_failures >= 2|repeat_failures > 1|\.min\(2\)|saturat' | grep -q .; then
        fail "failure counting no longer saturates" \
            "one flaky shape would pin every future task at E5"
    fi

    # Unreachable quorum and blind mismatch skip E1: k=2 with quorum 2 is
    # unanimity-shaped scrutiny, and a model that disagrees with itself needs
    # peers that can disagree with each other. The sed range below spans the
    # whole `next_tier` function (both match arms); anchoring on the function
    # name alone matched only the first arm in an earlier draft, and a probe
    # routing FindingConfirmed to E1 would have passed uncaught.
    body=$(sed -n '/pub const fn next_tier/,/^    }$/p' "$cohort/admit.rs")
    if ! printf '%s' "$body" | grep -q 'Tier::E0 | Tier::E1 => Tier::E2'; then
        fail "escalation no longer skips E1" "$body" \
            "unanimity-shaped scrutiny cannot review self-disagreement"
    fi
    # And the skip-E1 arm must map E0 to E2, not E1: matching only the presence
    # of E2 anywhere would pass a body where some other arm mentions E2 while
    # the skip arm quietly routes to E1.
    skip_arm=$(printf '%s' "$body" | grep -A3 'UnreachableQuorum | Self::BlindMismatch')
    if ! printf '%s' "$skip_arm" | grep -q 'Tier::E0 | Tier::E1 => Tier::E2'; then
        fail "the skip-E1 arm no longer routes E0 to E2" "$skip_arm" \
            "unanimity-shaped scrutiny cannot review self-disagreement"
    fi
fi

# ---------------------------------------------------------------------------
# T16 + T16.5 - sandbox composition guards
#
# The fd audit and the pty session are composition points: each one holds a
# promise that lives across crates (the audit sees every spawn, the session
# never bypasses the sandbox, the pty never shows an unflagged descriptor to
# a concurrent audit). The guards below pin the load-bearing lines with
# enough context that a decorative mention elsewhere cannot satisfy them -
# the lesson from T15.5 and T15.7, where bare-name matches certified what
# they could not see.
#
# Probed: each guard was verified to fail against the mutation it names
# (the same M1-M9 the test suite catches), and to pass on the healthy tree.
# ---------------------------------------------------------------------------
sandbox="crates/supra_sandbox/src"
shell="crates/supra_shell/src"
ffi="crates/supra_ffi/src"

if [ -d "$sandbox" ]; then
    # The stdio exemption is one expression: fd > 2 AND !cloexec AND !allow.
    # A probe that drops only `record.fd > 2` would pass a guard matching the
    # two remaining conjuncts, so all three must appear together.
    leaks=$(scan "$sandbox/fd_audit.rs" 'record\.fd > 2 && !record\.cloexec && !allow')
    if [ -z "$leaks" ]; then
        fail "the leak predicate no longer exempts stdio in one expression" \
            "crates/supra_sandbox/src/fd_audit.rs" \
            "fds 0/1/2 are the child's streams, not leaks; every fd above them must be audited"
    fi

    # A descriptor that closed mid-walk is absence, not danger. The guard
    # anchors on the EBADF arm *inside the cloexec match*; a bare `EBADF`
    # elsewhere (the constant's own doc) must not satisfy it. Probed with
    # the arm commented out - a plain grep on the file matched the comment
    # and passed, which is the T15 lesson restated: comments are not code.
    vanish=$(sed -n '/let cloexec = match cloexec_flag/,/^        };/p' \
        "$sandbox/fd_audit.rs" 2>/dev/null |
        grep 'Some(EBADF) => continue' || true)
    if [ -z "$vanish" ]; then
        fail "a vanished descriptor is treated as a live leak again" \
            "crates/supra_sandbox/src/fd_audit.rs" \
            "a closed fd cannot cross an exec; reporting it as a leak is a false positive under concurrency"
    fi

    # The audit must run from spawn, before the guard and the FFI. `scan`
    # strips comments, so commenting the call out cannot satisfy the guard -
    # the probe that proved this passed a plain grep on the commented line.
    wired=$(scan "$sandbox/spawn.rs" 'audit_descriptors\(policy\)\?')
    if [ -z "$wired" ]; then
        fail "spawn no longer audits descriptors before exec" \
            "crates/supra_sandbox/src/spawn.rs" \
            "the T4 boundary is the whole point of the audit"
    fi

    # The tree budget counts every child the host started. Anchored on the
    # call site inside spawn (post-FFI), because TreeBudget's own unit tests
    # would survive a spawn that forgets to record; `scan` keeps a commented
    # call from satisfying the guard.
    recorded=$(scan "$sandbox/spawn.rs" 'tree\.record_child\(\);')
    if [ -z "$recorded" ]; then
        fail "spawn no longer records the child in the tree budget" \
            "crates/supra_sandbox/src/spawn.rs" \
            "the T12.5 catch for stripped env-markers is the count"
    fi
fi

if [ -d "$shell" ] && [ -d "$ffi" ]; then
    # CLOEXEC from the syscall: the flag must ride the posix_openpt call
    # itself. Anchored on the call with all three flags, so a decorative
    # `const O_CLOEXEC` cannot satisfy the guard.
    born=$(scan "$ffi/pty.rs" 'posix_openpt\(O_RDWR \| O_NOCTTY \| O_CLOEXEC\)')
    if [ -z "$born" ]; then
        fail "the pty master is no longer CLOEXEC from birth" \
            "crates/supra_ffi/src/pty.rs" \
            "openpty-style set-after leaves a window a concurrent audit sees as a leak"
    fi

    # The parent's slave copy closes right after the spawn - the T4 note.
    # `scan` keeps a commented-out call from satisfying the guard.
    take=$(scan "$shell/session.rs" 'pty\.take_slave\(\);')
    if [ -z "$take" ]; then
        fail "the session no longer closes the parent's slave copy" \
            "crates/supra_shell/src/session.rs" \
            "a held slave keeps the pair alive; the master would never see EOF"
    fi

    # The child's stdio is the pty slave, overriding whatever the request
    # said. Matching the assignment pins the override, and `scan` keeps a
    # comment from standing in for the code.
    stdio=$(scan "$shell/session.rs" 'request\.stdio = Stdio::all\(slave_fd\);')
    if [ -z "$stdio" ]; then
        fail "the session no longer wires stdio to the pty slave" \
            "crates/supra_shell/src/session.rs" \
            "a session whose child cannot talk to its own terminal is not a session"
    fi

    # Shaping routes through the confined scanner, never a reimplementation.
    # Anchored on the scanner call inside push (Eof::More); the finish() scan
    # uses Eof::Final and is pinned by its own test, not this guard.
    routed=$(scan "$shell/shaping.rs" 'self\.scanner\.tokens\(bytes, Eof::More\)')
    if [ -z "$routed" ]; then
        fail "the shaper no longer routes bytes through the C++ scanner" \
            "crates/supra_shell/src/shaping.rs" \
            "T3's C1-positional rule lives in libsupra_ansi; two parsers for one grammar disagree on the bytes that matter"
    fi
fi

# ---------------------------------------------------------------------------
# T16.6 - journal guards
#
# The journal is the R1 class the permission engine prices edits against:
# "auto is the default only because T16.6 exists." Two of its promises are
# structural and unprobeable from a userspace test (fsync durability, the
# single-transaction undo), so they live here as pinned lines rather than
# as test assertions. All guards run through `scan`, so a commented-out
# line cannot satisfy them - the T15.7 lesson, applied from birth this
# time.
# ---------------------------------------------------------------------------
journal="crates/supra_journal/src"

if [ -d "$journal" ]; then
    # Fsync is the undo's durability: without it, a restore is a cache entry
    # the kernel may drop. No userspace test can observe a power loss, so
    # the call is pinned structurally - mutation M3 survived the suite and
    # is caught here instead.
    flushed=$(scan "$journal/journal.rs" 'file\.sync_all\(\)\?;')
    if [ -z "$flushed" ]; then
        fail "undo no longer flushes the restored bytes" \
            "crates/supra_journal/src/journal.rs" \
            "an unflushed restore is a cache entry the kernel may drop; M3 survived the suite and is caught here"
    fi

    # The write-ahead order: snapshot must commit the row before the caller
    # edits. The insert call sits inside snapshot()'s transaction; a guard
    # matching only the function name would pass a snapshot that reads but
    # never stores.
    stored=$(scan "$journal/journal.rs" 'schema::insert\(transaction, id, &path_text, &bytes, &digest\)\?;')
    if [ -z "$stored" ]; then
        fail "snapshot no longer commits the row before the edit" \
            "crates/supra_journal/src/journal.rs" \
            "T14's store-before-drop, restated for files: an edit without a committed snapshot is not R1"
    fi

    # Undo verifies the digest before writing: a damaged row must restore
    # nothing. The comparison (not the digest call alone) is the guard's
    # subject, because the digest is also computed in snapshot().
    verified=$(scan "$journal/journal.rs" 'if computed != stored_digest')
    if [ -z "$verified" ]; then
        fail "undo no longer verifies the stored digest before writing" \
            "crates/supra_journal/src/journal.rs" \
            "an undo that restores approximately the original is worse than no undo"
    fi

    # created_at is the id's own timestamp, not a second clock read: two
    # reads disagree when the millisecond turns, and ORDER BY created_at
    # could then name the older snapshot as newest. The flake that proved
    # it produced exactly that once in ~15 runs; M9 closed it and this
    # guard keeps it closed.
    stamped=$(scan "$journal/schema.rs" 'snapshot\.timestamp_ms\(\)')
    if [ -z "$stamped" ]; then
        fail "created_at no longer comes from the snapshot id" \
            "crates/supra_journal/src/schema.rs" \
            "two clock reads disagree on the millisecond boundary; the newest report would name the older snapshot"
    fi

    # The mark and the write share one transaction: mark_undone inside
    # undo's with_transaction. A mark that ran outside it would let two
    # concurrent undoes both pass the read.
    marked=$(scan "$journal/journal.rs" 'schema::mark_undone\(transaction, snapshot\)\?;')
    if [ -z "$marked" ]; then
        fail "the undo mark no longer shares the undo's transaction" \
            "crates/supra_journal/src/journal.rs" \
            "outside the transaction, two concurrent undoes both pass the read and both write"
    fi

    # The snapshot digest is domain-separated from T10's turn body: kind
    # 0x11, not 0x10. A collision between a turn body and a file snapshot
    # hashing the same bytes would look like verification.
    kinds=$(scan "$journal/journal.rs" 'CANONICAL_KIND: u8 = 0x11;')
    if [ -z "$kinds" ]; then
        fail "the snapshot digest no longer uses its own canonical kind" \
            "crates/supra_journal/src/journal.rs" \
            "0x10 is the turn body's kind; a shared kind makes a cross-type collision look like verification"
    fi
fi

# ---------------------------------------------------------------------------
# T16.7 - permission gate guards
#
# The gate is a composition point: catalogue -> rules -> authority ->
# matrix, plus the escape-hatch exception and the batch's unanswered-is-no.
# Each guard pins a load-bearing line through `scan`, so a comment cannot
# stand in for code. Probed against the same M1-M9 the suite catches.
# ---------------------------------------------------------------------------
permission="crates/supra_permission/src"

if [ -d "$permission" ]; then
    # Authority is consulted before the mode: the two-axis property. The
    # guard anchors on the check itself, inside the gate function's order -
    # a decorative `permits` elsewhere must not satisfy it.
    authority=$(scan "$permission/gate.rs" 'if !request\.class\.permits\(request\.invoker\) \{')
    if [ -z "$authority" ]; then
        fail "the gate no longer consults authority before consent" \
            "crates/supra_permission/src/gate.rs" \
            "no mode - yolo included - can widen the authority axis"
    fi

    # Damping routes through the structural shape only: the is_structural
    # condition guarding damped() is the measurable version of
    # verifiability-damps-risk.
    damped=$(scan "$permission/gate.rs" 'if request\.effect\.is_structural\(\) \{')
    if [ -z "$damped" ]; then
        fail "damping no longer routes through the structural shape" \
            "crates/supra_permission/src/gate.rs" \
            "verified structure earns one class lower; damping everything would soften the catalogue's R3s"
    fi

    # The escape hatch asks in every mode. Anchored on the match plus the
    # EscapeHatch arm, so the shape's name alone (present in docs and the
    # catalogue) cannot satisfy the guard.
    hatch=$(scan "$permission/gate.rs" 'matches!\(request\.effect, Effect::EscapeHatch \{ \.\. \}\)')
    if [ -z "$hatch" ]; then
        fail "an escape hatch no longer asks in every mode" \
            "crates/supra_permission/src/gate.rs" \
            "consent is what yolo pre-grants; stepping aside from a guard is not consent's to grant"
    fi

    # Unanswered is not consent: the batch's missing-answer default is
    # false. The repeat(&false) expression is the one-sentence contract.
    unanswered=$(scan "$permission/gate.rs" 'repeat\(&false\)')
    if [ -z "$unanswered" ]; then
        fail "unanswered batch items no longer refuse" \
            "crates/supra_permission/src/gate.rs" \
            "a question the user did not answer is not consent"
    fi

    # Rules are consulted before everything: resolve(rules) is the gate's
    # first move. A gate that evaluated the matrix first would let a deny
    # be outrun by an ask.
    rules_first=$(scan "$permission/gate.rs" 'if let Some\(effect\) = resolve\(rules\) \{')
    if [ -z "$rules_first" ]; then
        fail "the gate no longer consults rules first" \
            "crates/supra_permission/src/gate.rs" \
            "deny wins literally; the rule set outranks the mode matrix"
    fi
fi

# ---------------------------------------------------------------------------
# T17 - tool registry guards
#
# The registry's load-bearing properties are structural: I3's frozen
# manifest, the single-serialiser rule for arguments, and the
# precondition's instruction-carrying refusal. All through `scan`, so a
# comment cannot stand in for code. Probed against the same M1-M9 the
# suite catches.
# ---------------------------------------------------------------------------
toolsrc="crates/supra_tool/src"

if [ -d "$toolsrc" ]; then
    # Arguments canonicalise through T13's serialiser - the only sanctioned
    # producer of CanonicalJson. A registry that parsed and re-serialised
    # would give I7 two implementations that can disagree.
    canonical=$(scan "$toolsrc/registry.rs" 'canonicalize\(arguments\)\?')
    if [ -z "$canonical" ]; then
        fail "invocations no longer canonicalise through T13's serialiser" \
            "crates/supra_tool/src/registry.rs" \
            "the bytes that were hashed must be the bytes that reach the wire"
    fi

    # The precondition check runs on every invocation: the §5 property.
    # Anchored on the call inside invoke's sequence, after validation.
    preconditions=$(scan "$toolsrc/registry.rs" 'tool\.check_precondition\(map, facts\)\?;')
    if [ -z "$preconditions" ]; then
        fail "invocations no longer check the precondition" \
            "crates/supra_tool/src/registry.rs" \
            "the workflow is the only path the tool surface permits; a skipped precondition is a prose instruction waiting to happen"
    fi

    # The manifest lists every registered tool, disabled or not: I3's
    # byte-stability. The guard anchors on the manifest() body's iterator
    # over all tools - a filter on disabled would be the mutation.
    # Inverted anchor: the manifest's iterator must not filter on the
    # disabled set. M9 added `.filter(|tool| !self.disabled...)` before the
    # map; a positive anchor on values() cannot see a filter, so the guard
    # asserts its absence inside the manifest function's own body.
    manifest_body=$(sed -n '/pub fn manifest(/,/^    }/p' "$toolsrc/registry.rs")
    if printf '%s' "$manifest_body" | grep -q 'filter'; then
        fail "the manifest filters tools out instead of listing them all" \
            "crates/supra_tool/src/registry.rs" \
            "disabling is a session set, not a manifest mutation; a manifest that changed would break the BP1 prefix"
    fi

    # The read-before-edit precondition names the tool and the file in its
    # instruction: the refusal the model can follow. A bare Ok(()) rule
    # would be a barrier, not an instruction.
    # `scan_sql`, not `scan`: the instruction lives inside a format-string
    # literal, and `scan` blanks string contents by design - the subject
    # here IS the literal, exactly the SQL case the second helper exists
    # for. The pattern avoids brace characters because ERE reads them as
    # interval openers; the instruction's own words carry the anchor.
    instruction=$(scan_sql "$toolsrc/registry.rs" 'read_file .* before editing it')
    if [ -z "$instruction" ]; then
        fail "the read-before-edit refusal no longer carries an instruction" \
            "crates/supra_tool/src/registry.rs" \
            "the message is the model's only channel; the retry has to be able to be correct"
    fi
fi

# ---------------------------------------------------------------------------
# T18 - MCP gateway guards
#
# The gateway's load-bearing properties: the cache's append-only rule (I3's
# compatibility trick), the static tool's argument-blind resolver, and the
# stdio child's explicit env. All through `scan`/`scan_sql` as the subject
# demands; probed against the same M-series the suite catches.
# ---------------------------------------------------------------------------
mcpsrc="crates/supra_mcp/src"

if [ -d "$mcpsrc" ]; then
    # Append-only: INSERT OR IGNORE, never REPLACE - a replaced row edits
    # the manifest the session promised was frozen.
    appended=$(scan_sql "$mcpsrc/cache.rs" 'INSERT OR IGNORE INTO')
    if [ -z "$appended" ]; then
        fail "the manifest cache no longer appends" \
            "crates/supra_mcp/src/cache.rs" \
            "an upsert edits the frozen manifest; append does not break a prefix, replacement does"
    fi

    # The static gateway tool's resolver is argument-blind. The resolver
    # line appears twice (primary and fallback, identical by design), so a
    # positive anchor cannot tell which one it sees. Inverted, like the
    # manifest guard: inside gateway_tool()'s own body, no resolver may
    # name its parameter - `|args|` or any binding that reads the
    # arguments is the mutation.
    gateway_body=$(sed -n '/pub fn gateway_tool()/,/^    }/p' "$mcpsrc/gateway.rs")
    if printf '%s' "$gateway_body" | grep -E '\|(args?|_[a-z])\|' | grep -qv 'supra_tool::Field'; then
        fail "the gateway tool's resolver is no longer argument-blind" \
            "crates/supra_mcp/src/gateway.rs" \
            "what the servers offer changes content, never the tool"
    fi

    # The stdio child's env is explicit: env_clear before envs.
    cleared=$(scan "$mcpsrc/transport.rs" '\.env_clear\(\)')
    if [ -z "$cleared" ]; then
        fail "the stdio child no longer runs with an explicit env" \
            "crates/supra_mcp/src/transport.rs" \
            "the host env holds secrets no third-party server needs"
    fi

    # The budget is consulted at probe, before anything is cached.
    budgeted=$(scan "$mcpsrc/gateway.rs" 'listed\.tools\.len\(\) > TOOLS_PER_SERVER')
    if [ -z "$budgeted" ]; then
        fail "the per-server discovery budget is no longer enforced" \
            "crates/supra_mcp/src/gateway.rs" \
            "a server that advertises thousands of tools would spend the manifest budget on one remote"
    fi
fi

# ---------------------------------------------------------------------------
# T19 - skill loader guards
#
# The loader's load-bearing properties: reload-through-a-copy (a failed
# reload never adopted), the verbatim body, and cycle detection with the
# cycle named. Through `scan`/`scan_sql`; probed against the same
# M-series the suite catches.
# ---------------------------------------------------------------------------
skillsrc="crates/supra_skill/src"

if [ -d "$skillsrc" ]; then
    # Reload adopts a copy only after it resolved: the candidate-clone
    # pattern. A direct apply on self is M5's mutation.
    reloaded=$(scan "$skillsrc/loader.rs" 'let mut candidate = self\.clone\(\);')
    if [ -z "$reloaded" ]; then
        fail "reload no longer applies events to a copy" \
            "crates/supra_skill/src/loader.rs" \
            "a failed reload must leave the session with the skills it had"
    fi

    # The body is verbatim: to_owned, not trim - M9's mutation.
    verbatim=$(scan_sql "$skillsrc/skill.rs" 'body: body\.to_owned\(\)')
    if [ -z "$verbatim" ]; then
        fail "the skill body is no longer verbatim" \
            "crates/supra_skill/src/skill.rs" \
            "the model sees the author's formatting; a reflowed body is a different skill"
    fi

    # Cycle detection exists and builds the arrow-joined path: the
    # visiting-stack position search is the detection, the join is the
    # message an author can act on.
    cycles=$(scan "$skillsrc/loader.rs" 'position\(\|entry\| entry == name\)')
    if [ -z "$cycles" ]; then
        fail "cycle detection no longer names the cycle" \
            "crates/supra_skill/src/loader.rs" \
            "'cycle' alone sends the author hunting through every skill they wrote"
    fi
fi

# ---------------------------------------------------------------------------
# T20 - plugin host guards
#
# Isolation by absence is structural: the linker for a class holds exactly
# the class's imports, fuel is enabled, and the fuel trap is classified.
# Through `scan`/`scan_sql`; probed against the same M-series the suite
# catches.
# ---------------------------------------------------------------------------
pluginsrc="crates/supra_plugin/src"

if [ -d "$pluginsrc" ]; then
    # Fuel is on at the engine: without it, an infinite-loop guest is a
    # hang, not a refusal - M4's mutation.
    fuelled=$(scan "$pluginsrc/host.rs" 'config\.consume_fuel\(true\);')
    if [ -z "$fuelled" ]; then
        fail "the plugin engine no longer enables fuel" \
            "crates/supra_plugin/src/host.rs" \
            "an infinite-loop guest must die in milliseconds, not hang the turn"
    fi

    # The store carries the budget: enabled fuel without a set budget is
    # unlimited fuel - M5's mutation.
    budgeted=$(scan "$pluginsrc/host.rs" 'store\.set_fuel\(DEFAULT_FUEL\)\?;')
    if [ -z "$budgeted" ]; then
        fail "the plugin store no longer carries the fuel budget" \
            "crates/supra_plugin/src/host.rs" \
            "enabled-but-unset fuel is unlimited fuel"
    fi

    # Instantiate runs the verify itself: a caller that skips verify still
    # cannot link an over-classed component - M1's mutation, the paranoid
    # path.
    selfverified=$(scan "$pluginsrc/host.rs" 'self\.verify\(name, component, class\)\?;')
    if [ -z "$selfverified" ]; then
        fail "instantiate no longer verifies the import set itself" \
            "crates/supra_plugin/src/host.rs" \
            "a verify-less caller must still be refused; the paranoid path is the pinned path"
    fi

    # The fuel trap is classified by name: grepping the chain is the
    # classifier, and the word it greps for is load-bearing - M6's
    # mutation greps for nothing.
    # `scan_sql`, not `scan`: the classifier's subject is the string
    # literal "fuel" itself, which `scan` blanks by design - the same
    # case as T17's instruction guard.
    classified=$(scan_sql "$pluginsrc/host.rs" 'message\.contains\("fuel"\)')
    if [ -z "$classified" ]; then
        fail "the fuel trap is no longer classified" \
            "crates/supra_plugin/src/host.rs" \
            "a fuel exhaustion that reports as a generic trap hides the budget from the operator"
    fi
fi

# ---------------------------------------------------------------------------
# T21 - blackboard guards
#
# The peer contract's load-bearing lines: proposer exclusion at vote,
# membership through the validators map, unreachable closes the claim.
# Through `scan`; probed against the same M-series the suite catches.
# ---------------------------------------------------------------------------
bbsrc="crates/supra_blackboard/src"

if [ -d "$bbsrc" ]; then
    excluded=$(scan "$bbsrc/board.rs" 'if voter == state\.proposer \{')
    if [ -z "$excluded" ]; then
        fail "a proposer may vote on its own claim" \
            "crates/supra_blackboard/src/board.rs" \
            "T12.5 L7: a proposer's vote never counts toward its own claim"
    fi

    membership=$(scan "$bbsrc/board.rs" 'state\.validators\.get_mut\(&voter\)')
    if [ -z "$membership" ]; then
        fail "cohort membership is no longer checked through the roster" \
            "crates/supra_blackboard/src/board.rs" \
            "a stranger must have no slot to vote through"
    fi

    closed=$(scan "$bbsrc/board.rs" 'state\.status = outcome;')
    if [ -z "$closed" ]; then
        fail "a claim's status no longer updates per vote" \
            "crates/supra_blackboard/src/board.rs" \
            "the turn loop evaluates after every vote; a stale status hangs the loop"
    fi
fi

# ---------------------------------------------------------------------------
# T22 - introspector guards
#
# A gate that cannot run must refuse, and a bridge vote must carry the
# finding's evidence. Through `scan`; probed against the same M-series.
# ---------------------------------------------------------------------------
introsrc="crates/supra_introspector/src"

if [ -d "$introsrc" ]; then
    spawned=$(scan "$introsrc/gates.rs" 'IntrospectError::Spawn\(')
    if [ -z "$spawned" ]; then
        fail "a gate that cannot run no longer refuses" \
            "crates/supra_introspector/src/gates.rs" \
            "a gate that cannot run is a refusal, not a pass"
    fi

    evidenced=$(scan "$introsrc/bridge.rs" 'Some\(finding\.evidence_ref\(\)\)')
    if [ -z "$evidenced" ]; then
        fail "a bridge vote no longer carries the finding's evidence" \
            "crates/supra_introspector/src/bridge.rs" \
            "a confirming peer's verdict must point at the finding it confirmed"
    fi

    denied=$(scan_sql "$introsrc/gates.rs" '.D., .warnings')
    if [ -z "$denied" ]; then
        fail "the clippy gate no longer denies warnings" \
            "crates/supra_introspector/src/gates.rs" \
            "plain clippy exits 0 on warnings; the workspace's zero-warning bar is the gate's own bar"
    fi
fi

# ---------------------------------------------------------------------------
# T23 - turn loop guards
#
# The loop's load-bearing properties: agreement is the vote, the
# escalation abort is published, and the ceiling is checked. Through
# `scan`; probed against the same M-series.
# ---------------------------------------------------------------------------
coresrc="crates/supra_core/src"

if [ -d "$coresrc" ]; then
    agreement=$(scan "$coresrc/turn.rs" 'answer_is_supported\(&answer\).*Vote::Yes.*Vote::No')
    if [ -z "$agreement" ]; then
        fail "a disagreeing peer no longer votes no" \
            "crates/supra_core/src/turn.rs" \
            "a quorum over answers is a quorum: agreement is the vote"
    fi

    escalation=$(scan_sql "$coresrc/turn.rs" 'escalating now')
    if [ -z "$escalation" ]; then
        fail "unreachable no longer names its escalation" \
            "crates/supra_core/src/turn.rs" \
            "the abort reason is what the TUI shows; 'zz' is not it"
    fi

    # Inverted, like the T18 manifest guard: `if false && agents.len() >
    # PEER_CEILING` still contains the pattern, so a positive anchor
    # cannot see the disabling. The check must appear without a
    # false-guard prefix on its line.
    ceiling=$(scan "$coresrc/turn.rs" 'agents\.len\(\) > PEER_CEILING' | grep -v 'false &&' || true)
    if [ -z "$ceiling" ]; then
        fail "the turn loop no longer checks the peer ceiling" \
            "crates/supra_core/src/turn.rs" \
            "no configuration raises PEER_CEILING, and nothing may silently field more"
    fi
fi

# ---------------------------------------------------------------------------
# T24 - LSP guards
#
# The flip and the crash story: the server-answered path must mark its
# results semantic, and a gate that cannot run must refuse. Through
# `scan`; probed against the same M-series.
# ---------------------------------------------------------------------------
lspsrc="crates/supra_lsp/src"

if [ -d "$lspsrc" ]; then
    # Server-answered references are semantic: the literal the production
    # site emits is the flip itself. The construction inside a test
    # carries the same literal but cannot observe its mutation, so the
    # guard reads this site - the same defence-in-depth shape M1 proves.
    # Production-only: `scan` strips cfg(test) bodies by tracking the
    # brace depth from `#[cfg(test)]`, so the test's own `semantic: true`
    # (identical literal) cannot satisfy this guard. Below, `scan_sql` would
    # be wrong: the literal is a value, not a string subject the scan
    # blanks. Bare `scan` on this site is the right scanner, once test
    # bodies are excluded.
    flipped=$(scan "$lspsrc/client.rs" 'semantic: true')
    if [ -z "$flipped" ]; then
        fail "server-answered references no longer mark semantic true" \
            "crates/supra_lsp/src/client.rs" \
            "the AST ships semantic false; the server's answer is the one place that flips it"
    fi

    restarted=$(scan "$lspsrc/client.rs" 'spawn_child\(\)\?;')
    if [ -z "$restarted" ]; then
        fail "a crashed server no longer restarts" \
            "crates/supra_lsp/src/client.rs" \
            "one restart, not a loop: a server that dies twice is Crashed"
    fi

    # Coverage is by lookup, never by guessing: the same refusal
    # Language::detect makes for unknown extensions. Guarded through the
    # Uncovered variant this path returns, because the coverage check
    # itself is a language branch the schema already owns.
    covered=$(scan "$lspsrc/client.rs" 'LspError::Uncovered')
    if [ -z "$covered" ]; then
        fail "uncovered languages no longer refuse" \
            "crates/supra_lsp/src/client.rs" \
            "a server started against the wrong grammar reports references that do not exist"
    fi

    lspsrc="crates/supra_lsp/src"
    uncovered_in_servers=$(scan "$lspsrc/servers.rs" 'fn for_language\(')
    if [ -z "$uncovered_in_servers" ]; then
        fail "language coverage is gone" \
            "crates/supra_lsp/src/servers.rs" \
            "five servers cover seven languages by lookup, never by guessing"
    fi
fi

# ---------------------------------------------------------------------------
# T25 - DAP guards
#
# The client correlates responses by request_seq and reports failure as
# failure; the breakpoint confirmation carries the adapter's line and
# verified word. Through `scan`; probed against the M-series.
# ---------------------------------------------------------------------------
dapsrc="crates/supra_dap/src"

if [ -d "$dapsrc" ]; then
    correlated=$(scan "$dapsrc/client.rs" 'if seen == request_seq')
    if [ -z "$correlated" ]; then
        fail "responses no longer correlate by request_seq" \
            "crates/supra_dap/src/client.rs" \
            "a stale answer to an earlier request must not satisfy a later one"
    fi

    failures=$(scan "$dapsrc/client.rs" 'if success \{')
    if [ -z "$failures" ]; then
        fail "failed responses no longer report failure" \
            "crates/supra_dap/src/client.rs" \
            "a refused command that reports success is a silent lie"
    fi

    confirmed=$(scan_sql "$dapsrc/client.rs" 'confirmed\.get\("line"\)')
    if [ -z "$confirmed" ]; then
        fail "the confirmed breakpoint no longer reads the adapter's line" \
            "crates/supra_dap/src/client.rs" \
            "a requested line can move; the confirmed position is the one the stop reports"
    fi

    verified=$(scan_sql "$dapsrc/client.rs" 'confirmed\.get\("verified"\)')
    if [ -z "$verified" ]; then
        fail "the breakpoint no longer reads the adapter's verified word" \
            "crates/supra_dap/src/client.rs" \
            "verified is the adapter's word that the breakpoint binds"
    fi
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
    echo "            the configuration schema, credential resolution, diagnostics, the event bus,"
    echo "            the turn store, hybrid retrieval, the store's connection, anti-self-spawn,"
    echo "            the prompt ledger, the repo digest, and internal versions"
fi
exit "$status"
