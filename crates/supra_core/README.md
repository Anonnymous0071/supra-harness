# supra_core

The turn loop: thirteen steps, one session, no orchestrator. **T23** of
the stage sequence.

## What this stage is for

Section 9's loop, driven one vote at a time. The loop owns no LLM
client: answers arrive through `Turn::record`, so the choreography -
publish, evaluate per vote, abort on reach, escalate on unreachable -
is testable without a network, and the runtime (T30) supplies answers as
its shards complete.

| Module | Owns |
|---|---|
| `turn` | `Turn`, `PeerAnswer`, `Step`: the state machine |
| `error` | `TurnError`: ledger / segment / no-answer / empty claim |

## The mapping to the thirteen steps

Steps 1-2 (anchors, tier estimation) are the caller's inputs - pure
functions over the digest and cohort crates, with no LLM calls to hide.
`Turn::start` is step 4 (publish) with the ceiling checks step 2
implies; `shards()` reports the fan-out split the request budget needs.
`record` is step 6 (evaluate quorum incrementally per vote), returning
the step so the caller never waits on a late peer while quorum remains
reachable. `finish` is steps 8 and 13: the winning claim's body sealed
into the ledger, the segment and completion published as events. Steps
5, 7, 10, 11 are the runtime's - this crate's contract is the
choreography they plug into, and every observable moment (vote, reach,
abort, escalation, seal, completion) is an event, so the TUI never
blocks.

## Decisions

**The first answer is the working answer; agreement is the vote.** A
peer votes yes exactly when its answer matches the working answer -
which is what a quorum over answers means. A diverging peer votes no;
enough divergence makes quorum unreachable and the turn escalates
immediately, never after a timeout.

**Reached closes the claim; the closed claim refuses late peers.** The
abandonment of in-flight work is structural: after reach, the
blackboard's claim is closed and any late `record` refuses with the
blackboard's own reason. The same happens after unreachable.

**The ceiling and the empty cohort are refused twice, independently.**
`start` checks both, and the `let-else` over `agents.first()` refuses an
empty cohort again below the check - the second layer is why mutation
M7 survives (both layers produce the same refusal; dropping either
changes nothing observable). The claim-body check in `finish` has the
same shape: the blackboard's SQL CHECK refuses an empty body at publish,
so the loop's own `EmptyClaim` arm is the second layer, not the only
one. Both are the T15 M6 class, kept deliberately.

**The turn id is the ledger's id.** `finish` seals the claim body as a
`SegmentKind::Turn { turn, role: Assistant }` segment under the same
`TurnId` the events carried, so the store's segments and the bus's
events name the same turn.

## Mutation results

Ten mutations; eight caught, two survive as documented defence-in-depth,
control survived by design.

| Mutation | Verdict |
|---|---|
| M1: disagreeing peer votes yes | CAUGHT |
| M2: first answer never kept as working | CAUGHT |
| M3: reached does not abort the cohort | CAUGHT |
| M4: unreachable does not escalate | CAUGHT *(survived first)* |
| M5: votes are not events | CAUGHT |
| M6: peer ceiling not enforced | CAUGHT *(survived first)* |
| M7: empty cohort allowed | SURVIVED *(documented: let-else refuses identically)* |
| M8: finish ignores the claim body | SURVIVED *(documented: the blackboard's CHECK refuses empty bodies at publish)* |
| M9: finish seals nothing | CAUGHT |
| M10 control: comment only | SURVIVED (control) |

M4 and M6 survived first for fixture-shape reasons and closed with
targeted tests (the escalation abort's reason is asserted verbatim; the
over-ceiling refusal's message names the ceiling). M7 and M8 survive
because a second, independent layer produces the same refusal - the
recurring defence-in-depth class, stated rather than hidden.

Three fixture lessons en route, all arithmetic: k=3 with one yes, one
no, and the never-speaking proposer leaves pending=1, so
yes+pending=2 >= needed=2 and the claim is still reachable - unreachable
needed the third disagreeing validator; k=4 needs quorum 4, which
three validators can never supply, so the late-peer fixture moved to
k=6 where four agreeing validators reach and a fifth is genuinely late;
and the loop `peers[1..5]` is four voters, not five - the reach landed
inside the loop's own Collecting assertion. The quorum arithmetic is
rational, exact, and unforgiving of off-by-one fixtures, which is the
point of computing it that way.

## Obligations left to later stages

- **T24/T25** add the LSP and DAP gates to step 7; this crate's
  `Turn` hands the winning claim to whoever executes it.
- **T26** persists the turn's segments and the blackboard rows; the
  store side is already written by T21.
- **T29** subscribes the topics this crate publishes and renders the
  vote-by-vote stream.
- **T30** drives the loop with a real `Client`, feeds shard answers
  into `record`, and owns steps 11-12 (self-critique, rewrite at 92%).
