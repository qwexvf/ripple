---
name: use-ripple
description: Use ripple itself to find callers, blast radius, and review targets before editing code — dogfooding the tool we're building. Use when about to change a function/module and you need to know what depends on it, when asked "what breaks if I change X", "who calls X", "what should I review", or when working in an indexed repo (TypeScript / Elixir / GraphQL). Also records gaps found while using it.
---

# Use ripple on real code

We are building ripple. The fastest way to find its bugs and missing features is to
use it while working. Prefer ripple for "what depends on this?" questions, then
verify against the source — and **log every gap** (see Dogfood log below).

Ripple is not a grep replacement. Use it when the question is about *relationships*:
callers, blast radius, cross-service reach, what to review first.

## Before you can query

```
cargo run -p ripple-cli -- index <path>...        # graph → <path0>/.ripple/graph.redb
cargo run -p ripple-cli -- lsp doctor --root <p>  # which language servers are usable here
```

Indexes `*.ts` `*.tsx` `*.ex` `*.exs` `*.gql` `*.graphql`. **No Rust adapter yet**, so
ripple cannot index its own source — dogfood it against an indexed repo (e.g. the
5noobs stack) until that lands.

Multiple roots become one graph: `index <web> <api>`. The database lives under the
**first** root.

## Queries

```
neighbors <symbol> [--in|--out] [--depth N] [--root P] [--json]
impact <symbol>... [--budget N] [--root P] [--json]     # ranked blast radius
review [<base-rev>] [--budget N] [--root P] [--json]    # hunks to look at first
risk <symbol|file> [--root P] [--json]
eval [--commits N] [--root P]                           # static vs co-change recall
```

- `--in` = callers/importers (what breaks if this changes). `--out` = dependencies.
- `impact` seeds by name and ranks by confidence-weighted diffusion; `neighbors` is
  a raw traversal. Use `impact` to decide, `neighbors` to understand.
- `risk` fuses churn / bug-density / ownership from git with structural fan-out.

## Finding the exact symbol name

`neighbors` needs an exact name, and a multi-root index **namespaces module paths by
root directory name** (`5noobs-web/src/app/page.tsx`). Guessing wastes turns — use
the MCP `search` tool to find the real name:

```
cargo run -p ripple-cli -- mcp --root <p>
# then: {"jsonrpc":"2.0","id":1,"method":"tools/call",
#        "params":{"name":"search","arguments":{"query":"device"}}}
```

MCP tools: `search`, `impact`, `review_focus`, `neighbors`, `risk`, `explain_edge`,
`reindex`. `explain_edge` gives an edge's kind, confidence and site — use it whenever
an edge looks wrong. `reindex` after edits, or results are stale.

## Read confidence, don't ignore it

Every edge carries one. `1.0`/`0.95` extracted, `0.9` known receiver or matched
GraphQL operation, `0.85` typed receiver or Ecto reference, `0.6` by-name candidate,
and `base/N` when N targets are equally plausible. A `0.3` edge is a guess with three
candidates — check it before acting on it.

## Known behaviour that looks like a bug but isn't

- Self-recursion edges are dropped on purpose (X → X says nothing about blast radius).
- Elixir multi-clause functions collapse to one symbol; arity is not distinguished.
- Elixir `@spec`/`@type` bodies are ignored (they parse as calls but name types).
- `dataloader(...)` and inline `fn` resolvers produce no edge — under-link over invent.
- `deps/`, `_build/`, `vendor/`, `target/`, `.venv/` are never indexed.
- Type-level Absinthe fields aren't joined to consumers: only *root* fields are, so a
  nested selection (`player { team { … } }`) does not reach `team`'s resolver yet.

## Dogfood log — the point of all this

When ripple gives a wrong, missing, or confusing answer, append to
[`docs/12-dogfood-log.md`](../../../docs/12-dogfood-log.md): what you asked, what it
said, what was true, and what that implies (bug / missing feature / bad UX). One entry
is worth more than a guess about what to build next — every entry so far turned into a
committed fix.

Then decide: fix now if small and clearly wrong, otherwise log it and keep moving.
