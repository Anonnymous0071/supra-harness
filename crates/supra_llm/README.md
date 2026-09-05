# supra_llm

Provider clients. **T13** of the stage sequence: three providers behind one call shape,
each with its own `CachePolicy`, one canonical serialiser, and a thinking budget frozen
at startup.

## Modules

| Module | Owns |
|---|---|
| `policy` | per-provider cache behaviour: breakpoints, TTLs, minimums, thinking floors |
| `canonical` | the only sanctioned producer of `CanonicalJson`: sorted keys, no floats |
| `client` | one request, one response: render, send, read SSE |
| `error` | failures split by recoverability, not by transport |

## What this stage is for

Everything T14 needs to render a request: the provider's cache behaviour, the canonical
bytes that keep the prefix stable, and the thinking budget the prompt is rendered
against. Not the turn loop (T23), not the ledger (T14), not the choreography (I6).

## The three providers

| Provider | Breakpoints | TTLs | Minimum | Thinking floor |
|---|---|---|---|---|
| `Anthropic` | 4 | 1h, 5m | 1024 | 1024 |
| `OpenAI` | 1 (cache key) | implicit | 1024 | none |
| `Google` | implicit | implicit | 4096 | none |

`Anthropic` is T6's table verbatim. `OpenAI` carries one `prompt_cache_key`, not four
breakpoints. `Google` documents only a 2048-4096 minimum with no TTL or prefix rules,
so the policy assumes the higher floor: padding to 4096 when the true floor is 2048
wastes tokens once, assuming 2048 when the floor is 4096 silently disables caching
every turn. Section 11 records Google as unverified; the policy encodes that honestly.

## Decisions with probes behind them

**The sort is explicit, not inherited.** `serde_json::Map` without `preserve_order` is
a `BTreeMap`, so `keys.sort()` changes nothing today - deleting it passes the suite.
The guarantee must not depend on a transitive feature flag no member selects:
enabling `preserve_order` anywhere would switch the map to insertion order, and the
canonicaliser would emit whatever the parser saw. A redundant sort is cheap; a
feature-dependent guarantee is not one. The output bytes are pinned by a test, and the
map's own ordering is pinned by another, so the day the feature flips, tests fail -
not cache-hit graphs.

**`from_canonical` has one caller.** T6 cannot verify canonicity without duplicating
this serialiser, so the constructor name transfers the obligation: whoever calls it
asserts the text is canonical. Two producers would be two implementations that can
disagree - an invisible cache break. The guard counts call sites.

**The thinking floor is checked at construction.** `budget_tokens` is rendered into the
prompt, so a below-floor budget invalidates every breakpoint from the first turn.
T7 owns the comment; this constructor is where it lives. `Request::check` re-checks
per request, because a `Request` can be built by hand around the constructor.

**The credential arrives per request.** A `Client` holding a `SecretString` keeps the
credential alive for the session and widens every clone, log, and unwind path. `send`
takes it as a parameter, uses it once in the `Authorization` header, and never stores
it. The `Debug` impl names the provider and endpoint, never the pool internals - and
there is nothing secret to redact, because nothing secret is held.

**Retries belong to the caller.** Only `RateLimited` carries a delay (the provider's
own `retry-after`, else 60 s - never zero, because retrying immediately is how a limit
becomes a ban). `Unauthorized` is never retried: a bad credential retried is how a typo
becomes a lockout. `BadResponse` is not retried blindly: a malformed response from a
healthy provider is version skew, and I6's fan-out multiplies mistakes by eighty.
Backoff lives in the turn loop, which knows how many peers wait.

**Malformed SSE is refused, not skipped.** Silently dropping a malformed event would
lose tokens without a trace. An empty stream with no usage is `BadResponse`: nothing
arrived at all. Usage parses both spellings (`input_tokens`/`prompt_tokens`); absence
is `None`, because the third provider omits it unreliably.

**Floats are refused, duplicates are refused.** A float's text form is the one
primitive whose byte-stability is not obvious, so the canonicaliser reports the path
rather than picking a formatting. Duplicate keys are legal to `serde_json::Value`
(last wins, silently) and would bless a lost argument as stable - so a strict scanner
rejects them before parsing, with `\u0041`-style escapes decoded for identity.

## Mutation results

Eight mutations, all caught. One survived first.

| Mutation | Verdict |
|---|---|
| M1 keys emitted in insertion order | CAUGHT *(survived first)* |
| M2 floats accepted | CAUGHT |
| M3 duplicate keys accepted | CAUGHT *(probe uncompilable; equivalent covered)* |
| M4 Google floor lowered to 2048 | CAUGHT |
| M5 zero budget must pass the floor | CAUGHT |
| M6 constructor skips the floor check | CAUGHT |
| M7 breakpoint count unchecked | CAUGHT |
| M8 retry delay zero | CAUGHT |

**M1** survived because `serde_json::Map` is already a `BTreeMap` without
`preserve_order`: the suite passed with the sort deleted, correctly. Closed with two
tests that pin the guarantee against the feature flip rather than against today's
container.

## Guard checks

Six structural checks, each probed. Two were blind before shipping, both familiar:

- The credential check matched the sanctioned parameter (`credential:` mid-line) as
  well as a field. Now scoped to field spelling: no `&`, end of line.
- The retry check matched `retry_after_ms` (the delay *value*) and `RateLimited`
  (the variant). Now scoped to the violation: sleeps, loops, backoffs.
- `scan` takes a file, not a directory: the first `from_canonical` count passed zero
  sites and failed the baseline. Walk the crate instead.

## Obligations left to later stages

- **T13.5** resolves whether prior-turn thinking blocks must be resent and the
  signature rule beside `tool_use`. Until then this client carries thinking blocks
  verbatim and never drops them; T14 preserves them on turns containing `tool_use`.
- **T14** renders the prefix this client sends: `tools, system, messages`, tool calls
  consecutive (20-block lookback), padding to the policy minimum (I8).
- **T23** owns retry policy, warm-up-then-fan-out (I6), and one `SecretManager` per
  session threaded into `send` alongside `Config::provider_secret`.
