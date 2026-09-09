# supra_update

Signed release verification. **T30** of the stage sequence.

## What this stage is for

`verify` checks a minisign signature over release bytes before any of
them are applied; `parse_version` reads the tag, stripping a leading
`v`. Fetching is the CLI's job, verification is this crate's - no
network is opened here.

## Decisions

**A bad key is a signature error, not a panic.** Every failure -
unparseable key, undecodable signature, failed verification - returns
`BadSignature` with the cause as text. The caller decides what a
refusal means; this crate only says no.

**Versions are semver with an optional `v`.** Tags ship as `v0.1.0`;
the parser strips one leading `v` and nothing else.

## Mutation results

Covered by the CLI behavior probes (apply refusal, check naming)
and three unit tests: `v`-stripping, tampered payload refusal,
garbage-key refusal.
