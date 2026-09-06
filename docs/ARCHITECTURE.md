# supra-harness architecture

Status: T1-T14 complete. Stages T15 onward are unimplemented.

This document is normative. Where an implementation disagrees with an invariant
stated here, the implementation is wrong.

---

## 1. Thesis

> Token efficiency in a coding harness is a **prefix-stability problem**, not a
> prompt-size problem.

The arithmetic that forces this conclusion, from provider documentation:

| Token class | Effective price |
| ----------- | --------------- |
| cache read | `0.1x` base input |
| uncached input | `1.0x` |
| cache write, 5 min TTL | `1.25x` |
| cache write, 1 hour TTL | `2.0x` |
| output (including reasoning) | never cached, full price |

The read-to-uncached ratio is **10x**. The best achievable prompt compression
ratio is 2-3x. Sending 10,000 cached tokens therefore costs less than sending
1,000 uncached tokens.

Two consequences follow, and both are counter-intuitive enough to state plainly:

1. **supra does not compress prompts.** Compression is at best a 3x lever
   against a 10x pricing gradient, and it is actively harmful near the
   per-model minimum cacheable length (512-4096 tokens): compressing a prefix
   below that threshold disables caching entirely and multiplies cost by 10.
2. **supra does not summarise history.** Provider documentation lists
   summarisation, compaction, and truncation as cache-invalidating. A harness
   that summarises at 80% context is destroying its prefix and paying `1.25x`
   to rebuild it.

Existing harnesses that send 1-10k tokens per prompt are expensive because
**every turn is a cache miss**, not because the prompt is large. The four
mechanisms that cause those misses, each verified as prefix-breaking:

- injecting volatile data (timestamp, cwd, git branch, context %, todo state)
  into `system`
- auto-compaction and summarisation
- lazy-loading MCP tools, which mutates `tools` and invalidates `tools`,
  `system`, and `messages`
- non-deterministic JSON key ordering

---

## 2. Invariants

These are enforced by types and by CI, not by convention. Each names the stage
that owns enforcement.

### I1 - Append-only prompt ledger
The prompt is never rewritten, only appended to. Enforced by the type system:
`Sealed<Segment>` (T6) has no `&mut` accessor, and `PromptLedger` (T14) exposes
only `append(Sealed<Segment>)`. Every segment carries a sequence number and a
content hash.

### I2 - Volatile data never enters the prefix
`system` holds identity and output contract only. `tools` is a frozen,
sorted-key manifest. Everything volatile is rendered as an `EphemeralBlock`
(T6, T14) in the current request suffix and **discarded at seal time** - it
never enters the BP4 delta, so it never becomes perpetual.

This distinction is load-bearing. A 40-token state block appended perpetually
costs O(n^2) across a session: 100 turns accumulates ~202k re-read tokens. As an
ephemeral block it costs 40 tokens once per turn and nothing thereafter.

### I3 - Tools frozen for the session lifetime
Every tool is registered at startup; MCP servers are probed once and their
manifests cached to SQLite. Disabling a tool uses `allowed_tools` or
`tool_choice`, never removal. Dynamic capability sits behind a **static gateway
schema**, and newly discovered tools are described by appending - append does
not break a prefix, so dynamic discovery becomes cache-compatible.

### I4 - Compaction is append-only and lossless
Old turns are **not summarised**. They are evicted verbatim to SQLite and
indexed by embedding; the prefix retains only a ~15 token index entry per turn
(topic, range, one-line gist). The `recall` tool returns the original
**byte-identical**.

Two properties follow: compaction genuinely loses no context, because the
original is intact; and the index only grows, so it stays append-only.

When the window is genuinely near its limit, the prefix is rewritten **once**
into a new generation, with a 1-hour TTL (`2.0x` write) because it will be read
hundreds of times, executed **while idle** - after a turn completes, before the
user types - so the cost rides on human thinking time. The compaction threshold
is 92-95%, not 80%: every avoided compaction is one full rewrite not paid for.

### I5 - The four-breakpoint budget is spent deliberately

| Breakpoint | Boundary | TTL | Lifetime |
| ---------- | -------- | --- | -------- |
| BP1 | end of `tools` | 1h | frozen for session |
| BP2 | end of `system` | 1h | frozen for session |
| BP3 | end of memory index | 1h | advances only on new generation |
| BP4 | end of turn n-1 | 5m | rolling |

Ordering 1h -> 1h -> 1h -> 5m satisfies the documented rule that longer TTLs
must precede shorter ones. Because the cache-read lookback window is 20 blocks
and consecutive `tool_use` / `tool_result` runs each count as one position,
**tool calls are rendered consecutively** and never interleaved with prose.
That is a rendering rule with a measurable cost, not a stylistic one.

### I6 - Cache choreography for peer cohorts
A new cache entry becomes available only **after the first response byte**, and
traffic above 15 requests/minute may migrate to another machine. Therefore: send
**one warm-up request**, await first byte, **then** fan out; shard cohorts to
<=15/minute with a distinct `prompt_cache_key` per shard. All peers use a
**byte-identical** prefix and differ only in the task suffix.

Without this, 80 parallel requests are 80 cache writes - a 12.5x error.

### I7 - Canonical serialisation and cache-break attribution
The serialiser emits stably sorted keys and canonical float formatting.
Provider documentation names unstable `tool_use` key ordering as a cache
breaker; that bug class is removed at the type level rather than tested for.

Every turn recomputes the prefix hash locally and compares. An unexpected change
emits `Event::CacheBreak` with the **causing diff**, surfaced in the TUI meter.
An invisible cost leak becomes a debuggable defect.

### I8 - Minimum-length awareness, per model
Minimum cacheable prompt lengths (512 / 1024 / 2048 / 4096 depending on model)
live in `CachePolicy` (T13). If a static prefix falls below its model's
threshold, caching **fails silently with no error**. The response is to **pad
the prefix up to the threshold**: growing it is far cheaper than not caching it.

---

## 3. Language boundaries

| Language | Scope | Reason |
| -------- | ----- | ------ |
| Rust | host: event loop, prompt ledger, storage, providers, TUI, tools | memory safety in a long-running process; single static binary |
| C++20 | terminal cell width, ANSI parsing, OS sandbox | table-driven hot paths on the render budget; platform sandbox APIs are C |
| WASM | agent components (wasmtime) | capability isolation by absence: an agent has no import through which to spawn |

The C++ surface is a flat, `noexcept` C ABI. `-fno-exceptions` is not an
optimisation: an exception unwinding into Rust is undefined behaviour, so the
machinery is removed outright.

All `unsafe` is confined to T5 `supra_ffi`. `unsafe_code = "warn"` is set
workspace-wide and re-allowed only there.

---

## 4. Peer model - not an orchestrator

No agent is privileged. There is no lead, manager, or coordinator agent.

- Peers communicate through a **shared blackboard** (in-memory, persisted to
  SQLite).
- Consensus is **quorum voting** at `ceil(2k/3)` - computed as a rational, not
  as `0.67 * k`. Float arithmetic gives an off-by-one at k=3 (`0.67*3 = 2.01`,
  ceil 3), which would demand unanimity.
- Byzantine tolerance is `floor((k-1)/3)`. It only becomes meaningful at k>=4;
  below that, the guarantee comes from **deterministic gates** (compile, test,
  introspector, LSP), not from voting.
- Proposer and validator roles attach **per claim and rotate**. A proposer's
  vote does not count toward its own claim's quorum (T12.5 L7).

### Elastic cohort sizing
Cohort size is a function of evidence, never a constant. Tier estimation is a
**pure deterministic function** with **zero LLM calls** - `supra_cohort` (T15.5)
declares no dependency on `supra_llm`, and a negative test enforces it. Signals
come from the digest (blast radius, churn, anchor count), the tool registry
(requested reversibility class), findings, and past task profiles.

| Tier | k | Quorum | Byzantine | Trigger |
| ---- | - | ------ | --------- | ------- |
| E0 | 1 | blind re-derivation | - | Q&A, single-file read, blast radius 0 |
| E1 | 2 | 2 | 0 | single-file edit, blast radius <=2 |
| E2 | 3-5 | 2-4 | 0-1 | multi-file, blast radius <=10 |
| E3 | 7-12 | 5-8 | 2-3 | cross-module, high churn |
| E4 | 16-32 | 11-22 | 5-10 | auth, crypto, migrations, active findings |
| E5 | 33-80 | 22-54 | 10-26 | user-requested, or repeated-failure escalation |

The ranges are **not contiguous**: cohort sizes 6 and 13-15 belong to no tier.
That is a consequence of the tiers being discrete escalation steps rather than a
partition of 1..80, and it is safe only because selection runs **tier to k** -
evidence scores into a tier, and the tier fixes the range.

### The peer limit
Cohort size has two ceilings. `PEER_CEILING` is 80 and no configuration raises
it. Beneath it sits a **user-configurable limit**, any value in 1..=80,
defaulting to 16. The limit caps scrutiny; it is not a target, so a task scoring
E1 still fields two peers under a limit of 80.

A limit can fall between a tier's floor and the tier below it - a limit of 6 sits
above E2's ceiling of 5 and below E3's floor of 7. Resolution **reduces the
tier**, it does not truncate k:

```
admit(requested, limit) = (min(requested, largest_tier_whose_floor_fits(limit)),
                           min(that_tier_ceiling, limit))
```

So a limit of 6 turns an E3 task into E2 at k=5, and a limit of 14 turns an E4
task into E3 at k=12. Truncating instead - E3 at k=6 - would produce a cohort
belonging to no tier, which makes T30's tier-accuracy metric meaningless and
reports a level of scrutiny nothing was given. Reducing the tier is also the more
useful thing to tell a user: "your limit caps this task at E2" is actionable,
"you got 6 peers" is not.

`admit` is total for any non-zero limit, and `Tier::containing(k) ==
Some(tier)` holds for every result - which is what makes the gaps unreachable
rather than merely undocumented. A test walks every tier against every limit
from 1 to 80.

At k=1, the second pass receives the claim **without the reasoning that
produced it** and re-derives independently. This is **self-consistency, not
independent verification** - the same model can be wrong the same way twice. The
real guarantee at E0 comes from the deterministic gates running in parallel.
The second pass reuses a byte-identical prefix, so it is a cache read at
`0.1x`: elasticity and cache discipline reinforce each other.

### Output asymmetry without hierarchy
Cache discounts input only; output is always full price and dominates at large
k. Therefore a proposer writes fully, while **validators emit a structured
verdict of <=60 tokens** (`vote`, `confidence`, `reason`, `evidence_ref`),
enforced by schema rather than by instruction.

---

## 5. Instruction efficiency

The second half of "the model understands the task without thousands of
tokens": stop explaining, and make the environment carry the instruction.

| Prose instruction (~10 tokens, unreliable) | Structural constraint (0 prompt tokens, reliable) |
| ------------------------------------------ | ------------------------------------------------- |
| "always read a file before editing" | `edit_file` **fails** with a structured error if the file was not read this session |
| "prefer grep over reading whole files" | `read_file` on a large file returns a truncated view plus a hint |
| "verify with tests" | the introspector runs automatically post-edit; findings are appended |
| "do not touch anything outside the workspace" | the sandbox refuses; the error explains the boundary |

Result: the system prompt is **<=400 tokens** (identity plus output contract),
and instruction cost is paid only when relevant. The workflow is not described;
it is the only path the tool surface permits.

**Repo digest** (T15) supplies orientation with zero LLM calls: a tree-sitter
symbol index, dependency graph, and git churn, maintained incrementally by a
file watcher. A local hybrid BM25 + vector retrieval selects ~10 anchors, or
~300 tokens of precise pointers, appended in the suffix.

That retrieval is T11. Two lanes, because they fail differently: the semantic
lane finds a function that *does* what was asked without sharing a word with the
question, and the lexical lane finds an identifier the question names outright.
Fusing them is a union of two competences, not an average of two opinions - so
fusion is over ranks (`SCALE / (K + rank)`, summed as integers), never over the
lanes' own scores, which are not comparable and whose scales move with the corpus.

The semantic lane scans one binary code per entry - one bit per dimension, 96
bytes at 768 dimensions against 3072 for the vector - and then orders the hundred
nearest by their **exact** vectors. The codes only choose candidates: one bit per
dimension records a direction, not a magnitude, so it cannot separate the
near-duplicates a code corpus is full of. Measured, that returns what an
exhaustive scan would, at 9.6 MB resident per hundred thousand entries instead of
307 MB. The binarisation threshold and the embedding model's identity are
**frozen** at first write: a threshold that moves makes every code written before
it answer a different question from every code written after, and the scan would
rank them against each other without failing.

---

## 6. Permission model

Two axes are routinely conflated. They are separate here.

| Axis | Question | Enforced by | User-relaxable |
| ---- | -------- | ----------- | -------------- |
| `ToolClass` (T12.5) | *who* may invoke: agent, host, or user | absence of the WASM capability | **never** |
| Permission (T16.7) | does this need **user consent** | host-side gate at invocation | yes - that is what modes are for |

`yolo` relaxes the second axis only. Guard layers L1-L7 have no off switch, the
sandbox stays active, and the AST reparse gate still runs. Disabling the sandbox
is a separate `--sandbox off` flag with its own confirmation and a persistent
status-line warning.

### Reversibility, not "danger"
"Danger level" cannot be computed; reversibility can. Classification runs on the
**resolved effect**, never the tool name: `shell_run("cargo test")` is R0,
`shell_run("rm -rf node_modules")` is R3.

| Class | Definition |
| ----- | ---------- |
| R0 | no side effect |
| R1 | automatically recoverable (git-tracked and clean, or journal snapshot) |
| R2 | recoverable with intervention |
| R3 | irreversible: `rm` outside the index, force-push, network POST, `DROP TABLE` |

**Verifiability damps risk.** Because `replace_node` (T15.7) passes a reparse
gate with atomic rollback and preserves formatting, it is *provably* safer than
a blind string-matching `edit_file`, and the permission engine rates it one
class lower on the same file. Verified structure earns greater trust, and that
is measurable rather than felt.

| Mode | R0 | R1 | R2 | R3 |
| ---- | -- | -- | -- | -- |
| plan | run | **refuse** | refuse | refuse |
| ask | run | prompt | prompt | prompt |
| auto (default) | run | run | run | **prompt** |
| yolo | run | run | run | run |

`auto` is the default only because T16.6 `supra_journal` exists. Without an undo
stack, "auto" would be a hope rather than an engineering decision.

Rule precedence: `session` > `cli` > `project` > `user` > `builtin`, with **deny
always winning**. That is meant literally: *any* deny beats *every* allow
regardless of source, so precedence orders allows only. A `builtin` deny survives
a `session` allow.

Three reasons this is the reading rather than the softer one, where deny wins
only among rules of equal precedence:

- It matches the shape the design already uses. Guard layers L1-L7 have no off
  switch, and disabling the sandbox is not a rule at all but a separate
  `--sandbox off` flag with its own confirmation and a persistent status-line
  warning. Escape hatches here are explicit and ceremonial.
- The softer reading inverts the trust boundary. A `session` allow is something a
  slash command sets mid-conversation, and a prohibition a slash command can lift
  is not a prohibition.
- Unconditional explicit deny is the standard evaluation rule in security policy
  engines, so an operator's intuition transfers.

The obligation this places on rule authors: a deny is absolute, so `Deny` is the
wrong tool for "usually not". A default is expressed as **no rule at all**, which
falls through to the reversibility gate and asks. `builtin` therefore emits `Deny`
only for effects that must never be permitted under any mode by any user; T16.7
owns that catalogue.

**Mode changes cost zero tokens.** The mode is an `EphemeralBlock`, so
`Shift+Tab` fifty times in a session produces zero cache writes.
`tools_array_never_mutates_on_mode_change` is a hard CI failure - a regression
there destroys the economics.

### Thinking budget is frozen per session
`budget_tokens` is rendered into the prompt, so changing it invalidates cache
breakpoints. It is therefore read once at startup (`--think`, or `[thinking]
budget`) and **frozen**. There is no `/think` command. A different budget means
a new session.

This asymmetry is deliberate and visible in the UI: permission mode is free to
change, thinking budget is not.

---

## 7. Economics surfaces in the UI, not behind a command

`cost`, `cache`, and `context` have no slash commands. Transparency that must be
requested is opt-in, and opt-in transparency is never consulted at the moment it
matters - when spend is climbing.

Three quantities with opposite valence get different shapes:

| Quantity | Nature | Rising means | Shape |
| -------- | ------ | ------------ | ----- |
| context | bounded gauge | **bad** | fill bar, warming colour |
| cache | health indicator | **good** | sparkline trend |
| cost | unbounded accumulator | unavoidable | figure plus live delta |

A sparkline shows something a percentage cannot: the dip where a cache break
occurred. That is exactly the debugging affordance I7 promises.

Live cost during a turn is an **estimate** from local token counting, because
usage fields are final only after the stream ends. It carries a tilde (`+$0.014~`)
which disappears on reconciliation. A divergence above 10% emits
`Event::UsageDrift`: the token counter needs calibration, and that is worth
knowing.

Never shed at any width: context %, cache %, session spend, and the cache-break
marker. An invisible cost leak is the failure this design exists to prevent.

---

## 8. Stage map

37 stages. Ordering is strict: a stage may depend only on earlier stages. New
features are inserted as new stages at the correct dependency point; no
backtracking.

### Layer A - platform floor
| Stage | Crate / directory | Delivers |
| ----- | ----------------- | -------- |
| T1 | (root) | workspace, pinning, quality gates, CMake root, CI, this document |
| T2 | `cpp/libsupra_width` | cell width, grapheme segmentation, EAW + emoji tables |
| T3 | `cpp/libsupra_ansi` | escape parser/serialiser, SGR-safe truncation |
| T4 | `cpp/libsupra_sandbox` | bubblewrap+landlock, sandbox-exec, AppContainer |
| T5 | `supra_ffi` | safe RAII bindings; the only crate allowed `unsafe` |

### Layer B - contracts and infrastructure
| Stage | Crate | Delivers |
| ----- | ----- | -------- |
| T6 | `supra_types` | `Sealed<Segment>`, `EphemeralBlock`, `Event`, lineage, `ToolClass`, `Reversibility` |
| T7 | `supra_config` | precedence, 0600 enforcement, fail-fast |
| T8 | `supra_log` | structured logs, rotation, redaction, stderr under TUI |
| T9 | `supra_eventbus` | filtered pub/sub, explicit backpressure |
| T10 | `supra_store` | SQLite WAL, versioned migrations |
| T11 | `supra_vector` | FTS5/BM25, binary codes with exact rerank, two-tier LRU, rank fusion |
| T12 | `supra_secrets` | OS keyring plus encrypted fallback, `Secret<T>`, credential resolution |
| T12.5 | `supra_guard` | seven anti-self-spawn layers |

### Layer C - efficiency engine
| Stage | Crate | Delivers |
| ----- | ----- | -------- |
| T13 | `supra_llm` | three providers, `CachePolicy`, canonical serialisation, frozen thinking budget |
| T13.5 | (research) | resolve thinking-block preservation rules; blocks T14 |
| T14 | `supra_prompt` | append-only ledger, four breakpoints, lossless eviction, generations, hash guard |
| T15 | `supra_digest` | symbol index, dependency graph, churn, hybrid retrieval |
| T15.5 | `supra_cohort` | deterministic tier estimation, rational quorum, admission, sharding |
| T15.7 | `supra_ast` | byte-range splice, reparse gate, outline, query, rename |

### Layer D - capability
| Stage | Crate | Delivers |
| ----- | ----- | -------- |
| T16 | `supra_sandbox` | Rust policy layer over the C++ sandbox |
| T16.5 | `supra_shell` | persistent PTY sessions, deterministic output shaping |
| T16.6 | `supra_journal` | write-ahead snapshots, atomic undo |
| T16.7 | `supra_permission` | reversibility classification, four modes, batched prompts |
| T17 | `supra_tool` | registry, robust invocation, instruction-carrying preconditions |
| T18 | `supra_mcp` | stdio + HTTP clients behind a static gateway |
| T19 | `supra_skill` | skill loading, dependency resolution, hot reload |
| T20 | `supra_plugin` | wasmtime host, WIT ABI, verified import set |

### Layer E - peer intelligence
| Stage | Crate | Delivers |
| ----- | ----- | -------- |
| T21 | `supra_blackboard` | claims, votes, dynamic quorum, choreography, rotating roles |
| T22 | `supra_introspector` | static, dynamic, and cross-agent bug detection |
| T23 | `supra_core` | the turn loop |

### Layer F - editor integration and session
| Stage | Crate | Delivers |
| ----- | ----- | -------- |
| T24 | `supra_lsp` | five language servers, crash recovery |
| T25 | `supra_dap` | three debug adapters |
| T26 | `supra_session` | persistence, resume, branch, export |
| T27 | `supra_hook` | eight lifecycle events, prefix-safe by type |
| T28 | `supra_telemetry` | anonymous, opt-in |
| T28.5 | `supra_theme` | semantic tokens, glyph width probing, responsive banner |
| T28.7 | `supra_command` | command registry, palette |

### Layer G - interface and release
| Stage | Crate | Delivers |
| ----- | ----- | -------- |
| T29 | `supra_tui` | viewport, meter, status line, panels, spinner |
| T30 | `supra_cli`, `supra_update`, `supra_eval` | binary, updater, economy gate |

### Explicitly out of scope
No autonomous project mode, no nested sub-agents, no cloud sandbox, no ACP
mode, no hosted backend. No vendored terminal or agent framework: the TUI is
built here.

---

## 9. Turn loop (T23)

```
 1  digest.retrieve(task)                -> ~300 token anchors      (0 LLM calls)
 2  cohort.signals -> score -> tier      -> k, quorum, shards       (0 LLM calls)
 3  prompt.assemble()                    -> BP1..BP4, byte-stable prefix
 4  blackboard.publish(task, k)
 5  per shard: warm up -> first byte -> fan out   (non-blocking, JoinSet)
 6  evaluate quorum incrementally per vote
      quorum reached          -> abort remaining in-flight
      quorum unreachable      -> escalate immediately, do not await timeout
      E0 blind verify differs -> escalate E0 -> E2
 7  deterministic gates in parallel: compile, affected tests, introspector, LSP
 8  execute the winning claim (permission gate + sandbox, ULID tiebreak)
 9  append findings; a confirmed finding escalates
10  stream to the TUI (diff render, per vote)
11  self-critique (one low-temperature call, cached, appended)
12  when idle and usage >= 92%: rewrite one generation at 1h TTL
13  persist snapshot, cache_stats, cohort_stats, task_profiles
```

No `join_all` appears on the turn path. Three properties are test-enforced: a
turn never waits on a late peer while quorum remains reachable; unreachable
quorum (`yes + pending < needed`) escalates immediately rather than after the
30s timeout; and every vote is an event, so the TUI never blocks.

---

## 10. Budgets

Cost model, derived arithmetically from the verified multipliers. **Estimates,
not measurements** - T30 `supra_eval` validates them.

100-turn session, 5.4k prefix, history growing ~300 tokens/turn, input $3/MTok:

| | cache-unaware baseline | supra |
| --- | --- | --- |
| input per turn | ~21k uncached | 5.4k read + ~0.9k write delta + 0.2k uncached |
| total input cost | $6.30 | $1.03 |

Per-round cost by tier, including output:

| Tier | k | total |
| ---- | - | ----- |
| E0 | 1 | $0.012 |
| E2 | 3 | $0.016 |
| E3 | 12 | $0.046 |
| E4 | 32 | $0.115 |
| E5 | 80 (6 shards) | $0.399 |
| naive, no cache | 80 | $5.52 |

Performance budgets:

| Metric | Budget |
| ------ | ------ |
| cold start | < 200 ms |
| per-turn overhead, no LLM | < 50 ms |
| render frame | < 16 ms |
| meter render | < 2 ms |
| vector retrieval p99, 10k vectors | < 5 ms (measured 1.0 ms; 4.3 ms at 100k) |
| digest, 5k files | < 10 s |
| incremental digest update | < 100 ms |
| cohort spawn, k=80 | < 1 s |
| stripped binary | < 30 MB |
| idle RSS | < 50 MB |
| peak RSS, k=80 | < 500 MB |

---

## 11. Verification status

**Verified** from official provider documentation: all cache pricing
multipliers; prefix order `tools -> system -> messages`; the four-breakpoint
limit; the 20-block lookback; per-model minimum cacheable lengths;
summarisation, compaction, and truncation as cache breakers; TTL ordering
rules; first-response-byte availability; the 15 requests/minute machine
migration risk; tool definitions as part of the prefix; reasoning tokens billed
as output; `budget_tokens` rendered into the prompt.

**Derived, not measured**: every currency figure in section 10. All require
`supra_eval` validation before being stated as results.

**Resolved** (T13.5, from official Anthropic documentation — thinking, tool-use,
API reference, and context-editing pages):

- **Required:** within a tool-use turn, thinking blocks must be passed back complete
  and unmodified, alongside the `tool_use` block they accompanied. Modified blocks
  are rejected with a 400 error. The `signature` field verifies the block was
  generated by Claude; `redacted_thinking` blocks (safety-redacted, opaque and
  encrypted) must be passed back unchanged the same way.
- **Within the latest assistant message**, consecutive `thinking` blocks must match
  the generation order exactly: no rearranging, editing, or partial dropping —
  including `redacted_thinking` blocks.
- **Recommended:** across turns, pass everything back. The API automatically filters,
  keeps what the model needs, and bills only shown blocks.
- **Allowed:** outside tool use, omit prior turns' thinking. Omitting is silently
  accepted (or auto-stripped on last-turn-only models); modifying is a 400.
- **Per-model preservation:** keep-all (Opus 4.5+, Sonnet 4.6+, Fable/Mythos) bills
  retained thinking as input; last-turn-only (earlier, all Haiku) strips older
  blocks automatically. No code changes needed either way.
- **Clearing invalidates cache** at the clearing point (`clear_thinking_20251015`
  strategy, `keep` = last N `thinking_turns` or `"all"`). Keeping maximises cache
  hits; clearing reclaims window. This is the same tradeoff as I4's eviction, one
  level down: dropping content breaks the prefix it occupied.

**Decided for T14** (`evict.rs`): eviction preserves verbatim everything the API
requires, and drops only what the API declares omissible:

1. Turns containing `tool_use` keep their thinking blocks verbatim (signature
   included) — required, and a 400 otherwise.
2. Turns without `tool_use` may drop thinking blocks — allowed, silently accepted.
3. `redacted_thinking` blocks are preserved wherever they appear — same rule as
   thinking, opaque either way.
4. Within an evicted turn, consecutive thinking blocks keep generation order —
   the API checks the latest message's sequence, and an evicted turn resent via
   `recall` is a message again.
5. T14 does **not** call `clear_thinking_20251015`: server-side clearing
   invalidates cache at the clearing point, which is exactly the prefix break I7
   exists to attribute. Eviction already reclaims the space losslessly (verbatim
   to SQLite, ~15-token index in the prefix); asking the provider to also clear
   would pay the cache break without buying back anything eviction has not
   already reclaimed.

The thinking-perpetuity target is now claimable with the above scope: thinking
survives verbatim wherever the API requires it, and is dropped only where the
API declares dropping free.

**Unverified**: cache behaviour on Google beyond implicit caching with a
2048-4096 token minimum; that provider's TTL and prefix rules are undocumented,
so its `CachePolicy` is conservative and measured through `total_cached_tokens`.

**Verified in T2, and worth recording because it contradicts an assumption the
TUI design would otherwise rest on**: the Block Elements range is *not*
uniformly East Asian Ambiguous. U+2588 FULL BLOCK and U+2593 DARK SHADE are
Ambiguous, but U+2590..U+2591 - including LIGHT SHADE, the natural "empty"
counterpart in a gauge - are **Neutral**. The obvious pairing `█`/`░` therefore
mixes width classes: under a CJK locale the filled cells double while the empty
ones do not, and the bar silently changes length. Neither glyph is wrong; the
pairing is. T28.5 must probe each gauge glyph individually rather than treating
"block elements" as one class, and T29 must not assume the gauge has a fixed
cell cost.

**Verified in T3, and binding on every component that scans terminal bytes**: the
8-bit C1 control range `0x80..0x9F` is a **subset** of the UTF-8 continuation
range `0x80..0xBF`. U+6587 encodes as `E6 96 87`, whose second byte is the C1
code for START OF GUARDED AREA; U+1F600 encodes as `F0 9F 98 80`, containing
three C1-range bytes including `0x9B`, which is CSI. Any byte-wise test for C1
membership tears such characters apart and then invents a spurious escape
sequence that swallows the text after it.

The disambiguation must be **positional**: no byte in `0x80..0xBF` is a valid
UTF-8 lead, so such a byte is a C1 control exactly when it falls on a scalar
boundary. This binds T16.5, whose output shaping scans untrusted subprocess
bytes, and T29, whose renderer scans its own output - neither may reimplement
the check; both route through `libsupra_ansi`.

The mutation-testing corollary is worth stating separately, because it changed
how confidence is established in this project: the implementation was correct,
the suite passed, and a mutation that reverted this rule **survived** - meaning
nothing was actually exercising the invariant. A passing suite is not evidence
until something has tried to break it. Stages from here on treat mutation
survival as a test defect, not a curiosity.

**Verified in T4, and it changed the plan**: bubblewrap cannot be used the way
supra needs. Applying a Landlock ruleset and then exec'ing `bwrap` fails with
"Failed to make / slave: Operation not permitted", and still fails under a
maximally permissive ruleset that grants write access everywhere - so the cause is
bwrap's mount setup, not policy tightness. `no_new_privs` alone does not break it.
The reverse ordering works but inverts the trust boundary: the confined program
would apply its own confinement. The Linux backend is therefore
`unshare(NEWUSER|NEWNET|NEWPID|NEWIPC|NEWUTS)` plus Landlock with no mount
operations, which also yields per-port TCP policy that a network namespace cannot
express. T16 wraps this rather than shelling out to `bwrap`.

**Also verified in T4**: `landlock_add_rule` rejects directory-only access bits
applied to a regular file (`EINVAL`), but accepts file bits on a directory. That
asymmetry means every rule must be masked to its target's file type, and the
return value must be checked - a rejected rule is an *absent* rule, which widens
the sandbox rather than narrowing it. Probed directly: with correct masking,
`add_rule` succeeds for directories, regular files, device nodes, `/proc`, `/sys`,
and 20 000 consecutive rules, so its failure path is reachable only through a
second simultaneous defect.

**Method correction from T4, which supersedes how earlier stages reported
mutation results**: a mutation harness must verify that the mutation *compiled*.
An inline loop used during T4 ignored the build exit code, so a mutation rejected
by `-Werror` left the previous correct binary in place, the suite passed, and it
was reported as SURVIVED. Three mutations were recorded as test gaps having never
been built. `scripts/mutate.sh` now distinguishes CAUGHT, SURVIVED, and
BUILD_FAIL, and BUILD_FAIL is explicitly not a verdict.

Two consequences worth carrying forward. First, a test can pass because of
*leftover state* rather than correct behaviour: T4's workspace write test
overwrote a file persisting between runs, so truncation stood in for creation and
no directory-only permission was ever exercised. Suites must clean their own
fixtures. Second, a fail-closed refusal that protects weaker platforms is
unreachable on a strong one, so it goes untested exactly where it matters least
and is trusted where it matters most; T4 added a testing-only tier override to
reach those branches, and later stages with capability tiers should expect to need
the same.

**Stated limitations that will not be hidden in the implementation**: syntactic
rename without LSP can be wrong under shadowing or overloading, so results are
flagged `semantic: false`; the env-marker guard layer can be stripped by a
command that deliberately clears the environment, and the process-tree budget
catches the consequence rather than the intent; interactive-prompt detection is
heuristic and will produce false positives, which is why its action is to ask
the user rather than to kill silently.

**T4 boundary, stated so it is not over-trusted**: the sandbox is not a syscall
filter (no seccomp-bpf), `rlimit` is scheduling pressure rather than cgroup
accounting, a kernel bug defeats both mechanisms, and descriptors inherited across
`exec` remain usable. T16 and T16.5 must close descriptors they do not intend to
pass.

**Verified in T5, and a warning about documented guarantees**: `rustfmt.toml`
set `imports_granularity` and `group_imports`, which are nightly-only options.
On the pinned stable toolchain they emitted a warning on every run and took no
effect - the config documented a deterministic import order that did not exist,
and nothing failed until a diff was actually inspected. The same failure shape
applies to any tool configuration read from a channel other than the one that
runs: a guarantee stated in a file is not a guarantee the tool enforces. What
stable provides (`reorder_imports`, which sorts contiguous runs but never across
blank lines) is now what the config claims, and the std/external/crate grouping
is recorded as author-maintained convention, checked in review rather than by
rustfmt.

Two T5 FFI facts are binding on every later stage. First, `crates/supra_ffi` is
the only crate permitted to contain `unsafe`; anything above it that reaches for
`extern` is wrong and must either extend this crate's surface or justify a
second confined crate deliberately. Second, the layout ratchet is enforced at
compile time from both directions, and mutation M1 confirmed a drifted constant
is refused by the build script's `static_assert` - so a future contributor
"fixing" a layout mismatch by loosening an assertion is removing the only
mechanism standing between the FFI and silently wrong answers.

**Clarified in T6, because section 4's tier table has gaps**: cohort sizes 6 and
13-15 belong to no tier, and that is now safe rather than merely noted. The gap
is reachable through the configurable peer limit - a limit of 6 sits between E2's
ceiling and E3's floor - so `admit` resolves it by **reducing the tier** rather
than truncating k, and `Tier::containing(k) == Some(tier)` is a tested property of
every result across all 480 tier-limit combinations. `Tier::containing` still
reports `None` for 6 and 13-15, which is the correct answer now that nothing
produces them.

Two related corrections to this document came out of that work. The peer limit
itself was **missing from section 4 entirely** despite being a locked requirement,
which is how the reachable gap went unnoticed; it is now specified with its
default and its resolution rule. And E5's row read `<=80` with no floor, leaving
the tiers non-disjoint on paper; it now reads `33-80` with the derived quorum and
byzantine columns filled in.

**Decided in T6**: "deny always winning" in the rule-precedence sentence is
literal, and section 6 now carries the reasoning rather than the phrase alone. Any
deny beats every allow; precedence orders allows only; a default is the absence of
a rule, not a deny. T16.7 inherits the catalogue of what `builtin` may deny, not
the question of what deny means.

**Method note from T6, extending the T4 correction**: a mutation-harness bug is
one failure mode; a *test* that appears to cover an invariant while depending on
something else is another. T6's length-prefix test compared two two-field values
and passed with the length prefix removed, because the differing field tags
already separated them. The prefix only matters when the same tag repeats and the
payload contains that tag byte - a reachable case, since block text is arbitrary
model output. The lesson generalises: when a mutation survives, the first
question is not "which test is missing" but "what is the test I already have
actually depending on".

**Three method notes from the post-T8 audit**, each a class rather than an
incident.

*A bug found once must be looked for everywhere.* T7 established that an `open`
which can block needs a pre-flight `stat`, and T8 opened a log file without one -
so a FIFO target hung startup before any UI existed to explain it. Fixing a class
in one crate is not fixing the class. Both are now guarded, both are probed, and
the guard covers every future crate that opens a path.

*A fix is a change, and changes need adversarial tests.* The first correction for
the IPv6 loopback bug was itself a bypass: unwrapping `[::1]` and ignoring what
followed made `http://[::1].evil.example` read as loopback. It was caught by the
test written alongside it, before it shipped. A security check that decides
"is this the safe case" must be tested with inputs designed to look like the safe
case and not be it.

*Guards must test enforcement, not vocabulary.* Two checks added with these fixes
grepped for an identifier that still existed after the mutation deleted the call
site that used it. Both reported success on broken code. A check that a name
appears somewhere in a file is not a check that the name does anything.

The same applies to the checks in `scripts/check-invariants.sh`. Its first
version truncated each file at the first `#[cfg(test)]` marker, leaving every
line below the test module unscanned; a fourteen-case probe found it missed
seven violations out of eight while reporting success. A guard with a blind spot
is worse than no guard, because it certifies. Any future structural check must
be probed against deliberate violations before it is trusted.

**Method note from T11, on measurement itself.** Two measurements in this stage
produced reportable-looking numbers that were about the harness rather than the
subject, and the same mistake in both cases was condemning an approach on the
strength of a bad implementation of it.

*A layout is not an approach.* The first exact-scan measurement reported 11.2 ms
against a 5 ms budget and would have ruled out exhaustive search. It used one
float accumulator, and float addition is not associative, so LLVM had to keep a
single dependency chain; four accumulators cut it to 3.05 ms. The nested
`Vec<Vec<f32>>` layout, which the diagnosis blamed first, made no difference at
all.

*A degenerate fixture indicts every method at once, and looks like a result.* The
first recall measurement reported 0.544 for the design that shipped - and 0.055
for an HNSW index, which is the tell, because no graph index is that bad. The
corpus generator had normalised each centroid and then added per-component noise
nine times the size of the centroid's own components, and drawn its queries from
a differently seeded mixture. That is noise with a faint direction, not a
clustered corpus: every pairwise similarity collapses onto one value, so nothing
is retrievable and no method can be blamed for failing to retrieve it.

The general rule: **a fixture must state the property the measurement depends
on, and the measurement must assert it before reporting anything.** T11 exposes
`CorpusSignal` for exactly that, and its integration test asserts the corpus is
separable before it asserts recall. Where a number is impossible rather than
merely bad, the harness is the first suspect, not the subject.

*Two more instances of "enforcement, not vocabulary".* T11's schema checks were
satisfied first by the module documentation quoting the constraint they were
about, then - once comments were stripped - by the migration's own `--`
commentary inside the SQL string literal. Both passed on a schema with the
constraint deleted. The same weakness existed in T10's constraint checks and was
found only because T11's were probed; T10's are now probed too. Twenty-two
probes cover both stages.

*And one in the harness.* `production_lines | grep -q` reports failure under
`pipefail`: `grep -q` exits at its first match, closes the pipe, and the producer
dies of `BrokenPipeError`. Every check written that way fails on a file that
satisfies it - the opposite failure from a blind guard, and equally silent. A
helper that searches shipped code must collect the output before searching it.

**Method note from T12, on what "probed" means.** Six new guard checks landed with
this stage; probing them caught three blind spots before they shipped, and two of
the three are new instances of already-recorded lessons - which is itself the lesson.

*`scan` blanks string literals, so a literal is invisible to it.* The prompt guard
searched for `/dev/tty` with `scan` and missed a probe reading exactly that path:
the path lived inside a string, which `scan` blanks by design. The fix is `scan_sql`
for the literal spelling alongside `scan` for the identifier spelling - and the
reason it matters is that a doc example mentioning `/dev/tty` must *not* trip the
guard, so the two spellings cannot share one scanner.

*A property check beats a name check.* The KDF guard first searched for the string
`test-kdf` and missed the actual weakening, which was widening the `cfg` gate on the
existing constant - no new name anywhere. The rewritten guard asserts the property:
the production constant is `cfg(not(test))`-gated, exactly two definitions exist, and
no `cfg(any(test` gate touches the constant. A probe performing the exact weakening
now fails.

*The 0600 guard needed the segment, not the function.* `save` contains two
`set_permissions` calls - temp file before the write, destination after the rename -
so a whole-function ordering check passed with the first chmod deleted: the second
satisfied it. The guard extracts only `File::create..write_all`, the region where
ordering matters. A check that searches a wider region than its invariant is a check
with room for the violation to hide beside the evidence.

**Decided in T12**: 100 000 PBKDF2 rounds, not OWASP's 600 000. OWASP's floor assumes
a login server deriving one key per authentication; this store derives one key per
process. Measured release builds: 100 000 rounds cost ~74 ms, 600 000 cost ~447 ms,
and neither protects a weak passphrase - the threat model is a vault file that lands
somewhere it should not, with the passphrase in an environment variable on the same
class of machine. What would change the number is a memory-hard KDF, which prices
parallel guessing hardware out in a way iteration count cannot; none is pinned in the
workspace, and pulling one in for a fallback store is a T30 dependency decision.

**Decided in T12**: the library never prompts. An earlier version read the passphrase
from `/dev/tty` when no source existed; that hung the test runner, whose stdin was a
terminal with nobody behind it. A library that blocks on input without an explicit
opt-in hangs every non-interactive caller behind it. The CLI registers its prompt
through `set_passphrase_provider`; the library resolves memory, environment, provider.

**Method note from T12.5, on fixtures that fail before the gate.** Two mutations
survived (M3/M4) because the truncation test's hostile cases failed on shape before
reaching the gate under test: the version-2 case carried a two-char tag, the nonce
cases a short tag. A fixture that fails before the gate cannot test the gate - the
same class of mistake as T11's degenerate corpus, one level down. The fix is the same
too: mint a well-formed marker and break exactly one field, so the gate under test is
the only thing standing. A third survival (M5) is the dual: a suite holding only
fully-honest and fully-dishonest fixtures cannot see an early-accept on a *valid*
layer. The fix is a fixture honest everywhere except the last layer, demanding
exactly `[7]`.

**Method note from T13, on a redundant line that must stay.** The canonicaliser's
`keys.sort()` changes nothing: `serde_json::Map` without `preserve_order` is a
`BTreeMap`, so iteration is already sorted. Deleting it passes the suite - correctly.
The line stays because the guarantee must not depend on a transitive feature flag no
member selects: enabling `preserve_order` anywhere would switch the map to insertion
order, and the canonicaliser would emit whatever the parser saw. A redundant sort is
cheap; a feature-dependent guarantee is not one. Two tests pin it: output bytes, and
the map's own ordering. The general rule: **a passing suite is not evidence until
something has tried to break it - and a mutation that changes nothing is evidence
about the code, not a gap in the suite.** M1's survival taught what the line is for;
removing the line would un-teach it.

**Decided in T11**: the stage map named `sqlite-vec`, and the implementation does
not use it. The decision rests on measurement rather than preference:
`sqlite-vec` v0.1.x is pre-v1 with breaking changes expected and documents no
approximate index, so its KNN is exhaustive - the same algorithm, in C, behind an
unstable API and a new build dependency. Measured, the code scan already meets
the budget with 9.6 MB resident where an exhaustive f32 scan needs 307 MB, and
its inner loop is at this host's memory bandwidth limit, so there is nothing left
for a C kernel to win. An embedded HNSW was measured too and rejected on cost
rather than accuracy: 67 MB of graph for ten thousand entries against 1 MB of
codes, and 115 seconds to build a hundred thousand. What would reopen the
question is a corpus past a few hundred thousand entries; the largest repository
measured on this machine has 77,250 symbols, which is 7.4 MB of codes.

**Decided in T11**: a second schema ledger. `user_version` is a single 32-bit
slot and T10's core schema owns it, so every later owner of tables in that file -
T11's index, T16.6's journal - records its own version in `schema_component`
through `Store::migrate_component`. Each stage's DDL stays in the stage that owns
its meaning; the file keeps one schema history. Both ledgers are forward-only,
refuse a newer file, and carry each step's DDL and version bump in one
transaction. `CREATE VIRTUAL TABLE ... USING fts5` was checked to roll back with
its transaction, shadow tables included, rather than assumed to.
