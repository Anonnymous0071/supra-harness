# supra_eval

The economy gate. **T30** of the stage sequence.

## What this stage is for

Section 10's figures are derived, not measured - this crate validates
them. `offline_shape_check` runs without network and always runs in
CI; `estimate_mills` and `measure` price live turns for the `--live`
probe.

## Decisions

**Cache read is 0.1x, output is full price.** `estimate_mills` prices
cached input at a tenth of base ($3/MTok), uncached input at base,
and output never discounted - thinking tokens are billed either way.
`measure` builds the turn record from `Completion` usage, marking a
cache hit only when `cached_tokens` is nonzero.

**An empty session passes vacuously.** Accuracy 1.0, cost 0.0, zero
turns - there is nothing to gate, so the gate says so rather than
failing on absence.

## Mutation results

Six unit tests: the shape-check over every tier and limit 1-80, the
0.1x ratio, undiscounted output, aggregation with both gates,
vacuous empty, cache-hit marking. Covered by the CLI behavior
probes P3 (offline-first) and P4/P7 (live-skip and refusal).
