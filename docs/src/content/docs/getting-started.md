---
title: "Getting started"
description: "Build ripple, index a repository, and read your first ranked blast radius — with the output you should actually see"
sidebar:
  label: "Getting started"
  order: 1
---

Every command below was run against ripple's own repository, and the output is pasted
verbatim. If you follow along in a clone of ripple you should see something very close
to it — the numbers move as the git history grows.

## Build

There are no binary releases yet, so build from source. Rust 1.85 or newer (edition 2021).

```bash
git clone https://github.com/qwexvf/ripple && cd ripple
cargo build --release
```

The binary lands at `target/release/ripple`. Put it on your `PATH` or call it by path;
the examples below assume `ripple` resolves.

## Index a repository

```bash
ripple index .
```

```
indexed 83 files across 1 root(s) (83 added, 0 changed, 0 unchanged, 0 removed)
  → 856 nodes, 962 edges (79 co-change, 6 graphql, 0 db, 1 imported, 4 file-granular,
    309 with dependents) (./.ripple/graph.redb)
```

The graph is written to `.ripple/graph.redb`. Add `.ripple/` to your `.gitignore`.

Two things worth reading in that line. **`79 co-change`** are edges that came from git
history rather than from the source — files that keep changing in the same commit with
no call between them. **`309 with dependents`** is how many symbols anything actually
points at; if that number is low for your repo, call resolution is not reaching your
code and the rankings below will be thin.

> [!NOTE]
> Co-change needs history. On a `--depth 1` clone there is nothing to mine, and ripple
> degrades to static edges only rather than pretending. `git fetch --depth 400` first if
> you cloned shallow.

Indexing is fast enough that it is not a thing you schedule: 378 files of `honojs/hono`
take 0.47 s cold and 0.33 s warm on a release build. Roughly 600–800 files/second.

## Ask what breaks

```bash
ripple impact link
```

```
blast radius of link — 20 of 26 hits (ranked) — 6 more cut by --budget 20:
  1.55  Calls<0.81> build_incremental (crates/resolve/src/lib.rs)
  1.25  Calls<0.65> build (crates/resolve/src/lib.rs)
  0.95  Calls<0.51> index_project (crates/cli/src/main.rs)
  0.82  Calls<0.51> resolves_bare_calls_through_an_elixir_import (crates/resolve/tests/build.rs)
  0.74  Calls<0.42> cmd_mcp (crates/cli/src/main.rs)
  ...
```

Read the columns left to right:

- **`1.55`** — the ranking key: how much of the change's weight reaches this symbol,
  scaled by how risky the symbol itself is. This is the whole point; a flat set of
  26 dependents would not tell you to look at `build_incremental` first.
- **`Calls<0.81>`** — the edge kind it arrived on and the confidence. `1.0` means
  extracted directly from source. Anything lower is inferred, and ripple splits
  confidence across ambiguous candidates rather than inventing one edge.
- **`20 of 26 hits ... 6 more cut by --budget 20`** — the truncation is stated. It is
  never silent, which matters when an agent is consuming this.

A `ChangesWith` line in that list is the interesting case: it is a place that has no
static path to your change but that history says gets fixed alongside it.

## Ask what to review

```bash
ripple review HEAD~3
```

```
review focus (15 changed symbols), highest priority first:
  14.95  macro_call (crates/lang/src/elixir/macros.rs)      — high bug-density (0.74), high churn (0.74), 20 downstream, untested
  13.78  collect_keywords (crates/lang/src/elixir/macros.rs) — high bug-density (0.74), high churn (0.74), 21 downstream, untested
  11.59  link_cross_service (crates/resolve/src/crossservice.rs) — high bug-density (0.70), high churn (0.89), 9 downstream, untested
  ...
  2.36   crates/resolve/tests/fixtures/nested/badges.ex      — 2 downstream, untested
```

Top to bottom is a 6× spread here, and on larger histories it reaches 30×. That ordering
is what ripple is for — it changes where a reviewer's attention goes, and it does not
depend on the co-change signal being strong.

Two names in that output are weaker than they sound, and it is better to know now:
`bug-density` is the share of commits touching the file that looked like fixes or
reverts, not defects per KLOC; `untested` means ripple found no test referencing the
symbol, which is also what an unresolved reference looks like.

## Trace a route

```bash
ripple path cmd_impact impact
```

```
route 1 — 1 hops, confidence 0.75
  cmd_impact (crates/cli/src/main.rs)
    │ Calls<0.75> line 394
  ▼ impact (crates/query/src/lib.rs)
```

On a cross-service setup this is the command that earns its keep: a React page reaching
an Absinthe resolver in a different repository, in one traversal. See
[14-demo.md](design/14-demo.md).

## Score one thing

```bash
ripple risk crates/resolve/src/lib.rs
```

```
crates/resolve/src/lib.rs (crates/resolve/src/lib.rs)
  composite 0.92 | churn 0.96 bug 0.81 ownership 0.00 fanout 0.99
```

`ownership 0.00` here is not a bug in your repo — with a single author every file scores
identically and the percentile collapses. Terms with no variance across the corpus are
dropped from the composite rather than counted as zero.

## Hand it to an agent

```bash
ripple mcp
```

An MCP server over stdio with eight tools. If the graph is missing it indexes first, so
an agent can be pointed at a repository that has never been indexed. Setup and the full
tool list are in the [MCP reference](reference/mcp.md).

## Before you trust it

- **Re-index after editing.** There is no watcher and no staleness check — `ripple index`
  after you change code, or `impact` answers from the old graph without saying so.
- **Check your language.** Call resolution is usable for TypeScript and Elixir. Rust and
  GraphQL are shallower; everything else is unsupported. `ripple lsp doctor` reports what
  is indexed here and whether a language server is available to sharpen it.
- **Check `with dependents`.** If the index line reports few symbols with dependents,
  resolution is not reaching your code and the rankings are built on very little.
