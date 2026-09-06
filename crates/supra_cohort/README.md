# supra_cohort

Deterministic cohort tier estimation. **T15.5** of the stage sequence: turn
step 2 is `cohort.signals -> score -> tier -> k, quorum, shards (0 LLM calls)`.

## Modules

| Module | Owns |
|---|---|
| `signals` | the evidence, as bands and counts - never prose, never floats |
| `score` | the ladder: bands to tier, composing by maximum |
| `admit` | the decision: tier under the limit, k, quorum, shards, escalation |
| `profile` | what past tasks taught: shape keys, saturating failures |

## What this stage is for

Cohort size is a function of evidence, never a constant. This crate reads the
evidence - digest signals (blast radius, churn, anchor count), the tool
registry (requested reversibility class), findings, and past task profiles -
and produces the tier, then the admission, then the numbers the turn loop
needs. Pure and total: the same signals always yield the same tier, on every
machine, on every run.

The arithmetic lives in T6 (`Tier`, `admit`, `quorum`, `shards_needed`); this
crate owns only what T6 does not: scoring evidence into a tier. The turn loop
(T23) supplies the signals, persists the profiles, and drives escalation.

## Decisions

**A ladder, not a weighted sum.** Weights would need tuning, tuning would need
a dataset nobody has - the "right" cohort size is unmeasured until T30's
tier-accuracy metric lands. Ordered rules with per-rule tests let data improve
the ladder without replacing it: each mistake is attributable to one rule.

**Maximum, not first match.** Overlapping evidence (wide blast *and* an active
finding) must resolve independent of rule order, because rule order is the
thing most likely to be edited casually. Each rule is reasoned about alone;
the composition cannot surprise.

**Bands, not raw counts.** A blast radius of 8 is alarming in a 10-file tool
and routine in a 10k-file monorepo. Normalisation happens at construction
(where the denominator is known), so the ladder reads bands and stays stable
while repositories grow around it.

**One finding is enough for E4.** A finding is a confirmed defect, not a
suspicion - counting to two before escalating would spend one full cohort
re-confirming what a gate already proved.

**Failures saturate at two, success resets to zero.** The third failure
teaches nothing the second did not; an unbounded counter pins every future
task at E5. A shape that recovers has recovered - old failures are not a tax.

**Sensitive areas match narrowly.** `auth`, `crypto`, `migrat` - not `sec` or
`crypt`, which would escalate every security-adjacent path on a substring.
Case-insensitive, because filesystems disagree about case.

**Unreachable and blind-mismatch skip E1.** k=2 with quorum 2 is
unanimity-shaped scrutiny; a model that disagrees with itself needs peers that
can disagree with each other. A confirmed finding steps one tier instead - the
evidence grew by one finding, so scrutiny grows by one step. E5 saturates;
there is no E6.

**`Admission` carries the numbers together.** k, quorum, shards are read in
three places (spawn, vote, fan-out); four calls at three sites is twelve
chances to pass a different k to one of them. One struct, built once: the
numbers cannot disagree because there is one of each.

## Mutation results

Fourteen mutations, all caught. Two survived first.

| Mutation | Verdict |
|---|---|
| M1 single failure escalates to E5 | CAUGHT |
| M2 active finding ignored | CAUGHT |
| M3 finding escalates to E3 not E4 | CAUGHT |
| M4 hot churn ignored | CAUGHT *(survived first)* |
| M5 irreversible request ignored | CAUGHT *(survived first)* |
| M6 E2 evidence scores E1 | CAUGHT |
| M7 unreachable quorum stays at E1 | CAUGHT |
| M8 failures accumulate unbounded | CAUGHT |
| M9 E5 needs three failures | CAUGHT |
| M10 one finding is not enough | CAUGHT |
| M11 E5 escalation wraps to E4 | CAUGHT |
| M12 crypto/migration paths not sensitive | CAUGHT |
| M13 `reduced` never reports | CAUGHT |
| M14 *(reserved)* | - |

**M4/M5** survived because each deleted signal shared its rule with covered
neighbours - an OR where every other arm had a test. Closed with minimal-evidence
tests isolating each signal alone: hot churn with nothing else must clear E3,
R3 with nothing else must clear E2.

## Guard checks

Five structural checks, each probed. Three were blind before shipping, all
familiar shapes:

- The float check passed a directory to `scan`, which reads one file -
  `production_lines` failed, `|| true` masked it as a pass. Now one file per
  call. A guard that cannot see the violation certifies it.
- The max-composition check matched presence (`tier.max` anywhere); one
  surviving call beside a new early return still passed. Now counts five.
- The escalation check's sed range covered one arm; the anchored assertion
  covered the other. Now spans the whole function and asserts the skip arm
  maps E0 to E2.
