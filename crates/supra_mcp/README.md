# supra_mcp

MCP client gateway: stdio and HTTP transports behind one static gateway
schema, manifests cached to SQLite, discovery by append. **T18** of the
stage sequence.

## What this stage is for

I3, stated in full in the architecture: "Every tool is registered at
startup; MCP servers are probed once and their manifests cached to SQLite.
Disabling a tool uses `allowed_tools` or `tool_choice`, never removal.
Dynamic capability sits behind a **static gateway schema**, and newly
discovered tools are described by appending - append does not break a
prefix, so dynamic discovery becomes cache-compatible."

| Module | Owns |
|---|---|
| `gateway` | `Gateway`: probe, cache, the static tool, dispatch |
| `transport` | `Endpoint`/`Transport`: stdio child + HTTP POST |
| `rpc` | the JSON-RPC 2.0 envelope, by hand |
| `cache` | the `mcp_tool` table, the third `schema_component` owner |
| `error` | `McpError`: transport / protocol / version / name / budget |

## Decisions

**The static gateway is one tool.** `mcp` with `server`, `tool`,
`arguments` - never changing no matter what the fleet offers. What the
servers offer reaches the model as *content* (cached rows, listed on
request), never as `tools` mutations; the prefix the model saw at BP1 has
nothing dynamic in it to break. A test pins that the registration and the
argument-blind effect resolver are the same for every fleet.

**The cache is append-only by construction.** `INSERT OR IGNORE`, never
update, never delete: a server that changed a description between sessions
appends nothing and edits nothing, because the cached row is the
manifest's promise. The second-probe test pins that no duplicate rows
appear and the first row stands byte-identical. The third
`schema_component` owner, after T11's index and T16.6's journal.

**No SDK.** MCP is JSON-RPC 2.0 with a handful of methods; the envelope
here is ~40 lines and owned. An SDK's version pins would fight the
workspace's, and its grammar would be a second implementation of one this
crate can state in full - the T15.7 lesson applied to dependencies.

**The stdio child's env is explicit.** `env_clear()` plus exactly what the
configuration says: the host env holds secrets no third-party server
needs. The test's server exits unless its env is marker-free - and the
first version of that assertion taught this machine a lesson it wrote
into the fixture: a piped python sets its own `LC_CTYPE`, so the check
marks host variables (`HOME`, `USER`, `CARGO`, ...) rather than demanding
a one-variable env.

**`-u`, or the afternoon the tests hung.** A piped python block-buffers
its stdout; an answer sitting in the buffer is a gateway blocked on
readline. Every test responder runs `python3 -u`, and the comment on each
argv says why. The transport tests (stdio round-trip, closed-child, drop
reaps) proved the shape end to end; the gateway probe tests inherited the
flag through the same fixture.

**A dead server is named, not fatal.** Probe failures return one refusal
per server, in configuration order, and the rest of the fleet proceeds -
one dead server is not a dead harness. The budget (256 tools per server)
refuses at probe time, before any byte reaches the model.

**Text blocks only, and the type check is load-bearing.** `tools/call`
results can carry image and resource blocks; the gateway answers with
text blocks only. The fixture gives every block a `text` field, so a
filter that reads `text` without checking `type` fails the test - a
base64 image blob rendered as text is corruption wearing a reply.

## Mutation results

Ten mutations; all ten caught (the control is comment-only and survives
by design).

| Mutation | Verdict |
|---|---|
| M1: cache append becomes upsert | CAUGHT |
| M2: budget check removed | CAUGHT *(survived first)* |
| M3: namespacing skipped | CAUGHT |
| M4: env_clear dropped | CAUGHT *(survived first)* |
| M5: child not reaped on drop | CAUGHT |
| M6: probe failure kills the fleet | CAUGHT |
| M7: rpc error treated as result | CAUGHT |
| M8: text filter removed | CAUGHT *(survived twice)* |
| M9: gateway resolver sees args | CAUGHT *(survived twice)* |
| M10 control: comment only | SURVIVED (control) |

Four survived first, each for a fixture-shape reason, each closed by
making the fixture able to observe the difference: M2 needed a server
that actually offers 300 tools; M4 needed the child to *check* its own
env (an `env_clear` no test can see is not enforced); M8 needed non-text
blocks carrying a `text` field (a filter reading `text` alone passed
while the blocks had none); M9 needed an empty map (two maps both
carrying `server` agreed regardless of the resolver's keying). The
pattern is the T15.7 lesson restated: a surviving mutation is a statement
about the fixture, not about the code.

## Obligations left to later stages

- **T23** registers the static `mcp` tool through T17's registry and
  routes its invocations to `Gateway::call`; the `server`/`tool` fields
  are validated by T17's schema and dispatched here.
- **T26** may render `cached_tools()` as content on request - the cache's
  ordered, byte-stable listing is what makes that render cheap.
- **T30** loads the server configuration (`ServerConfig` per entry) and
  owns the probe's failure reporting to the operator.
