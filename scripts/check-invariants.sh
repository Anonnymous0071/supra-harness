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
        line = SQL_COMMENT.sub("", line)
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
    echo "            and internal versions"
fi
exit "$status"
