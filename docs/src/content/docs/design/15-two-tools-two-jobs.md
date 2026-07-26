---
title: "15 — tree-sitter and LSP do different jobs"
description: "Why tree-sitter produces the graph and LSP grades it — the measurement that killed the accuracy-tier framing"
sidebar:
  label: "15 — Two tools, two jobs"
  order: 15
---
[`11-lsp-integration.md`](11-lsp-integration.md) called LSP "the Tier-2 accuracy tier":
the same job as tree-sitter, done more precisely, upgrading edges where a server is
available. Measurement says that framing is wrong — not because language servers are
inaccurate, but because they are doing a **different job**.

This doc records the distinction and the numbers that forced it (2026-07-26, 5noobs
stack: TypeScript/React + Elixir umbrella, two git repos, `tsgo` 7.0.0-dev and
`dexter` 0.7.1).

## What the measurement showed

`impact <symbol> --verify lsp`, 163 files in the query's neighbourhood:

| outcome | count |
|---|---|
| confirmed (both found the call) | 1516 |
| **added (only the server found it)** | **0** |
| contradicted (we have it, server doesn't) | 18 — every sampled one a server miss |

Two independent servers, on a real full-stack app, added **not one edge** ripple
lacked. Meanwhile server-specific errors were real and systematic:

- `dexter` credits a call made inside an ExUnit `test` block to the preceding `defp`.
- `dexter` reports callers for some clauses of a multi-clause function and not others.
- `tsgo` treats each overload declaration as its own symbol, so one ripple node is
  compared against eight server answers unless they are unioned first.

A tier that adds nothing and is wrong in specific ways is not an accuracy tier.

## The actual split

| | tree-sitter | LSP |
|---|---|---|
| job | **produces** the graph | **grades** the graph |
| prerequisites | none — works on code that doesn't compile | one language, one workspace, usually a build |
| reach | N repos × N languages, one pass | its own workspace |
| cadence | every index (1.5s for 1153 files) | only when you want a verdict |
| failure mode | misses a construct until someone notices | disagrees confidently and specifically |

## Why they cannot swap

**A server cannot produce this graph.** Cross-service edges are the point of ripple and
no server can emit them: 365 `GraphqlCall` edges come from matching TypeScript GraphQL
operations against Absinthe root fields *in a different repository*, and 941 `DbQuery`
edges from Ecto schema references. No language server knows about a declaration in
another language in another repo. That is the whole differentiator, and it is
structurally out of LSP's reach.

**tree-sitter cannot grade itself.** Scoring your own output against your own output
measures nothing — the same defect as the co-change leakage in
[`12-dogfood-log.md`](12-dogfood-log.md), where recall was computed over the very
commits the edges were mined from and read 4× too high. Grading needs an *independent
second implementation*. LSP is the only one available that spans languages.

## What follows for reconciliation

Because the grader is itself fallible, its verdicts are weighted rather than trusted
(the table in [`11-lsp-integration.md`](11-lsp-integration.md), and why each row is
what it is):

| case | action | reason |
|---|---|---|
| both found it | confidence → 1.0 | two independent extractions agreeing is the strongest evidence available |
| server only, inside a symbol we index | add at 0.7 | one unconfirmed source; invariant 5 forbids stating a guess as fact |
| server reports a call inside no symbol we index | count, add nothing | issue #18's territory — a file-granular claim, not a function-level one |
| server denies one of ours | **report only** | every sampled denial was the server's miss; acting on it deletes true edges |
| no server, or it timed out | nothing changes | behaviour must degrade to today's, never below it |

`--floor-contradicted` and `--drop-contradicted` exist for an operator who has reason to
trust a particular server more than this default does.

## What the grader was actually worth

Every TypeScript bug found on 2026-07-25/26 came out of the diff against a server, not
out of reading code:

| found via the oracle | effect |
|---|---|
| `.tsx` parsed with the TypeScript grammar, so JSX was error nodes | +250 symbols |
| `export { X }` as a separate statement never marked an export (shadcn convention, 24 files) | components became importable |
| barrel `export * from` not followed | 693 server-only edges → 0 |
| a call inside a `const` initialiser credited to the const | edges named the right caller |

Total: **15787 → 19640 edges (+24%)** on the same corpus, all of it tree-sitter-side
extraction fixed by looking at where an independent implementation disagreed.

That is the value: not upgraded edges, but a **regression harness** that points at what
the base tier is silently missing. `eval --oracle lsp` is where that lives, and it is
worth running on a schedule rather than per query.

## Where to invest next, and why

`--verify lsp` earning 0 additions says the vertical axis — call-resolution precision
inside one language — is near its ceiling. The number that isn't near a ceiling is
recall: held out properly, static edges link **7.1%** of same-commit file pairs, 10.5%
fused with co-change. Most real coupling is not syntactic, and **no language server will
ever close that gap**, because it cannot see across the boundary where the coupling
lives.

So the work with headroom is horizontal:

- more cross-service evidence kinds — HTTP, pub/sub, gRPC (#13); GraphQL fragments (#22)
- test ↔ implementation, config ↔ consumer, schema ↔ migration
- anything that links two artefacts no single compiler ever sees together

## A third, narrower use

For a language with no adapter yet, a server can carry call resolution while only
`tags.scm` is written (#16 Phase 4). Worth doing — but the ordering matters, and today's
result inverts the obvious one: not "use LSP instead of writing tree-sitter queries",
but **"write tree-sitter queries while a server grades them"**. That is how the +24%
above was obtained, in a language that already had an adapter.

## Honest limits of this conclusion

- One corpus, two servers, both tree-sitter-based in part (`dexter` fully so). A
  compiler-backed server (`gopls`, `rust-analyzer`, ElixirLS after a build) might add
  edges where these two did not. `added: 0` is a measurement of these two on this code,
  not a law.
- The samples are small: 30 files per language for the oracle, 163 for verification.
- "Every sampled denial was a server miss" means five, checked by hand against the
  source. It is enough to reject "trust the server", not enough to quantify how often
  the server is wrong.
