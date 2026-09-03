# supra-harness architecture

Status: T1-T4 complete. Stages T5 onward are unimplemented.

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
| E5 | <=80 | <=54 | <=26 | user-requested, or repeated-failure escalation |

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

Rule precedence, deny always winning: `session` > `cli` > `project` > `user` >
`builtin`.

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
| T11 | `supra_vector` | sqlite-vec, two-tier LRU, BM25 |
| T12 | `supra_secrets` | OS keyring plus encrypted fallback |
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
| vector retrieval p99, 10k vectors | < 5 ms |
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

**Open, and deliberately not guessed** (T13.5): whether prior-turn thinking
blocks must be resent and what omitting them does; the signature verification
rule for thinking blocks adjacent to `tool_use` in one turn. Both determine
whether `evict.rs` may drop thinking blocks from old turns. Until resolved,
T14 will be **conservative** - preserving thinking blocks on turns containing
`tool_use` - which means the thinking-perpetuity target is not yet claimable.

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
