# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **T16.7** `crates/supra_permission`: the host-side permission gate -
  reversibility classification of resolved effects, the four-mode consent
  matrix composed through rules and authority, and batched prompts.
  - **The catalogue**: `Effect`, a closed enumeration of effect shapes,
    classified exhaustively. Classification runs on the resolved effect,
    never the tool name - `shell_run("cargo test")` is R0 and
    `shell_run("rm -rf node_modules")` is R3, and the difference is data
    the caller supplies (`Remove { target }`, `RecoverableEdit { basis }`),
    not a substring this crate guesses at. A string classifier would be a
    second grammar that disagrees with the shell about `env rm` and `\rm` -
    the T15.7 two-parsers lesson, restated for classification.
  - **The gate**: rules first (deny wins literally; an allow
    short-circuits; no rule falls through to the matrix), authority second
    (before the mode is examined, so no mode can widen it), the matrix
    third - with the escape-hatch exception: a guard-stepping-aside effect
    asks in every mode, `yolo` included, because consent is not the axis a
    sandbox rides on. Damping routes through the structural shape only:
    under `ask`, a blind edit prompts and a verified splice runs, which is
    verifiability-damps-risk made measurable.
  - **Batched prompts**: many questions become one prompt, answers come
    back per item, and an unanswered item refuses - a question the user
    did not answer is not consent. An empty batch is `EmptyBatch`, not an
    empty approval.
  - `RuleSource` gained `Display` in `supra_types` (the lowercase word a
    refusal message shows), the only T6 surface this stage needed.
  - Ten mutations: nine CAUGHT, control survived by design. Five guards in
    `check-invariants.sh`, all through `scan()`, probed 5/5 caught. A
    sixteen-cell walk pins the gate's composition against T6's matrix so
    the two cannot drift.
- **T16.6** `crates/supra_journal`: write-ahead file snapshots and atomic
  undo - the R1 reversibility class that makes `auto` the default mode.
  - **`snapshot`** reads a file, digests it, and commits the row in one
    `Immediate` transaction, before the caller's edit may touch the file:
    T14's "store before drop", restated for files. A missing file is an
    error, not an empty snapshot - an undo that "restores" nothing would
    report success while reverting nothing.
  - **`undo`** spans one `Immediate` transaction across read, verify,
    write, fsync, and mark. The store's lock is the only arbiter two
    concurrent undoes must both pass, so the file write happens inside it -
    a dedicated two-thread test pins that exactly one undo wins. A damaged
    row restores nothing (`Corrupt`, no bytes offered - I4's reasoning
    verbatim); the corruption probe had to be a same-length mutation
    because the schema's own CHECKs refuse the lazier corruptions first.
    A crash between write and mark is idempotent: undoing again rewrites
    the same bytes.
  - **`SnapshotId`** joins `supra_types` as the sixth ULID-backed distinct
    id; `snapshot_digest` is domain-separated at canonical kind `0x11`
    (T10's turn body holds `0x10`), pinned by a domain-separation test.
  - **`created_at` comes from the id, not a second clock read.** Two clock
    reads disagree whenever the millisecond turns between them, and
    `ORDER BY created_at DESC` could then name the *older* snapshot as
    newest - a real flake, caught once in ~15 runs and closed structurally;
    a test and a guard both pin the derivation.
  - Schema as the second `schema_component` owner (`"journal"`, forward
    only, dense from 1), with T10's CHECK discipline: 26-char id, 32-byte
    digest, `byte_len = length(before)`, `undone IN (0, 1)`.
  - Nine mutations: seven CAUGHT; M3 (fsync dropped) survived the suite
    because no userspace test can observe a power loss, and is caught by a
    structural guard instead; M8 control survived by design. Six guards in
    `check-invariants.sh`, all written through `scan()` from birth (the
    T15.7 lesson) and probed 6/6 caught on mutated code.
  - Cleanup: `ulid` removed from `supra_sandbox`'s dependencies - a dead
    dependency whose comment promised a "per-process ticket id" that the
    distinct-id types in `supra_types` already own.
- **T16.5** `crates/supra_shell`: persistent pty shell sessions with
  deterministic output shaping, spawned through the T16 sandbox.
  - **`supra_ffi::pty`**: pty primitives behind the confined `unsafe` -
    `posix_openpt`/`grantpt`/`unlockpt` plus the slave `open`, both with
    `O_CLOEXEC` from the syscall that creates the descriptor. `openpty(3)`
    was rejected with a measured reason: it returns both descriptors
    unflagged, leaving a window in which a concurrent fd audit sees the
    pair as a leak. Resize/size round-trip through `TIOCSWINSZ`/
    `TIOCGWINSZ`; the parent's slave copy closes on `take_slave` so the
    master sees EOF when the child exits.
  - **`ShellSession`**: one pty pair, one child, one `Shaper`. The child
    is spawned through `supra_sandbox::spawn` with the slave fd as all
    three standard streams, an explicit environment, and the full audit
    before exec. There is no unsandboxed path in this crate - `yolo`
    cannot reach one, by construction. `read` blocks until a byte arrives
    (the TUI owns the reader thread); EOF and post-exit `EIO` both return
    `Ok(None)` so a draining reader finishes cleanly.
  - **`Shaper`**: deterministic transcript from raw pty bytes. Every byte
    routes through the C++ scanner (T3's C1-positional rule binds the
    shaping - no reimplementation), with explicit C0 semantics (`\n`
    commits a line, `\r` returns the cursor to column zero, backspace
    steps left, other controls drop) and per-line cell-accurate truncation
    through the C++ planner. The same byte stream shapes to the same
    transcript whatever the chunk boundaries - pinned by a test that cuts
    mid-sequence on purpose. The prompt heuristic is a flag the caller
    reads, never a trigger this crate pulls (T4: false positives ask the
    user).
  - **Two parallel-test races, closed structurally.** The vanish race: a
    session dropping its pty while another test's audit walks
    `/proc/self/fd` produced `LeakyDescriptor { path: "" }` - a closed
    descriptor reported as a leak. The audit now treats a mid-walk
    EBADF/ENOENT as absence, not danger. The probe race: tests that
    manufacture leaks by clearing CLOEXEC shared a binary with tests whose
    spawns must see a clean host; a test-only `AUDIT_LOCK` serialises
    them. Verified with ten consecutive parallel runs per crate.
  - Nine mutations: eight CAUGHT, one control survived by design. M1
    (drop `take_slave`) is closed by an EOF-deadline test that turns the
    would-be hang into a failure; M9 (vanished fd as live leak) by a
    synthetic-directory unit test, deterministic where the race it models
    is not.
- **T16** `crates/supra_sandbox`: host-side choreography of the C++ sandbox -
  fd audit, process-tree budget, and the three-step spawn (audit → guard → FFI).
  - **Pre-spawn fd audit** walks `/proc/self/fd`, reads each `FD_CLOEXEC` flag
    through `supra_ffi::fd`, and refuses on the first descriptor that is
    without the flag and not on the policy's allow list. The T4 note is the
    binding: "descriptors inherited across `exec` remain usable. T16 and T16.5
    must close descriptors they do not intend to pass." Standard streams
    (fd 0/1/2) are excluded because a TTY-attached child must talk to the
    user; the boundary is the descriptors *above* stdio, and that is the only
    shape the audit recognises.
  - **Three-step spawn**: audit first, then the seven-layer guard, then the
    FFI. A refusal at any step returns without touching the next. The order
    is the product, and `scripts/mutate.sh` proved each step is test-enforced
    rather than convention-enforced.
  - **`TreeBudget`** is the host-side catch for what the guard cannot see:
    when an env-marker layer is stripped, the counter still counts the
    consequence. Cumulative and burst counters; the TUI reads the burst.
  - **`supra_ffi::fd`** gained the primitive the audit calls: a hand-rolled
    `fcntl` for `F_GETFD`/`F_SETFD`/`FD_CLOEXEC`. Confined to the only `unsafe`
    crate in the workspace; no other crate holds `unsafe`.
  - **Five refusal shapes** with the operator in mind: `Authority` (guard
    said no, here is the layer list), `Consent` (gate said no, here is the
    mode/reversibility), `LeakyDescriptor` (audit said no, here is the fd),
    `Unsupported` (the platform cannot enforce), `Ffi` (C side refused,
    message verbatim). Deny-wins is preserved: `yolo` skips consent; nothing
    here skips authority.
  - Seven mutations: M1/M2/M3/M4/M5/M6/M7 CAUGHT. The two that survived
    first (M1: a unit test for `record_child` could not be a passing suite
    without an end-to-end spawn; M2: the audit's *helper* was tested but
    not the call site) closed with end-to-end fixtures that exercise the
    full `spawn` path. Eight guard checks, all probed.
- **T15.7** `crates/supra_ast`: byte-range splice, reparse gate, outline, query,
  rename - the call-graph precision T15 promised, syntactic only.
  - **`replace_node` with three gates**: exact range (no rounding, empty matches
    nothing), clean result (errors anywhere refuse), same kind (cross-kind is
    delete plus insert, refused as a splice). Source never mutated in place;
    formatting preserved by the absence of a formatter.
  - **Query narrows by reachability, confirms by tree**: import edge first,
    whole-identifier comparison second (`recall_turntable` never matches).
    Outlines render T15's harvest with depth; renames splice back to front,
    refuse shadows, return files without touching the filesystem.
  - **`semantic: false` is a field**, not a footnote - T24 flips what it
    computes without reshaping the result.
  - Twelve mutations: M2/M7/M11 survived first (error gate masked by kind gate;
    order unobservable at equal length; guard-vs-finder refusal conflated -
    closed with isolated fixtures and an independent finder unit test), M10
    survives by design (redundant-today sort, T13 M1 class). Eight guard
    checks, all probed; four were blind before shipping.
- **T15.5** `crates/supra_cohort`: deterministic tier estimation, admission,
  escalation - turn step 2 with zero LLM calls.
  - **A ladder, not a weighted sum.** Ordered rules over signal bands, composing
    by maximum so overlapping evidence resolves independent of rule order. E5 on
    user request or 2+ failures; E4 on sensitive areas or any active finding;
    E3 on wide blast or hot churn; E2 on contained blast, many anchors, active
    churn, or R3; E1 on any side effect or few anchors; E0 otherwise.
  - **Bands, not raw counts.** Blast/churn/anchor thresholds calibrated to the
    document's triggers, normalised at construction so the ladder stays stable
    while repositories grow. Failures saturate at 2, success resets; E5
    saturates, unreachable-quorum and blind-mismatch skip E1.
  - **`Admission` carries k, quorum, shards together** - built once from T6's
    arithmetic, read everywhere, so the numbers cannot drift apart.
  - Fourteen mutations, all caught; two survived first (M4/M5: deleted signals
    sharing a rule with covered neighbours - closed with minimal-evidence
    tests). Five guard checks, all probed; three were blind before shipping
    (directory-to-scan, presence-vs-count, single-arm sed range).
- **T15** `crates/supra_digest`: symbol index, dependency graph, churn, hybrid
  retrieval anchors - orientation with zero LLM calls.
  - **A real parser, not line patterns.** Seven compiled-in grammars (Rust,
    TypeScript, Python, JavaScript, Go, C, C++); anything else is
    `UnsupportedLanguage` by design. Error nodes yield nothing (guesses indexed
    as facts misdirect anchors); stale entries die with the breakage.
  - **Import edges, not call edges** - over-approximation is the safe direction
    for scrutiny. `crate::` rewritten at record time against the importer's
    directory; unindexed paths (external targets, deleted files) have empty
    radii, not singleton ones.
  - **Gists are deterministic strings** (`name (kind[ of parent], language):
    first line`), shortened at word boundaries, refused past 80 bytes. Ten
    pointers, ~300 tokens at 3 bytes/token, rendering in fused-rank order.
  - **Pool entries die with the retrieval** (explicit drain at every exit,
    including `?` exits); embedding trouble degrades to lexical, never fails.
  - Eight mutations, all caught; two survived first (M4: count-not-consequence,
    closed with sequenced retrievals around a deletion; M6: defence in depth,
    same class as T13's M1). Six guard checks, all probed; one blind before
    shipping (constant presence vs comparison).
- **T14** `crates/supra_prompt`: append-only ledger, four breakpoints, lossless
  eviction, generation rewrites, hash guard.
  - **Sealing is the ledger's act.** `append` takes an unsealed `Segment` and returns
    the assigned `SeqNo`; callers cannot reserve positions. The prefix hash covers
    `(SeqNo, ContentHash)` pairs - a rewrite re-seals every segment at new positions,
    so content alone would verify against the wrong generation.
  - **Eviction commits before the prefix forgets** (T10's rule, honoured here), with
    T13.5's disposition: tool turns keep thinking verbatim, prose turns drop it,
    keyed on `ToolUse` presence anywhere (`.any()`, not first position). Recall is
    byte-identical; missing is not corrupt.
  - **Rewrite at 92% while idle**, moving content never rewording; both seals kept
    so a reorder fails verification loudly. Eight mutations, all caught; two
    survived first (M4: no interleaved-region fixture; M6: order-sensitivity is not
    position-sensitivity). Seven guard checks, all probed; two were blind before
    shipping (two-line call spelling, name-vs-meaning narrowing).
- **T13.5** (research, no new crate): thinking-block preservation rules resolved from
  official Anthropic documentation, unblocking T14's eviction policy.
  - **Required:** within a tool-use turn, thinking blocks pass back complete and
    unmodified with their `tool_use` (400 otherwise); `signature` verifies provenance,
    `redacted_thinking` passes back unchanged; consecutive blocks keep generation order.
  - **Allowed:** outside tool use, prior turns' thinking may be omitted (silently
    accepted or auto-stripped; modifying is a 400, omitting is free).
  - **Decided for T14:** evict verbatim what the API requires (`tool_use` turns keep
    thinking + signature, `redacted_thinking` everywhere, order kept); drop only what
    the API declares omissible; never call `clear_thinking_20251015` (server-side
    clearing invalidates cache at the clearing point - eviction already reclaimed the
    space losslessly, so clearing would pay the break without buying anything back).
- **T13** `crates/supra_llm`: three providers behind one call shape, each with its own
  `CachePolicy`, one canonical serialiser, and a thinking budget frozen at startup.
  - **`Anthropic` is T6's table verbatim** (4 breakpoints, 1h/5m, 1024 minimum, 1024
    thinking floor). `OpenAI` carries one `prompt_cache_key`. `Google` assumes the 4096
    floor: padding wastes once, a miss bills every turn. Section 11 records Google as
    unverified; the policy encodes that honestly.
  - **The serialiser is the only sanctioned producer of `CanonicalJson`.** Sorted keys,
    no floats, duplicates refused (with `\u0041`-style escapes decoded for identity).
    The bytes on the wire are the bytes that were hashed.
  - **The thinking floor is checked in `Client::new`**, not at the first request -
    T7's fail-fast, placed where it lives. Zero disables thinking on every provider.
  - **The credential arrives per `send()`**, never as a field. Retries belong to the
    turn loop: only `RateLimited` carries a delay (provider's header, else 60 s, never
    zero); `Unauthorized` is never retried; malformed SSE is refused, not skipped.
  - Eight mutations, all caught; one survived first (M1: the sort is redundant today
    because the map is already a `BTreeMap` - evidence about the code, closed with two
    pinning tests). Six guard checks, all probed; two were blind before shipping
    (parameter vs field, value vs violation vocabulary).
- **T12.5** `crates/supra_guard`: seven anti-self-spawn layers, no off switch.
  - **Four questions, seven layers.** L1/L2 readiness (identity, key); L3/L4 this binary
    (name, inode); L5 the copy L4 cannot see (HMAC marker); L6 this agent (lineage);
    L7 this claim (no self-vote). Every layer runs on every judgement; the verdict
    carries all refusals.
  - **L4 compares inodes, never paths**, via `supra_ffi::sandbox::file_identity` - the
    `self_identity` T4 built for exactly this. A symlink test proves it: an innocent
    name passes L3 and fails L4.
  - **The marker is HMAC-SHA256 over `version:nonce`**, verified constant-time, key
    zeroized, entropy failure returned as `NoEntropy` rather than panicked. A rotation
    test stands in for "another process".
  - Eight mutations, all caught; three survived first (M3/M4: fixtures failing before
    the gate; M5: no honest-except-last fixture). Nine guard checks, all probed; three
    were blind before shipping (call-site vs name, doc comment vs call, region scope).
- **T12** `crates/supra_secrets`: OS keyring plus encrypted fallback, `Secret<T>`, and
  credential resolution in `supra_config`.
  - **The ladder is probed, not assumed.** `keyring` v4 reports `NoDefaultStore` in ~10 ms
    with no Secret Service provider, so `SecretManager::open` costs nothing and cannot hang
    startup. `get` searches keyring-then-file; `set` writes the primary only, so a credential
    lives in exactly one store.
  - **`Secret<T>` closes the three ordinary leaks**: `Debug` and `Display` print
    `[REDACTED]`, the value wipes on drop, and there is no `Clone`/`Eq`/`Serialize` to derive
    around it. `into_zeroizing` hands the value to a consumer inside a wiping guard.
  - **The vault is AES-256-GCM with a PBKDF2-HMAC-SHA256 key, 100 000 rounds** - measured
    ~74 ms vs ~447 ms for OWASP's 600 000, neither of which protects a weak passphrase. The
    threat model is a file that lands somewhere it should not, stated plainly.
  - **0600 before the first byte lands**, on the temp file and re-asserted after rename.
  - **The library never prompts.** An earlier `/dev/tty` prompt hung the test runner; the CLI
    registers its prompt via `set_passphrase_provider`.
  - **`Config::provider_secret`** resolves `api_key_env`/`api_key_keyring` to a
    `SecretString`, with `MissingCredential` carrying the source by name - never the value.
- **T11** `crates/supra_vector`: hybrid lexical and semantic retrieval over T10's file -
  FTS5/BM25, binary codes with an exact rerank, a bounded exact-vector cache, and rank fusion.
  - **The first stage's cost is bytes moved.** At 768 dimensions an exact vector is 3072 bytes
    and a code is 96. Measured on a 4-core i3-8100T, worst of 20: an exhaustive f32 scan takes
    24.584 ms and 307 MB resident at a hundred thousand entries, where the code scan takes
    4.037 ms and 9.6 MB. The exhaustive scan is not slow because of its loop - it runs at about
    12.5 GB/s, this host's single-core streaming limit - but because it reads 307 MB.
  - **The codes only choose candidates; the exact vectors order them.** One bit per dimension
    records a direction, not a magnitude, so it cannot separate the near-duplicates a code
    corpus is full of. Over a hundred queries on a corpus with cluster structure, a rerank width
    of 50 recovers the exhaustive top-10 in full, and 100 keeps doing so on every corpus whose
    top-10 similarity exceeds its mean by 0.29 or more.
  - **The binarisation threshold is frozen at first write.** A threshold that moves makes every
    code written before it answer a different question from every code written after, and the
    scan ranks them against each other without failing. A caller passing a different threshold
    is told rather than silently overridden, compared over encoded bytes rather than floats.
  - **A model change is reported, not scored.** Embeddings from two models share no space, so a
    cosine between them is a number with no meaning - and it would still rank. The caller
    decides between re-embedding and running lexical-only.
  - **Fusion is over ranks.** A cosine is bounded; `bm25()` is unbounded, negative, and scaled by
    corpus term statistics, so normalising them onto a shared range would make the fused ranking
    depend on corpus size. `SCALE / (K + rank)` summed as integers is reproducible bit-for-bit. A
    slot missing from a lane contributes nothing rather than a penalty: the lexical lane is
    always narrower, and scoring silence as negative would let it veto the wider one.
  - **`bm25()` is negative and better is more negative**, so ordering is ascending; `DESC` or an
    absolute value inverts the lane while still returning the requested number of results.
  - **The lexical query is tokenised, not passed through.** FTS5 gives meaning to quotes, stars,
    parentheses, colons, hyphens, carets and the bare words `AND`/`OR`/`NOT`/`NEAR`, all of which
    appear in a task description - so `fix the parser (see #12)` passed through is a syntax
    error. Terms are runs of alphanumerics and underscores, quoted and joined with `OR`.
  - **The text is indexed but not stored** (`content=''`, `contentless_delete=1`): the corpus is
    the source of truth for its own text, and `contentless_delete=1` is what lets an
    incrementally maintained index delete a row at all.
  - **Both resident tiers are updated only after the commit**, so a failed write cannot leave
    them describing a row that does not exist.
  - `RRF_SCALE` was 1,000,000 with a comment claiming rank distinctness to 1000. It collapses at
    941. The claim is now derived from `MAX_FUSION_DEPTH` and asserted at compile time.

- **T10** `crates/supra_store`: a second schema ledger, and two doors for the stages that own
  tables in the same file.
  - `user_version` is a single 32-bit slot and the core schema owns it, so every later owner
    records its own version in `schema_component` through `Store::migrate_component`. Each
    stage's DDL stays in the stage that owns its meaning while the file keeps one schema history.
    Both ledgers are forward-only, refuse a newer file, and carry each step's DDL and version
    bump in one transaction.
  - `CREATE VIRTUAL TABLE ... USING fts5` was **verified** to roll back with its transaction,
    shadow tables included, rather than assumed to: a half-created FTS5 table would leave a
    component at version 0 with its shadow tables present, failing every retry for ever on a name
    that already exists.
  - `Store::with_connection` and `Store::with_transaction` keep the mutex inside, so no caller
    can hold the connection past its closure or take the lock twice. `with_transaction` is
    generic over the caller's error type, so a downstream fault does not arrive as a storage
    fault.

### Fixed

- `scripts/check-invariants.sh`: three more defects, all found by probing the six new T12
  checks before they shipped - two of them new instances of recorded lessons.
  - `scan` blanks string literals, so a `/dev/tty` path inside a string was invisible to the
    prompt guard. Fixed with `scan_sql` for the literal spelling alongside `scan`.
  - The KDF guard searched for the name `test-kdf` and missed the actual weakening (widening
    the `cfg` gate, no new name anywhere). Rewritten as a property: production constant is
    `cfg(not(test))`-gated, exactly two definitions exist, no `cfg(any(test` touches it.
  - The 0600 guard searched the whole `save` function, where the second chmod (destination,
    after rename) satisfied it with the first (temp file, before write) deleted. Now scoped
    to `File::create..write_all`.
  - Also fixed in passing: a `grep -q` pattern starting with `->` parsed as a flag
    (`grep: invalid option -- '>'`), masked by `|| true` further down the pipeline.
  - A constraint check was satisfied by the module documentation that quotes the constraint, and
    then - once comments were stripped - by the migration's own `--` commentary inside the SQL
    string literal. Both passed on a schema with the constraint deleted.
  - **The same weakness existed in T10's constraint checks** and was found only because T11's
    were probed. T10's are now probed too; twenty-two probes cover both stages.
  - `production_lines | grep -q` reports failure under `pipefail`, because `grep -q` exits at its
    first match, closes the pipe, and the Python producer dies of `BrokenPipeError`. Every check
    written that way would have failed on a file that satisfied it - the opposite failure from a
    blind guard, and equally silent.

- **T10** `crates/supra_store`: SQLite in WAL mode, forward-only migrations, and the
  verbatim turn store invariant I4 rests on.
  - Bodies are stored as `BLOB` and **never re-rendered**. Storing a structure and rendering
    it back would make byte-identical recall depend on the renderer staying identical for the
    life of the store, which nobody can promise across versions.
  - Every row carries a digest of its own body; recall recomputes it and returns `Corrupt`
    with **no bytes** on a mismatch. Returning them with a warning would be worse than
    returning nothing - the model would carry on with content that is no longer what the
    conversation contained. Tests cover all 256 byte values, an empty body, a megabyte of
    non-repeating bytes, a tampered body, and a truncated one.
  - A turn has one body: evicting identical bytes twice is a no-op so a retry is safe, and
    evicting *different* bytes under the same id is refused.
  - Eviction takes an `IMMEDIATE` transaction. It reads then writes, and a deferred
    transaction that has read must *upgrade* - which SQLite refuses rather than deadlocking,
    and `busy_timeout` cannot help because the upgrade is unsafe to retry.
  - `synchronous = FULL` by default. In WAL, `NORMAL` can lose the last commits on power
    loss, and this store holds turns the prefix has already dropped, so a lost commit is a
    lost conversation. Affordable because eviction happens at a generation rewrite, not per
    turn. **T14 must commit the eviction before dropping the turn from the prefix** - no
    setting here makes the other order safe.
  - Migrations are forward only; a store written by a newer supra is refused rather than
    read with older code, which would misinterpret rather than fail.
  - Establishes the canonical-kind convention: **T6 owns `0x00`-`0x0F`, downstream crates
    take `0x10` upward, `0xF0`+ stays for tests**, enforced by a compile-time assertion
    because a collision makes two different values hash alike.
  - 45 tests plus a doc test. Ten mutations, all caught.

- `scripts/check-invariants.sh` gained nine T10 checks and a `scan_sql` helper: `scan` blanks
  string literals so a banned word in a message is not a hit, which made a check on embedded
  SQL blind. All nine probed against deliberate violations.

- **T9** `crates/supra_eventbus`: filtered publish/subscribe over the T6 event taxonomy.
  - **Publishing cannot block, by type.** `Bus::publish` is synchronous, returns no
    `Result`, and has nothing to await. The publisher is the turn loop; any shape that let
    a consumer's slowness reach it would make turn latency a function of the slowest
    subscriber. What gives instead is the subscriber's bounded, drop-oldest ring - blocking
    the publisher stalls the turn, dropping the newest discards what a woken consumer most
    needs, and an unbounded queue is the same bug with a longer fuse.
  - Loss is reported twice over and the two agree: `Delivery::missed_before` rides the next
    delivery a consumer wanted, `Subscription::missed_total` answers without waiting. An
    invariant test states the relation directly - received plus buffered plus not-yet-
    attached equals the lifetime total.
  - No async runtime. Waiting is the subscriber's business and uses a `Condvar`; publishing
    needs no runtime because it cannot wait. An adapter belongs to the first stage with an
    async consumer.
  - Events fan out as `Arc<Event>`, so cost does not scale with consumer count. Sequence
    numbers are global and come from an atomic, so concurrent publishers produce a dense
    sequence - a skipped number would be indistinguishable from a dropped event.
  - `TopicSet` is a bitset whose width is read from `Topic::ALL`, with a compile-time
    assertion that it still fits and an explicit match for bit positions. A twelfth topic
    fails to compile rather than becoming silently undeliverable, and reordering the enum
    cannot silently remap stored filters.
  - Dropping the bus closes every subscription, so a waiter learns the stream ended instead
    of retrying for ever; `Closed` takes precedence over `Timeout`. Every lock ignores
    poisoning, and a test panics inside a subscriber to show the others keep working.
  - 40 tests plus a doc test. Ten mutations, all caught.

- `scripts/check-invariants.sh` gained the T9 checks: `publish` may not be async, return a
  `Result`, or block; the crate may not gain an async runtime; an evicted delivery's gap
  count must be carried forward; and the bitset width must stay derived from `Topic::ALL`.
  All five probed against deliberate violations.

- **T8** `crates/supra_log`: structured diagnostics, and the one crate permitted to
  write to stderr.
  - **Redaction happens at the sink**, over the whole formatted line, after every call
    site has had its say. T7 removed every field that could hold a credential from the
    configuration schema and a test still found a leak, so the net goes at the last
    moment before bytes become durable. Two mechanisms, because each misses what the
    other catches: an **exact** field-name match (`api_key` is a secret, `api_key_env`
    is a signpost) and a prefixed value shape (`sk-`, `ghp_`, `AKIA`, `Bearer `, PEM).
  - What must survive is as load-bearing as what is caught. A generic entropy rule would
    eat supra's own diagnostic material - ULIDs, content hashes, cache keys - so there is
    none. A `ContentHash` surviving in both its forms, and `api_key_env`'s value
    surviving, are tests rather than intentions.
  - **One line, one write** on an `O_APPEND` descriptor, so two processes sharing a log
    produce whole lines rather than shredded ones - and so redaction sees a complete line
    exactly once, which is what makes it sound. Threaded and split-write tests cover both
    halves.
  - Size-bounded rotation with a fixed file count; an existing file's length counts
    toward the threshold at open, or a restart would reset the bound. The file is created
    `0600`, for the same reason T7 holds the user's configuration to `0600`.
  - stderr mirroring is suppressed by a **guard**, not a pair of setters: a `set(false)`
    whose `set(true)` is missed would silently discard every diagnostic for the rest of
    the session. A line written under the guard still reaches the file - suppression is
    about the terminal, not the record.
  - A failed write is counted and reported on the next line that succeeds. A gap in a log
    is only debuggable if the log says there is one.
  - `build` returns a subscriber without installing it, so the wiring is testable as many
    times as a test suite likes; exactly one test installs a global subscriber.
  - The crate deliberately does **not** depend on T7. Configuration loading is exactly
    when diagnostics are needed, and a logger that cannot start until configuration has
    loaded cannot report why configuration failed to load.
  - 56 tests plus a doc test. Twelve mutations: eleven caught, one recorded as
    platform-equivalent rather than papered over.

- `scripts/check-invariants.sh` gained the T8 checks: no diagnostic outside `sink.rs` may
  touch stderr, the redaction fast path must read `SHAPES` and `SECRET_FIELDS` rather than
  restate them, the field rule must stay an exact match, and the log file must stay
  `0600`. All five probed against deliberate violations.

- **T7** `crates/supra_config`: layered configuration - discovery, per-field
  precedence, provenance, and fail-fast validation.
  - Precedence is per **field**, not per file: `ConfigLayer` is all-`Option` and
    says only what its file said, `Config` is fully resolved. Without that split a
    project file setting `[cohort] limit` would erase the user's
    `[thinking] budget`. Every resolved setting also records **which layer supplied
    it**, so `/config` can answer the question a user actually has when a setting
    appears to be ignored.
  - Resolution cannot fail. Layers are validated individually, so combining them is
    a total function - which also means a precedence bug cannot hide behind an error
    path.
  - `Config` has no setter and no `&mut` accessor. That is what "the thinking budget
    is frozen per session" amounts to in practice: not a rule to remember but the
    absence of a way to break it.
  - **No configuration field can hold a credential.** Only `api_key_env` and
    `api_key_keyring` - the *name* of an environment variable or keyring entry -
    exist. `api_key` and `auth_token` are declared solely to be refused with the
    remedy, since `deny_unknown_fields` alone would say "unknown field" and leave the
    reader to guess. A value in `api_key_env` that is not shaped like a variable name
    is refused too: otherwise supra would look up a variable literally named `sk-...`,
    report a missing credential, and send the reader looking in the wrong place.
  - **The project layer is not trusted.** A repository is cloned from anywhere, so
    `.supra/config.toml` may not name a provider, an endpoint, or a credential
    source, and its permission mode may only make the session *stricter*. Under plain
    precedence `project` outranks `user`, so a cloned repository shipping
    `mode = "yolo"` would get it.
  - Reading is hardened in two different directions. The permission check runs on the
    **open handle** rather than the path, so the mode reported and the bytes read
    belong to the same file. The file-type check runs **before** the open, because
    opening a FIFO read-only blocks until a writer appears - a handle-based type check
    would already have hung the process.
  - Only the user layer is held to `0600`; a project file is normally committed and
    cannot be. Its safety comes from the schema instead.
  - Environment variables use a reserved `SUPRA_CONFIG_` prefix, and an unrecognised
    one is refused. A bare `SUPRA_` prefix could not be: `SUPRA_CXX`,
    `SUPRA_CPP_BUILD_DIR`, and `SUPRA_CMAKE_BUILD_TYPE` are real build variables, and
    refusing unknown `SUPRA_*` names would break a developer's own shell.
  - Every loader input is injectable, so the precedence rules are tested as
    arithmetic. A loader that reached for the real environment internally could not be
    exercised without mutating process-global state, and `set_var` is both `unsafe` in
    this edition and racy across parallel tests.
  - 82 tests plus a doc test. Twelve mutations against the load-bearing rules, all
    caught.

- `scripts/check-invariants.sh` gained the T7 checks: no mutation path on the
  resolved `Config`, no field that can hold a literal credential, `deny_unknown_fields`
  on every schema shape, and `describe_toml_error` as the single place that sees a
  parser error. The last is checked structurally rather than by grepping for
  `error.to_string()`, because a probe showed that pattern misses `e.to_string()`.

- `scripts/mutate.sh` wraps its Rust branch in `timeout`. A mutation can remove a
  guard against blocking rather than a guard against a wrong answer, and an unbounded
  hang stalls the harness instead of reporting a verdict.

- **T6** `crates/supra_types`: the contract layer, where the architecture's
  invariants stop being prose.
  - **I1** is the type: `Sealed<T>` has no `DerefMut`, no `as_mut`, and no
    `into_inner` - the last one because moving the value out would allow
    edit-and-reseal at the same sequence number, which is a rewrite wearing an
    append's clothes. Every reader that legitimately needs the contents needs only
    `&T`.
  - **I2** is a type error rather than a runtime refusal: the `Sealable` bound sits
    on `Sealed`'s type definition, so `Sealed<EphemeralBlock>` cannot be *written
    down*. The 202k-against-4k arithmetic that justifies the invariant is a test, so
    the reason for the type is executable.
  - Content hashing runs over a length-prefixed canonical encoding owned by this
    crate, deliberately **not** the T13 wire serialiser. Hashing the wire bytes
    would make every stored digest move whenever the wire format did, and I4's
    guarantee that a recalled turn is byte-identical would be measured against a
    moving target. No `serde_json` in the dependency graph at all.
  - Every validated type re-runs its constructor on deserialisation - `Sealed`,
    `Segment`, `MemoryIndexEntry`, `Lineage`, `Verdict`. Deriving `Deserialize`
    would be a hole straight through each invariant: a session file could
    reintroduce exactly the shapes the constructor rejects, and that content is
    about to be sealed into a prefix and paid for on every later turn.
  - No floats anywhere: `MicroUsd` is integer micro-dollars, `Confidence` is an
    enum, cache multipliers are integer percentages, and quorum is `(2k).div_ceil(3)`
    because `(0.67 * 3).ceil()` is 3 and would silently demand unanimity from a
    three-peer cohort. Every documented figure is reproduced as arithmetic: the
    tier table's quorum and byzantine columns at every k, the 15 req/min shard
    count landing on 6 for k=80, the $6.30 hundred-turn baseline, the I8 padding
    trade-off.
  - `Lineage` caps depth at 1 and refuses an id already in the chain, so a peer
    cannot spawn and a model cannot spawn itself. A cohort of 80 is siblings at
    equal depth with no peer in another's ancestry, which is the structural
    statement of "not an orchestrator".
  - The permission model keeps its two axes apart: `decide` consults `ToolClass`
    before `Mode`, and an exhaustive walk of class x invoker x mode x reversibility
    shows no mode - `yolo` included - can turn an authority refusal into permission.
  - 47 event variants across 11 topics, with the topic partition and a serde
    round trip both walked through an exhaustive sample list that fails to compile
    when a variant is added without one.
  - `admit` resolves a requested tier against the configurable peer limit by
    reducing the **tier** rather than truncating k, because a limit of 6 sits
    between E2's ceiling and E3's floor and truncating would yield a cohort in no
    tier. `Tier::containing(k) == Some(tier)` is tested across all 480 tier-limit
    combinations, which is what makes the table's gaps at 6 and 13-15 unreachable
    rather than merely noted.
  - 122 tests. Fifteen mutations against the load-bearing invariants, all caught.

- `scripts/check-invariants.sh`, wired into `just lint`: the CI half of "enforced by
  types and by CI, not by convention", covering what a type system cannot assert -
  an absence. Checks for a mutation path on `Sealed`, a `Sealable` impl for
  `EphemeralBlock`, ephemeral state reaching a segment, a `Mode` on the authority
  axis, a float in the quorum module, and `unsafe` outside `supra_ffi`.

- `scripts/mutate.sh` infers the crate from the mutated file's path, so the Rust
  branch works for every crate the workspace grows rather than only `supra_ffi`.

- **T5** `crates/supra_ffi`: safe Rust bindings to the three C++ libraries, and
  the only crate permitted to contain `unsafe`.
  - Bidirectional layout ratchet: `abi_sizes.rs` is the single source for every
    structure's size and alignment, asserted against C++ `sizeof` by a
    `static_assert` in the build script and against Rust `size_of`/`align_of`
    in `sys.rs`. A hand-written `extern` declaration drifting from its header
    fails to compile instead of silently reading wrong offsets, and both checks
    are compile-time, so they stay valid when cross-compiling.
  - Sentinels become types at the boundary: width `-1` becomes
    `Width::NonPrintable` (distinct from `Width::Zero`, because output
    sanitising must tell them apart), East Asian Ambiguous stays a caller
    parameter per the T2 finding, and every C++ `bool`/`int` protocol becomes a
    real `bool` or enum.
  - Ownership is designed in, not documented in: `Token` owns its 432-byte
    structure and borrows slices from `&self`; `Policy` owns its paths as
    `CString`s and materialises the borrowed-pointer raw form only per call;
    `Process` kills and reaps on drop. Raw policy construction goes through the
    ABI's own `allow`/`allow_port` constructors rather than writing fields, so
    the C side's normalisation rules cannot drift from a second implementation.
  - 47 Rust tests, including cross-library agreement (a styled string measures
    the same as its stripped form), chunk-boundary scanner resumption, sandbox
    execution end-to-end (exit codes, environment non-inheritance, drop-kills),
    and fail-closed refusals reached via `force_tier_for_testing`.
  - Five deliberate mutations via the Rust branch of `scripts/mutate.sh`; four
    caught, one (the layout ratchet) refused at build time by the
    `static_assert`, which is that invariant's designed enforcement point. One
    gap found and closed: nothing asserted `fixed_string` stops at the first
    NUL or preserves bytes >= 0x80 through the negative-`c_char` conversion.
  - `crates/supra_ffi/README.md` records the ratchet rationale, sentinel
    discipline, and the full mutation table.

- `scripts/mutate.sh` gained a Rust branch: for `.rs` targets it runs
  `cargo test -p supra_ffi --locked` and inspects the log to keep the T4
  discipline intact - one invocation both compiles and runs, so "could not
  compile" is reported BUILD_FAIL (not a verdict) and only a compiled-then-
  failed run counts as CAUGHT.

- **T4** `cpp/libsupra_sandbox`: OS-level process isolation behind one C ABI.
  - Linux backend is native rather than a bubblewrap wrapper, decided by
    measurement: applying a Landlock ruleset and then exec'ing `bwrap` fails with
    "Failed to make / slave: Operation not permitted", and still fails under a
    ruleset granting write access everywhere. The ordering supra needs - host
    applies policy, then execs - does not work with bwrap under any policy.
    `unshare(NEWUSER|NEWNET|NEWPID|NEWIPC|NEWUTS)` plus Landlock needs no mount
    operations at all, and gives per-port TCP policy that a network namespace
    cannot.
  - Tiered and reported, never assumed. The probe applies a real ruleset in a
    forked child and confirms a denial occurs, because a reported ABI version
    proves only that the LSM is compiled in and not that it is enabled in the
    boot-time LSM list. A filesystem policy the platform cannot enforce refuses
    to run.
  - Policy is a struct, never an argv string, and commands are argv vectors:
    building a command line from user-supplied paths is the injection surface
    T12.5 exists to remove. Zero-initialising yields the most restrictive setting.
  - Child setup failures are reported through a CLOEXEC pipe, so a caller learns
    which of nine steps failed rather than only that the child exited.
  - Verified escape attempts, all denied: reads and writes outside the allowlist,
    `../..` traversal, TCP connect, `chroot` then reaching out, grandchildren via
    nested `sh -c`, installing a wider ruleset, host process visibility, and host
    environment inheritance.
  - `supra_sandbox_self_identity` provides (device, inode) identity for T12.5
    guard layer L4, so a symlink or rename cannot masquerade. The residual gap - a
    *copied* binary has a different inode - is asserted rather than documented
    away.
  - Two bugs found by testing rather than reading: `spawn` blocked until the
    payload exited (measured 5004ms for a 5000ms sleep) because the PID-namespace
    intermediate never execs and so never closed its CLOEXEC copy of the report
    pipe, which also made `kill` untestable; and `readlink` truncated silently,
    where a partial path can resolve to a different file and would make guard
    layer L4 compare the wrong inode.
  - macOS and Windows refuse rather than running unconfined, which keeps T16.7's
    `auto` mode safe on platforms whose backend has not landed.

### Fixed

- **`STRICT` alone did not protect the turn id column** (T10, found before shipping). A
  probe appeared to show SQLite rejecting an integer in a `TEXT` column; a test contradicted
  it and the shell settled it - a STRICT `TEXT` column *accepts* an integer and converts it,
  and the probe's rejection had come from the `BLOB` column beside the id. A turn id written
  as `1` would have become the text `'1'`, parsed as nothing, and surfaced at recall. The
  invariants the code depends on are now `CHECK` constraints: a 26-character id, a 32-byte
  digest, `byte_len = length(body)`, a non-negative timestamp.
- **`total()` returns a REAL** (T10, found before shipping). Chosen over `sum()` because
  `sum()` of an empty table is `NULL`, and it silently traded that for a float - which failed
  to deserialise, and would have started losing whole bytes above 2^53.
  `coalesce(sum(...), 0)` gets both properties, and a test pins the types.
- **Two T10 mutations survived a full suite.** Eviction's `IMMEDIATE` transaction was
  untested because one process's connection mutex serialises writers; closed with two `Store`
  handles on one file racing behind a barrier. And `decode_digest`'s wrong-width path was
  unreachable through SQLite thanks to the `CHECK`; closed with a direct unit test, because
  the distinction it preserves - `Malformed` means the file was edited around SQLite,
  `Corrupt` means the bytes changed under a valid digest - points at different remedies.
- **Two T10 guard checks were blind when first written**, both in the way the method note
  predicts. One grepped for `SchemaTooNew` across the whole file and was satisfied by the
  mention in the test module; it now scans `migrate` itself. The other used `scan`, which
  blanks string literals - so a check for `total(byte_len)` could not see SQL living inside a
  literal; `scan_sql` keeps string contents for exactly that case.
- **The event bus under-reported loss** (T9, found before shipping). A delivery evicted from
  the front of the ring carries its own gap count, and discarding it made a consumer summing
  `missed_before` under-report while `missed_total` stayed right. Two sources of truth that
  disagree are worse than one that is approximate. The count is carried forward, and an
  invariant test states the relation rather than sampling it.
- **Two T9 tests looked like they covered a property while depending on something else** -
  the same shape as the T6 length-prefix test and the T8 fast path. `publish`-side pruning
  was checked through `subscriber_count()`, which prunes as it counts, so the check proved
  nothing; closed with a non-pruning accessor. And the wakeup test asserted only that a
  delivery arrived, when with no `notify_all` at all `wait_timeout_while` re-checks its
  predicate at timeout expiry, finds the event, and returns it *successfully* ten seconds
  later; closed by asserting the elapsed time is a fraction of the timeout.
- **A FIFO log target hung the process at startup** (T8). Opening a FIFO for *writing*
  blocks until a reader appears, and the sink opens before any UI exists to explain the
  stall. T7 already guarded the equivalent on its read path and T8 had not - the same bug
  class, missed on carry-over. An audit test hung until a pre-flight `stat` was added. Only
  FIFOs are refused, so `/dev/null` stays a legitimate target.
- **IPv6 loopback endpoints were rejected** (T7). The loopback exemption split the authority
  on `:`, which yields `[` for `[::1]:8080`, so an ordinary local proxy was refused with a
  message about sending credentials in clear. The authority is now parsed and loopback is
  decided by `IpAddr::is_loopback`, covering all of `127.0.0.0/8` and `::1`.
- **The first fix for that was itself a bypass** (T7). Unwrapping the brackets and ignoring
  the remainder made `http://[::1].evil.example` read as loopback - an attacker-controlled
  host served plaintext. Caught by its own adversarial test before shipping. After the
  closing bracket only a numeric port is permitted. Loopback is parsed rather than
  prefix-matched for the same reason: `starts_with("127.")` would accept
  `127.evil.example`.
- **Redaction echoed a short multibyte value in full** (T7). The guard was on `len()` -
  bytes - while the truncation took characters, so a two-character CJK value was six bytes,
  passed the guard, and was reproduced whole by the message whose purpose is not to
  reproduce it. Both now count characters.
- `write_to_file` had two identical `if`/`else` branches after a rotation attempt.
  Collapsed; the tolerated-failure reasoning moved into a comment.
- Two of the guard checks added with these fixes tested that a **name existed** rather than
  that it was **enforced**: deleting only the call site, or only the `if`, left the grep
  satisfied. A probe caught both. They now scan `open_append`'s body and the `if
  !port_is_well_formed` gate specifically, and all four detect their violations.
- **The redaction fast path silently let a whole class of credential through.** The
  pre-check that lets ordinary lines skip the matcher listed its own literals - `"sk-"`,
  `"gh"`, `"xox"` - and one was wrong: **`github_pat_` does not contain `gh`**, because
  the letters are g-i-t-h. Every GitHub personal access token passed straight through a
  matcher that would have caught it. The pre-check now reads the same tables the matcher
  does, so drift is impossible by construction, and a property test walks every entry.
- **A filter default could silence everything without failing.** `build` took the default
  level as a string, and almost any string parses as a *valid* directive because a bare
  word is read as a target name: `"inf"` becomes `inf=trace`. A typo would not error - it
  would enable trace for a target nothing logs to and silence the rest. The parameter is
  now a `LevelFilter`, which cannot be misspelled.
- **The dropped-line counter had no coverage.** The only test that touched it set it by
  hand, so a mutation deleting the increment survived. Closed with `/dev/full`, which
  answers every write with `ENOSPC` - a deterministic version of a full disk.
- **A pasted credential could leak through the error refusing it.**
  `ConfigError::Invalid` originally carried the `toml` crate's `Display` verbatim,
  which prints the offending **source line** with a caret under it. So the message
  explaining that supra never reads a literal credential from a file quoted that
  credential - into stderr, into the T8 log, and into the next bug report. Errors now
  report the location and the cause and never the content. One residual is named
  rather than glossed: a mistyped *scalar* still has its value in the parser's cause
  text, a surface that needs a value in a field of the wrong type.
- **The project layer could loosen the permission mode.** The tightening rule was
  applied as a pass *after* ordinary precedence, but ordinary precedence had already
  accepted the project's mode - and a pass that can only tighten cannot undo a
  loosening. The exhaustive test missed it because it used `Cli` as the operator
  layer, which outranks `Project`; the bug only appeared when the operator layer sat
  *below* the project. Non-operator-controlled layers are now excluded from the
  ordinary pass, and the test walks every operator-controlled source.
- `docs/ARCHITECTURE.md` section 4 was **missing the configurable peer limit
  entirely**, despite it being a locked requirement (1..=80, default 16). Its
  absence is how a reachable gap in the tier table went unnoticed: the limit can
  fall between a tier's floor and the tier below it, and nothing said what happens
  then. The section now specifies the limit, its default, and the resolution rule.
  E5's row also read `<=80` with no floor, leaving the tiers non-disjoint on paper;
  it now reads `33-80` with its derived quorum and byzantine columns filled in.
- `docs/ARCHITECTURE.md` section 6 stated "deny always winning" as a phrase with
  two possible readings. It now carries the decision and the reasoning: any deny
  beats every allow, precedence orders allows only, and a default is the absence of
  a rule rather than a deny. T16.7 inherits the catalogue of what `builtin` may
  deny, not the question of what deny means.
- `rustfmt.toml` claimed deterministic import ordering through
  `imports_granularity` and `group_imports`, which are nightly-only options: on
  the pinned stable toolchain they emitted a warning on every run and took no
  effect, so the documented guarantee did not exist. Removed; what stable
  actually provides (`reorder_imports`, which sorts within contiguous runs but
  never across blank lines) is kept, and the std/external/crate grouping is
  documented as an author-maintained convention rather than a formatter
  guarantee.
- `scripts/mutate.sh` replaces an inline mutation loop that **produced false
  results**: it ignored the build exit code, so a mutation rejected by `-Werror`
  left the previous correct binary in place, ctest passed, and it reported
  SURVIVED. Three mutations were recorded as test gaps having never been
  compiled. The script distinguishes CAUGHT, SURVIVED, and BUILD_FAIL, treating
  the last as "not a verdict".
- T4 test gaps found by that harness, both closed: the workspace write test
  overwrote a file **persisting between runs**, so truncation stood in for
  creation and no directory-only Landlock bit was exercised; and the fail-closed
  refusals were unreachable on a Landlock-capable kernel, leaving the protection
  for *older* kernels untested. `supra_sandbox_force_tier_for_testing` pins the
  tier downward to reach them.

- **T3** `cpp/libsupra_ansi`: escape sequence parsing, SGR state, style-safe
  truncation.
  - Resumable state machine over the DEC STD 070 / VT500 grammar, covering the
    cases a regex cannot: both OSC terminators (`ESC \` and `BEL`), colon
    sub-parameters (`ESC[4:3m`, `ESC[38:2::255:0:0m`), 8-bit C1 introducers,
    DCS/APC strings, and sequences split across chunk boundaries.
  - `supra_ansi_plan_truncate` returns a plan rather than a copy: prefix length
    plus a trailer, guaranteeing `cells <= max_cells`, no cut inside an escape
    sequence, no split grapheme cluster, and a terminal left in its default
    state. An unstyled line gets an empty trailer.
  - Positional C1 disambiguation. The C1 range `0x80..0x9F` is a subset of the
    UTF-8 continuation range, so U+6587 (`E6 96 87`, second byte = C1 SGA) and
    U+1F600 (three C1-range bytes) would be torn apart by a byte-wise test. The
    scanner carries the outstanding continuation count, so the distinction
    survives a chunk boundary landing mid-character.
  - Full SGR model: 8 attribute bits, 6 underline styles including the
    sub-parameter form, indexed and RGB colour in both the semicolon and colon
    encodings, underline colour, and OSC 8 hyperlink state. Folding and
    serialisation round-trip, which is what makes the truncation trailer sound.
  - Four CTest suites passing under RelWithDebInfo and ASan+UBSan, clean under
    clang-tidy: chunk-size invariance at every size from 1 up, truncation across
    12 inputs x 26 limits x 2 locales, and totality over all 256 single bytes.
  - Eight deliberate mutations introduced, all eight caught. One initially
    survived - the byte-wise C1 test - proving no test actually exercised the
    invariant; a split-boundary regression test now does.
  - `cpp/testing`: shared assertion helpers, extracted from
    `libsupra_width/tests` when libsupra_ansi became the second consumer.

- **T2** `cpp/libsupra_width`: terminal cell width and grapheme segmentation.
  - Flat C ABI: UTF-8 decoding, per-code-point width, UAX #29 grapheme cluster
    segmentation, string measurement, cluster-safe truncation, validation, and a
    startup glyph probe.
  - East Asian Ambiguous resolved from a caller-supplied locale flag rather than
    baked in, because the class is a property of the terminal rather than of the
    text. `supra_width_probe` lets the TUI measure its own glyph set and fall
    back to an ASCII tier.
  - Tables generated from vendored Unicode 17.0.0 extracts and committed, so the
    build needs neither Python nor network. The generator asserts that every
    table is sorted, non-overlapping, and coalesced.
  - Hangul LV/LVT derived arithmetically instead of tabulated, removing 798
    ranges (31% of the total); the generator verifies the derivation against the
    UCD on every run.
  - Malformed UTF-8 yields U+FFFD and advances exactly one byte, guaranteeing
    forward progress for every caller's scan loop on arbitrary bytes.
  - Five CTest suites, all passing under both RelWithDebInfo and ASan+UBSan:
    766 official UAX #29 conformance cases, exhaustive UTF-8 round-trip over all
    1 112 064 scalars, all 11 172 Hangul syllables, and truncation across
    7 inputs x 21 limits x 2 locales.
  - Conformance suite refuses to pass vacuously: fewer than 500 parsed cases
    exits 2, so a moved fixture cannot look like a green run.
  - Six deliberate mutations introduced and all six caught, including
    GB12/GB13 flag pairing, GB11 pictographic ZWJ, GB9c Indic conjuncts, and
    wide-cluster splitting during truncation.

- **T1** Workspace foundation.
  - Cargo workspace: resolver 3, edition 2024, MSRV 1.85, shared package
    metadata, dependency pinning policy, lint policy, and four build profiles.
  - Toolchain pinned to Rust 1.96.0 with `wasm32-wasip2` and `wasm32-wasip1`.
  - Quality gates: `rustfmt.toml`, `clippy.toml`, `deny.toml` (licence
    allowlist, advisory policy, banned crates, source allowlist).
  - CMake root establishing the shared C++20 contract for T2-T4: C++20,
    `-fno-exceptions` at the ABI boundary, warnings-as-errors, PIC static
    archives, optional sanitizers.
  - `justfile` as the single build entry point; recipes report out-of-scope
    stages instead of failing.
  - CI: lint, supply chain, C++ on three platforms, C++ under
    ASan+UBSan, WASM components, tests on three platforms.
  - Release and security workflows: cross-target build, checksums, signing
    hook, advisories, licence policy, CodeQL, secret scanning.
  - `docs/ARCHITECTURE.md`: the prefix-stability thesis, eight invariants,
    peer-consensus model, permission model, and the 37-stage map, with
    verified/derived/open status recorded per claim.

[Unreleased]: https://github.com/trubs/supra-harness/commits/main
