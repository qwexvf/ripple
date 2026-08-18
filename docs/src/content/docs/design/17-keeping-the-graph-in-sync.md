---
title: "17 — Keeping the graph in sync"
description: "Why the index goes stale, and the cheap way to keep answers true to the current tree: HEAD snapshot plus a lazy working-tree delta, propagated by export signature."
sidebar:
  label: "17 — Staying in sync"
  order: 17
---
The graph is a durable snapshot on disk. Code changes; the snapshot does not. Between an edit and the next `ripple index`, every answer is built from what the code *used to be* — a renamed function still answered with a `0.95` next to it, a fabricated fact presented as a measured one. Today the only defence is a warning: `impact`, `neighbors` and `review` hash the files their answer rests on and print "N files changed since indexing" on stderr. That is honest, but it puts the work on the human.

This is the plan to close the gap without the two expensive answers — rebuild-everything, or watch-everything.

## What we already know for free

Two cheap signals are already in the store:

- **A content hash per file** (`FileStamp`). Comparing it to the file on disk says whether a file changed, with one read and no parse. `warn_if_stale` already does this for the files in an answer.
- **The HEAD commit sha** (the git overlay's cache key). Churn, bug-density and co-change are derived from committed history, so they can only change when HEAD moves.

The insight is that ripple does not need to discover what changed — it can compute the dirty set from these in a pass that never parses.

## The model: HEAD snapshot + working-tree delta

Split the graph into two layers.

- **Durable layer** — the full index at HEAD, in redb. It is rebuilt only when HEAD moves, and even then incrementally: unchanged files are reused from the extract cache. Its validity key is `HEAD sha` + the extract-shape + **the query/grammar version** (see [#71](https://github.com/qwexvf/ripple/issues/71) — a `.scm` change must invalidate it, which today it does not).
- **Working layer** — the files `git status` reports dirty (modified, added, untracked), re-extracted on demand. This is usually one to five files. Their nodes and edges are spliced onto the durable graph in memory before a query answers.

So "answer against the current tree" becomes true with **no watcher**: the query re-extracts the handful of dirty files and patches the graph itself. Cost is bounded by the size of the diff, never the size of the repo.

## The clever part: propagate by export signature, not by file

Re-extraction is cheap. Re-*linking* is the part that could cost the whole repo, because resolution is a global pass. The trick is a second per-file hash — the file's **exported-symbol signature** (the set of its public names, and enough of each to tell a rename from a body edit):

- Edit a **function body** → the export signature is unchanged → only that file's *own* outgoing edges are recomputed. Every incoming edge from another file still resolves to the same `SymbolId`, so it stays valid. **O(1 file).**
- Edit a **signature or export** → re-resolve only the files that import this one, found by walking the existing `Imports` edges backwards. **O(importers), one hop** — not the repo.

Content hash tells you *what* changed; the export hash tells you *how far it propagates*. Almost every edit in an agent loop is a body edit, so almost every sync is O(1).

## Determinism

A spliced graph must be byte-identical to what a full reindex would produce — same stable sort, same tie-break, same reduction order (the invariant in `CLAUDE.md`). The splice therefore re-sorts the adjacency it touches rather than appending; a sync that produced a different edge order than a cold index would be a bug even if every edge were correct.

## Shipping order

1. **Sync-at-query** (correctness, everywhere, no watcher). Turn `warn_if_stale` into `sync_if_stale`: when an answer's files are dirty, rebuild the graph in memory — reusing the extract cache for clean files, re-extracting the dirty ones — and answer from that. The first cut reuses the existing incremental build wholesale (re-read all files, re-extract only changed, re-link once); the export-signature propagation above is the optimisation that turns the re-link from O(repo) into O(importers). Opt-in behind a flag first, then the default once it is proven fast.
2. **Resident daemon** ([#14](https://github.com/qwexvf/ripple/issues/14)) — latency only. A long-lived process holds the hot graph and applies the same deltas on debounced filesystem-watch events, so an interactive query pays ~0 and the cold-load cost disappears. Correctness never depends on the daemon; it is a cache in front of the sync that already works.

The daemon is the well-known answer (it is how an LSP server stays live). The point of this document is that ripple should not *need* it to be correct — the cheap signals it already stores make the working tree reachable from any query, and the export-signature hash makes keeping up with edits proportional to the edit, not the repository.
