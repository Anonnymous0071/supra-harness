# supra-harness

A terminal coding-agent harness built from scratch in Rust, C++20, and WASM.

**Status: pre-alpha.** T1 of 37 stages is complete: workspace, toolchain
pinning, quality gates, and the architecture contract. There is no runnable
binary yet.

---

## Why

Coding harnesses ship 1-10k tokens on every prompt. The usual response is to
compress the prompt or summarise history. Provider pricing says both are wrong:

| Token class | Effective price |
| ----------- | --------------- |
| cache read | `0.1x` base input |
| uncached input | `1.0x` |
| cache write | `1.25x` (5m) / `2.0x` (1h) |
| output, reasoning included | never cached, full price |

The read-to-uncached ratio is **10x**. The best realistic compression ratio is
2-3x. **Sending 10,000 cached tokens costs less than sending 1,000 uncached
ones** - and compressing a prefix below a model's minimum cacheable length
(512-4096 tokens) disables caching entirely, multiplying cost by 10.

Existing harnesses are expensive because **every turn is a cache miss**, not
because their prompts are large. The four usual causes are all documented as
prefix-breaking: volatile data in `system`, summarising compaction, lazy-loaded
MCP tools mutating `tools`, and unstable JSON key ordering.

So supra does the opposite of the usual advice:

- **No prompt compression.** Tool schemas ship in full and frozen.
- **No summarisation.** Old turns are evicted verbatim to SQLite with a ~15
  token index entry; `recall` returns them byte-identical. Auto-compaction that
  genuinely loses nothing, because nothing was thrown away.
- **Volatile data never enters the prefix.** Mode, cwd, branch, and context
  percentage render in the request suffix and are discarded at seal time, so
  changing permission mode costs zero tokens.

The full derivation, all eight invariants, and the honest list of what is
verified versus estimated: **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)**.

## Design commitments

**Economics on screen, not behind a command.** Context, cache hit rate, and
spend live in a permanent two-line meter. There is no `/cost`. Transparency you
have to ask for is never consulted when it matters - while spend is climbing.

**Peers, not an orchestrator.** No agent is privileged. Peers share a blackboard
and reach consensus at `ceil(2k/3)`, computed as a rational so k=3 needs 2 votes
rather than unanimity. Cohort size is a function of evidence: 1 peer for a
question, 3 for a multi-file edit, up to 80 when asked. Tier estimation is a
pure deterministic function with **zero LLM calls**.

**The environment carries the instruction.** Instead of prose in the system
prompt, `edit_file` fails if the file was not read this session, and `read_file`
truncates large files with a hint. Zero prompt tokens, fully reliable, and it
teaches exactly when it matters. The system prompt is under 400 tokens.

**Structural edits, not string matching.** Byte-range splices through
tree-sitter preserve formatting exactly, and a reparse gate rejects any edit
producing an `ERROR` or `MISSING` node, with atomic rollback. `rename_symbol`
costs ~20 output tokens where a multi-file text patch costs ~2500.

**No agent can spawn itself.** Seven layers, the strongest being absence: the
WASM linker exposes no spawn import, so an agent has no vocabulary for it. No
permission mode can relax this.

## Build

Requirements: Rust 1.96, CMake 3.24+, clang++ with C++20, git. Optional:
`bubblewrap` (Linux sandbox), `clang-tidy`, `cargo-deny`, `ninja`.

```sh
just doctor      # report toolchain status
just bootstrap   # install missing rustup targets and components
just ci          # what the pipeline runs
```

`just --list` shows everything. Recipes report when a stage's scope does not
exist yet rather than failing.

## Layout

```
Cargo.toml           workspace, dependency pinning, lint and profile policy
CMakeLists.txt       C++20 root: shared flag contract for T2-T4
cmake/               CMake helper functions
cpp/                 C++20 libraries: width (T2), ansi (T3), sandbox (T4)
crates/              Rust crates, T5 onward
agents/              WASM agent components (T20)
docs/ARCHITECTURE.md normative architecture contract
scripts/             build, packaging, and diagnostic scripts
justfile             single entry point for humans and CI
```

## Licence

MIT or Apache-2.0, at your option.
