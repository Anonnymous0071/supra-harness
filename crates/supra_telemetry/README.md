# supra_telemetry

Anonymous, opt-in usage telemetry. **T28** of the stage sequence.

## What this stage is for

Two files, deliberately small, because the privacy posture is the
design: a report is four numbers and a random id, and consent is one
word in one file with the default off.

| Module | Owns |
|---|---|
| `report` | `Report`, `Counters`: what a session did |
| `consent` | `Consent`: the marker file, off by default |

## Decisions

**The report id is random per report, never the session id.** The
session id is the identity a resume restores - a report that carried it
would let two reports be tied to one person. Two reports from one
session share nothing but the counts.

**Nothing but counts and a tier.** No file paths, no environment, no
prompt bytes. The JSON round-trip test asserts the serialized form
contains no `path` or `user` substring - the report cannot leak what it
does not carry.

**Consent is one word, and absent is off.** The marker file holds `on`
or `off`; absence reads as `None` (off); garbage reads as `None` (off) -
a corrupted marker is not consent. The default is `Off`: telemetry that
ships enabled is telemetry that was never asked for.

**The write is atomic.** Temp file, sync, rename - a half-written
marker cannot flip consent for the next session.

**The sink is not here.** This crate builds bytes; where bytes go is a
decision that belongs to the runtime configuration (T30) and the
operator. No socket opens from a library.

## Mutation results

Six mutations; five caught, control survived.

| Mutation | Verdict |
|---|---|
| M1: default consent is On | CAUGHT |
| M2: report carries a fixed identity | CAUGHT |
| M3: garbage consent reads as On | CAUGHT |
| M4: counters wrap instead of saturate | CAUGHT |
| M5: write not atomic | CAUGHT |
| M6 control: comment only | SURVIVED (control) |

## Obligations left to later stages

- **T30** reads consent at startup, builds reports from `Counters` at
  session end, and supplies the sink function. `--ignore-project-config`
  confirmation reads `Consent` too.
- **T29** renders "telemetry: on/off" in settings, sourced from this
  crate.
