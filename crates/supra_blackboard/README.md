# supra_blackboard

The shared peer blackboard: claims, per-claim votes with proposer
exclusion, incremental quorum, rotating roles. **T21** of the stage
sequence.

## What this stage is for

§4's contract, as types: no agent is privileged; peers publish claims to
one shared board, validators vote, quorum is `ceil(2k/3)` computed
rationally in `supra_types`, a proposer's vote never counts toward its
own claim (T12.5 L7), and proposer roles attach per claim and rotate
within a turn.

| Module | Owns |
|---|---|
| `board` | `Blackboard`: publish, vote, outcomes, rotation |
| `schema` | the `bb_claim`/`bb_vote` tables, the fourth `schema_component` owner |
| `error` | `BlackboardError`: store / sqlite / unknown claim / duplicate / cohort / closed / tally / proposer |

## Decisions

**The validator roster is explicit.** `publish` takes the validators by
id; membership, duplicate-detection, and the closed check all key off
one map (`AgentId -> Option<Vote>`), so a stranger has no slot to vote
through - the T20 isolation-by-absence lesson, applied to a blackboard.
A proposer listed among its own validators is refused at publish.

**The tally spans the cohort, k = proposer + validators.** `QuorumTally`
comes from T6 unchanged: `new(k)`, `quorum(k) = ceil(2k/3)` rational,
`pending` counts every unspoken member including the proposer who will
never speak. The consequence is deliberate: at k=2 one yes of two needed
stays `Open` forever, so an E1 claim escalates rather than carrying on a
single vote - small cohorts lean on deterministic gates, exactly as §4
says ("below k>=4 the guarantee comes from deterministic gates, not from
voting").

**Unreachable escalates immediately and closes.** `yes + pending <
needed` flips the claim to `Unreachable` the moment it becomes true (two
abstains at k=4 suffice), and a closed claim refuses further votes - the
turn loop escalates now, never after a 30s timeout. Reached closes the
same way, aborting whatever is still in flight.

**Rotation is per turn, least-used first, ties by id.** `next_proposer`
counts each candidate's live proposals within the turn and returns the
least-used; the id tiebreak keeps it deterministic. Two proposals in one
turn never repeat a proposer - test-pinned.

**Claims and votes persist to SQLite** under the fourth `schema_component`
(`"blackboard"`, after T11, T16.6, T18): claims in one transaction at
publish, each vote in one transaction that also closes the row on a
terminal status. The in-memory board is authoritative for the turn; the
store is the audit trail T26 resumes from.

**Defence in depth, stated.** The claim body budget (2048 bytes) is
enforced twice, independently: `publish` refuses before any store call,
and the `bb_claim` CHECK refuses at the SQL boundary. Mutation M8 (drop
the Rust check) survives *because the SQL CHECK catches the same
overflow* - two layers, either sufficient. The T15 M6 class, kept
deliberately: the test asserts the refusal shape, not which layer
refused.

## Mutation results

Ten mutations; nine caught, one survives as documented defence-in-depth,
control survived by design.

| Mutation | Verdict |
|---|---|
| M1: proposer may vote on own claim | CAUGHT |
| M2: cohort membership check dropped | CAUGHT |
| M3: duplicate vote accepted | CAUGHT |
| M4: closed claim still accepts votes | CAUGHT |
| M5: status never updated | CAUGHT |
| M6: proposer-in-validators check dropped | CAUGHT |
| M7: k excludes the proposer | CAUGHT |
| M8: body budget check dropped | SURVIVED *(documented: the SQL CHECK refuses the same overflow)* |
| M9: rotation ignores prior proposals | CAUGHT |
| M10 control: comment only | SURVIVED (control) |

## Obligations left to later stages

- **T23** owns the choreography: publish at step 4, evaluate per vote at
  step 6, execute the winning claim at step 8 with the ULID tiebreak.
- **T22** introspector findings append to this board's claims as
  evidence-bearing votes.
- **T26** resumes from the persisted `bb_claim`/`bb_vote` rows; the
  in-memory map is per-session.
