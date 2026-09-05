# supra_guard

Seven anti-self-spawn guard layers. **T12.5** of the stage sequence: one model must not
spawn itself - not as a child process, not as a copy of this binary, not as a vote on its
own claim.

## Modules

| Module | Owns |
|---|---|
| `identity` | who this process is: canonical path plus (device, inode), established once |
| `marker` | the HMAC-SHA256 environment marker a copied binary cannot shed |
| `layers` | all seven layers and the verdict that composes them |
| `error` | one refusal per layer, each naming its layer in the message |

## What this stage is for

> A proposer's vote does not count toward its own claim's quorum - and no agent spawns
> anything, ever.

The structural half lives in T6 (`Lineage`: depth 1, no cycles, revalidated on load).
This crate is the physical half - proving a command about to run is not this harness -
plus the vote rule. Four different questions (this binary? this copy? this agent? this
claim?), which is why one layer cannot substitute for another.

## The layers

| Layer | Refuses | Mechanism |
|---|---|---|
| L1 | no identity established | `is_established` |
| L2 | no marker key generated | `has_key` |
| L3 | the command names this binary | argv[0], its final component, its resolved name |
| L4 | the command *is* this binary | (device, inode) via `supra_ffi` |
| L5 | the marker does not authenticate | HMAC-SHA256 over `version:nonce`, constant-time |
| L6 | the lineage would cycle or nest | `Lineage::child` |
| L7 | a proposer voting its own claim | voter id against proposer id |

L1/L2 are readiness, not judgement: they refuse because the guard cannot yet tell, not
because the spawn is wrong. Every layer runs on every judgement even after one refuses -
"refused by L3, L4, and L5" tells the operator what was attempted, where "refused by L3"
only says what happened.

There is no off switch. `yolo` relaxes consent (T16.7); it does not touch authority.

## Decisions with measurements or probes behind them

**L4 compares inodes, never paths.** A path comparison passes a symlink with an innocent
name and fails a hardlink with a guilty one - both wrong. A symlink test proves the
distinction: `innocent-name -> test binary` passes L3 (the name is innocent) and fails L4
(the file is this binary).

**L4 does not cover a copy, so L5 exists.** A *copied* binary has a different inode and
passes L4 while still being this harness. The marker closes exactly that gap: the host
sets `SUPRA_SPAWN` to `version:nonce:tag` at startup under a per-process 32-byte key, and
a copy started any other way has no marker or one it cannot have minted. A rotation test
stands in for "another process" and proves a marker minted under one key fails under the
next.

**The marker is HMAC, not a comparison.** `verify_slice` is constant-time; a byte-wise
early exit would let a local process time the match byte by byte. Verification
recomputes nothing - the tag helper is called exactly once outside its definition, when
issuing - so there is no shadow verification path for a `==` fast path to hide in.

**The key is zeroized, the failures are returned.** A plain `[u8; 32]` leaves both key
generations in freed heap. `expect` on getrandom or the HMAC constructor turns a kernel
state into an abort; `NoEntropy` carries it as a refusal with a different remedy from
`NoIdentity`, because re-establishing identity will not help when the kernel reports no
entropy.

**A cleared environment fails closed.** `env -i` strips the marker, and that refusal lands
as L5 `Absent` - the documented residual, caught by the process-tree budget (T16) rather
than the intent. Stated in SECURITY.md rather than fixed here, because no environment
mechanism survives its own clearing.

## Mutation results

Eight mutations, all caught. Three survived first, each for a recorded reason.

| Mutation | Verdict |
|---|---|
| M1 L7 self-vote disabled (`if false`) | CAUGHT |
| M2 L4 same-file refusal dropped | CAUGHT |
| M3 unknown marker version accepted | CAUGHT *(survived first)* |
| M4 malformed nonce accepted | CAUGHT *(survived first)* |
| M5 L6 valid lineage early-accepts | CAUGHT *(survived first)* |
| M6 L5 marker check removed | CAUGHT |
| M7 L3 name check removed via clean-command early-accept | CAUGHT |
| M8 verify under a zero key | CAUGHT |

**M3/M4** survived because the truncation test's only hostile cases failed on shape
before reaching the gate under test: the version-2 case had a two-char tag, the nonce
cases a short tag. A fixture that fails before the gate cannot test the gate - the same
class of mistake as T11's degenerate corpus. Closed with tests that mint a well-formed
marker and break exactly one field.

**M5** survived because the suite held only fully-honest and fully-dishonest fixtures:
an early-accept on a *valid* layer is invisible to both. Closed with a
valid-everywhere-except-L7 fixture that demands exactly `[7]`.

## Guard checks

Nine structural checks, each probed. Three were blind before shipping - all three new
instances of recorded lessons:

- The L4 check grepped for the *name* `is_own_file` anywhere in the file, and passed
  with the call site replaced: vocabulary, not enforcement. Now asserts the call at the
  judgement site plus the `file_identity` definition behind it.
- The L5 check grepped for `verify_slice` anywhere, and passed with the call replaced
  while the doc comment still named it. Now asserts the `mac.verify_slice` call site.
- The 0600-shaped mistake repeated: the chmod check searched the whole `save`
  equivalent, where a second chmod satisfied it. Same fix - scope to the region where
  ordering matters.

## Obligations left to later stages

- **T16.5** calls `judge` before spawning anything, and **T20** derives the agent's WASM
  import set from `ToolClass` so a peer has no name to call - the absence behind the
  refusal. Both consume this crate; neither reimplements it.
- **T21** enforces L7 at the tally: a proposer's vote never enters its own claim's
  `QuorumTally`. The layer here refuses the *attempt*; the tally refuses the *count*.
- **T23** establishes identity and generates the marker key at startup, before any agent
  code runs, and exports the marker into each peer's environment.
