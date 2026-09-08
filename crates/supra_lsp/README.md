# supra_lsp

Five language servers over one JSON-RPC/LSP client: references that flip
the AST's semantic flag, and crash recovery. **T24** of the stage
sequence.

## What this stage is for

T15.7's contract: syntactic rename and reference results carry
`semantic: false`, and T24 is the stage that flips it - a language
server proves which occurrences name the symbol under shadowing and
overloading, where the text search over-approximates.

| Module | Owns |
|---|---|
| `servers` | the five configured servers and their language coverage |
| `framing` | LSP wire framing: Content-Length headers over stdio |
| `client` | the live client: spawn, initialize, references, restart |
| `error` | `LspError`: transport / protocol / uncovered / server / crashed |

## Decisions

**Five servers cover seven languages.** rust-analyzer (Rust),
typescript-language-server (TypeScript + JavaScript), pyright (Python),
gopls (Go), clangd (C + C++). Two share, by lookup - never by guessing.
The same refusal the digest's `Language::detect` makes for unknown
extensions is the refusal this crate makes for uncovered languages.

**One restart, not a loop.** The client kills the process, re-spawns,
re-initializes, and answers the request from the fresh instance. A server
that dies twice is refused as `Crashed` - retrying twice only hides a
server that keeps dying.

**The frame is Content-Length, always.** Every message carries its
length; a frame without it is malformed, because a length the sender
did not state is a boundary a reader cannot trust. A short body is
malformed for the same reason.

## Mutation results

Seven mutations; four caught, three via guard, control survived.

| Mutation | Verdict |
|---|---|
| M1: semantic stays false (no flip) | CAUGHT via guard |
| M2: coverage lookup removed | CAUGHT |
| M3: crash restart skipped | CAUGHT |
| M4: Content-Length not enforced | CAUGHT |
| M5: header made optional | CAUGHT |
| M6: binary names are nothing | CAUGHT |
| M7 control: comment only | SURVIVED (control) |

M1 is the T15.7 flip this stage exists to perform: a server-answered
reference whose `semantic` does not flip reintroduces the shadowing
false-positives, and the literal `semantic: true` at the site where the
server-answered path constructs its reference - inside
`references_once`, not inside a test of its own construction - is what
the guard reads. A test that builds the struct with the right literal
cannot observe a mutation that changed the constructed value inside the
production code.

## Obligations left to later stages

- **T15.7** calls this crate's `references` where the digest indexed, and
  the index's `semantic: false` flips to the server's `true` where a
  server proved it.
- **T23** adds the LSP gate to step 7; the introspector's findings still
  append at step 9, now with server-proved references where the file was
  covered.
- **T26** persists no language-server state; servers restart cleanly per
  session, which is the fail-closed end of the crash story.
