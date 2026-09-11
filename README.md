# supra-harness

A terminal coding-agent harness built from scratch in Rust, C++20, and WASM.
One binary, `supra`. No prompt compression, no history summarisation — the
prefix stays byte-stable so the provider's cache does the saving.

**Status: T30 complete.** The 37-stage build sequence is closed: 36 crates,
one `supra` binary (`run`, `eval`, `update`, `config show`), signed releases
via `release.yml`. `supra run` assembles the full startup path today and
reports mode, cohort, session dir, and the admission plan.

---

## The problem every harness has right now

A real bill from September 2026, one session, nine consecutive turns:

| Prompt tokens | Completion | Spend/turn |
| --- | --- | --- |
| 706,767 → 713,592 (growing ~800/turn) | 140–985 | ~$1.78–1.80 |
| 45,688 (fresh session) | 7 | ~$0.11–0.15 |

Read it carefully: the operator pays ~700k input tokens to receive under 1k
output tokens, every turn, and the prompt grows monotonically because history
is appended raw. A fresh session drops to 45k — proof the 700k is accumulated
context, not the task. At $1.80/turn a 100-turn session costs ~$180.

Four root causes, all industry-wide:

1. **Unstable prefix.** Tools, system prompt, and history are re-serialised
   every turn with different key order or freshly rendered prose, so the
   provider's prefix cache misses and 700k tokens are billed at full price.
2. **History as string append.** Every turn pastes the raw transcript. It
   grows ~700 tokens/turn forever and is never compacted.
3. **Repo dumps, not digests.** Whole files ship per prompt instead of
   ~300-token anchors recalled byte-identical on demand.
4. **Invisible leakage.** Cost and cache live in a web dashboard or behind a
   command nobody runs mid-session. The operator learns at invoice time.

## What supra brings

**Prefix-stability, not compression.** The read-to-uncached price ratio is
10x; the best realistic compression is 2–3x. Sending 10,000 cached tokens
costs less than 1,000 uncached ones — and compressing a prefix below a
model's minimum cacheable length (512–4096 tokens) disables caching entirely
and multiplies cost by 10. So supra never compresses and never summarises:

- Tools ship in full and frozen; history evicts verbatim to SQLite with a
  ~15-token index and recalls byte-identical; volatile data (mode, cwd,
  branch) renders in the request suffix and is discarded at seal time, so
  changing permission mode costs zero tokens. Eight prefix invariants,
  enforced by types and by CI (`scripts/check-invariants.sh`).
- Derived result for a 100-turn session at $3/MTok: $6.30 baseline → $1.03
  supra. Currency figures are **estimates, not measurements** — `supra eval`
  validates them, and the gate fails the build if they regress.

**Peers, not an orchestrator.** No agent is privileged. Peers share a
blackboard and agree at `ceil(2k/3)` quorum computed as an integer rational
(k=3 needs 2 votes, not unanimity). Cohort size follows evidence: 1 peer for
a question, 3 for a multi-file edit, up to 80 when asked. Tier estimation is
a pure deterministic function with zero LLM calls, and admission under a
limit only ever reduces the tier — never leaves a gap.

**Economics on screen, not behind a command.** Context %, cache %, session
spend (`+$0.014~`, tilde while estimated), the cache-break marker, and the
permission mode are five segments that never shed at any terminal width.
There is no `/cost`. Transparency you have to ask for is never consulted
while spend is climbing — and when the prefix breaks, the marker appears
that turn, not on the invoice.

**The environment carries the instruction.** `edit_file` fails if the file
was not read this session; `read_file` truncates large files with a hint.
Zero prompt tokens, fully reliable. The system prompt is under 400 tokens.

**Structural edits, not string matching.** Byte-range splices through
tree-sitter preserve formatting exactly; a reparse gate rejects any edit
producing an `ERROR`/`MISSING` node with atomic rollback.

**No agent can spawn itself.** Seven layers, the strongest being absence:
the WASM linker exposes no spawn import, so an agent has no vocabulary for
it. No permission mode relaxes this.

**Trust is explicit.** `--ignore-project-config` and `--sandbox off` each
require their own `--yes`; `update apply` refuses without a verified
minisign signature (fetch, verify, then apply, in that order); secrets live
in the OS keyring with an encrypted-file fallback, never in plaintext.

The full derivation, all invariants, and the honest list of verified versus
estimated: **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)**. Per-crate
decisions and mutation tables: each `crates/supra_*/README.md`.

---

## Install

Requirements: Rust 1.86+, CMake 3.24+, clang++ with C++20, git. Optional:
`bubblewrap` (Linux sandbox), `clang-tidy`, `cargo-deny`, `ninja`, `just`.

### Option A — one line (release binary)

```sh
curl -fsSL "https://raw.githubusercontent.com/Anonnymous0071/supra-harness/vX.Y.Z/scripts/install.sh" | SUPRA_VERSION=vX.Y.Z SUPRA_PUBKEY='<published minisign public key>' bash
```

Installs the selected `supra` release for your platform into `~/.local/bin`
(override with `PREFIX=`), verifies its SHA-256 checksum and required Minisign
signature before touching the destination, and refuses when `minisign` or the
independently published `SUPRA_PUBKEY` is unavailable. Use the tag-pinned
installer URL shown in that release's notes; do not pipe the mutable `main`
branch into a shell. Needs `curl`, `minisign`, and either `sha256sum` (Linux)
or `shasum` (stock macOS).

### Option B — from source (developers)

```sh
git clone https://github.com/Anonnymous0071/supra-harness.git
cd supra-harness
just doctor      # report toolchain status
just bootstrap   # install missing rustup targets and components
just ci          # what the pipeline runs: lint, tests, build
cargo install --locked --path crates/supra_cli
```

`just --list` shows everything. Every cargo invocation passes `--locked`:
`Cargo.lock` is the reproducibility guarantee.

### Verify it works

```sh
supra --version        # supra 0.1.0
supra                  # run: mode, cohort, session dir, admission plan
supra config show      # resolved config and where each value came from
supra eval             # offline economy shape-check (always runs, no network)
supra eval --live      # live probe; skips explicitly without credentials
supra update check     # names the verifier for the artefact
```

---

## Layout

```
Cargo.toml           workspace, dependency pinning, lint and profile policy
CMakeLists.txt       C++20 root: shared flag contract for T2-T4
cmake/               CMake helper functions
crates/supra_ffi/native/ C++20 libraries: width, ansi, and sandbox
crates/              Rust crates: 36 members, T5 onward (supra_cli is the binary)
agents/              WASM agent components (T20)
docs/ARCHITECTURE.md normative architecture contract
scripts/             build, packaging, install, and diagnostic scripts
justfile             single entry point for humans and CI
```

## Licence

MIT or Apache-2.0, at your option.
