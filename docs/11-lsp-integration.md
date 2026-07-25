# 11 — LSP as the accuracy tier

**Decision.** Take what language servers already compute (per-language, per-file
truth about symbols and calls) and layer ripple's own value on top. Tree-sitter
stays the base tier so ripple always answers; LSP *upgrades* what it can reach.
No query ever blocks on a server.

## Why lean on LSP at all

Hand-writing Tier-2 call resolution per language is the expensive part. Elixir
proved it: a `refs.scm` plus three separate correctness fixes — a definition's
own name in its header parsing as a call, multi-clause functions sharing one
`SymbolId` (30k duplicate edges), and `@spec`/`@type` bodies parsing as calls
(4.2k bogus edges). Every language brings its own version of that. `gopls`,
`pyright`, `rust-analyzer`, `ElixirLS`/`Lexical` already solve it correctly, with
arity, `import`ed and macro-generated calls, and protocol dispatch.

Elixir-specific alternatives (`mix xref`, compiler tracers) give the same data in
bulk and are cheaper per repo — but they're Elixir-only. LSP is the interface
nearly every language already implements, so it's the one worth building against.

**`dexter` (remoteoss) is the Elixir case that shaped this design.** It speaks
plain LSP with `callHierarchy/incomingCalls`/`outgoingCalls`, follows
`defdelegate` chains, and — the important part — *requires no compilation*: it
indexes from source, ~11s for a 57k-file monorepo, ~10ms queries. That kills the
assumption that a language server implies a build, and it's why the built-in table
marks Elixir `inline = true` while `rust-analyzer` (which builds its cache first)
is background-warm only. Being tree-sitter-based, dexter is a strong *peer* rather
than an oracle: agreement doesn't prove correctness, but disagreement localises a
bug in either side. Compiler tracers remain the only true ground truth.

## The layering

| tier | source | rationale |
|---|---|---|
| 0–1 symbols, imports, modules | tree-sitter `.scm` | no build, no server, works on code that doesn't compile |
| **2 call resolution** | **LSP where available**, tree-sitter as fallback | correctness for the price of config, not code |
| 3 cross-service (GraphQL, Ecto, HTTP) | ripple only | no server knows the TS↔Absinthe join |
| evolution (churn, ownership, co-change) | ripple only (git) | 34 of ripple's 40 recall points |
| decisions (blast radius, review targeting, risk) | ripple only | the actual product |

A server knows one language, in one workspace, that compiles. Ripple knows *N*
repos, *N* languages, their history, and what to look at first. Those don't
compete.

## Slow servers are a design constraint, not a blocker

Cold-start dominates: `ElixirLS` compiles the project, `rust-analyzer` builds its
cache — minutes, not milliseconds. Per-request latency is secondary but real, and
`callHierarchy` has no bulk form (one request per symbol). Five mechanisms keep
that off the critical path:

1. **Never inline-block.** The tree-sitter graph is the answer path. LSP results
   only ever *upgrade* the persisted graph. A slow or missing server changes
   result *freshness*, never query latency.
2. **Hard budget.** `--verify lsp --budget 2s`: whatever returns inside the
   budget is used, the rest stays tree-sitter, and the output reports
   `{verified, unverified}` so the answer is never silently partial.
3. **Content-hash cache.** Verified edges persist keyed by the file's BLAKE3 hash
   — already computed for incremental indexing. Verification is paid once per
   file *version* and reused by every later query. Cost amortizes to zero on
   stable files.
4. **Background warm, churn-ordered.** `ripple lsp warm` verifies during idle
   time, walking files in git-churn order (the overlay already ranks them), so
   the files a human is most likely to touch are verified before they ask.
5. **Per-server profile, measured not guessed.** `ripple lsp doctor` probes each
   configured server: is it present, does it implement `callHierarchy`, how long
   to index, what's the median request latency. That measurement sets
   `max_concurrency`, `request_timeout`, and whether the server is allowed
   inline at all or is background-warm only.

Bulk beats per-symbol wherever the protocol offers it: `documentSymbol` is one
request per file; `callHierarchy` is only spent on symbols inside the query's
neighborhood (a PR is ~50 symbols, not the graph's ~25k).

## Reconciliation

Rides on the existing `confidence` field, so `store` and `query` stay unaware:

| case | action |
|---|---|
| our edge, confirmed by server | confidence → 1.0 |
| server has an edge we lack | add at 1.0 — the real win |
| we have it, server denies it, file **is** indexed | drop, or floor confidence |
| server absent, timed out, or file unindexed | keep ours untouched |

That last row is why this is safe to add: with no server, behaviour is exactly
today's.

## Determinism and provenance

"Query output is reproducible" is a load-bearing invariant, and LSP answers vary
with server version and index state. So LSP-derived edges are **persisted data,
not recomputed truth**: they are written to the store with an explicit source, and
a query never re-derives them mid-session. This adds a `source` field to
`ir::Edge` (`Extracted` | `LspVerified` | `CoChange`), which also makes
`explain_edge` honest about where a claim came from.

## Configuration is data

Adding a server must not touch Rust — same rule as the `.scm` adapter seam:

JSON, not TOML — `serde_json` is already a dependency and a config format isn't
worth a new one. An entry replaces the built-in for that language; omitted fields
take their default.

```json
// .ripple/lsp.json
[
  { "language": "elixir", "command": "lexical", "inline": false, "max_concurrency": 2 },
  { "language": "gleam",  "command": "gleam", "args": ["lsp"], "root_markers": ["gleam.toml"] }
]
```

Built-in defaults cover `elixir` (dexter), `typescript`, `go`, `python`, and
`rust`.

## Phases

1. **`crates/lsp` — client + `doctor`.** ✅ **done.** Synchronous stdio JSON-RPC
   (reader thread, id-matched requests, per-request timeout, stderr drained and
   kept as diagnostics), capability probe, and `ripple lsp doctor [--json]`, which
   probes **every indexed root** — a cross-repo index has a different language mix
   per root, so the Elixir server belongs to the repo with `mix.exs`, not to the
   one holding the database. Each line states a fact about the environment:
   `n/a` (no root marker), `missing` (and whether that language is actually
   indexed), `broken` (with the tail of the server's stderr), or `ready` (with
   handshake latency and the capabilities that matter). Verified against real
   `gopls`: 128ms handshake, `callHierarchy=true`.
   Still to add here: UTF-16↔byte position conversion, once something actually
   sends positions.
2. **`ripple eval --oracle lsp --sample N`.** ✅ **done.** Samples files evenly
   across a root, asks the server for each function's callers via
   `documentSymbol` → `prepareCallHierarchy` → `incomingCalls`, and diffs against
   ripple's `Calls` edges. Callers in files ripple doesn't index are dropped (a
   server that also indexes stdlib would otherwise look infinitely better), and
   self-recursion is excluded because ripple drops it deliberately.

   Comparability is most of the work: servers spell one function three ways
   (`changeset`, `changeset/2`, `FiveNoobs.Players.PlayerReport.changeset`), and
   "the server can't resolve this symbol" must stay distinct from "this symbol has
   no callers". First result against dexter on 5noobs: **144/165 (87.3%) identical
   caller sets**, 1 possible false positive, 20 possible misses. It immediately
   found two real bugs — a renamed `alias ... as:` that made calls through it
   unresolvable (fixed), and Elixir `import` being unhandled (open). See
   [`12-dogfood-log.md`](12-dogfood-log.md).
3. **On-demand verification.** `impact` / `review_focus --verify lsp` over the
   seed set plus one hop, with the budget and cache above.
4. **Breadth proof.** Add Go or Python with `tags.scm` only and all call edges
   from the server; measure the cost of "adding a language".

## What this does not fix

Recall stays ~40% and is dominated by non-static coupling (co-change), not
call-graph precision. This raises Tier-2 *precision*. It does not widen coverage,
and no server will ever produce the cross-repo cross-service edges that are
ripple's actual differentiator.
