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

## Baseline numbers — compare against these

Last measured 2026-07-25 on the 5noobs stack (web + api). A count that moves without
an explanation is the cheapest bug signal this project has, so re-measure and diff:

```
1153 files → 8929 nodes, 15787 edges
  (2461 co-change, 343 graphql, 941 db, 134 imported, 1634 file-granular, 4117 with dependents)
  ↑ nodes/with-dependents dropped on 2026-07-25 because duplicate definition nodes
    (one per clause) stopped being counted — the store only ever kept one of each
cold index ~1.4s, warm ~0.7s
eval (5noobs-web, held out — co-change mined only from commits older than the test window):
  --commits 50  (500 train, 2078 pairs): static 7.1% | co-change 3.4% | fused 10.5%
  --commits 300 (148 train, 4188 pairs): static 6.5% | co-change 1.3% | fused  7.8%
  ↑ prefer the 50 line: a 300-commit test window leaves only 148 training commits
    (15 trained pairs), so co-change is starved, not wrong
eval --oracle lsp vs dexter 0.7.1, 40 files (compare by position, state granularity):
  --granularity function: 165/165 (100.0%) | 0 ripple-only | 0 server-only
  --granularity file    : 153/165 (92.7%)  | 44 ripple-only | 0 server-only
  the 44 are true edges dexter misses (it reports 1 caller for filter_posts and misses
  5 real call sites in lfg_posts_test.exs) — ripple-only at file granularity is capped
  by the oracle's own completeness, so read it with that in mind
153 call sites still sit inside no indexed symbol; they now link at file granularity
impact changeset --verify lsp (132 files, ~8s, dexter 0.7.1):
  1516 confirmed | 0 added | 28 contradicted (reported, not applied) | 331 unresolved
```

**Build the release binary before timing anything** or compile time lands inside the
measurement — that has produced two wrong numbers already.

Test stack: `~/projects/private/omeroid/5noobs/5noobs-api` (Elixir umbrella) and
`~/.../5noobs-api/5noobs-web` (TS, its own git repo). Index cross-repo with
`index <WEB> <API>`; the database lands under the **first** root. `rm -rf <repo>/.ripple`
when finished.

`dexter` (Elixir LSP, needs no compile) is installed via mise, but its shim isn't on a
non-interactive PATH — prefix with `PATH=~/.local/share/mise/shims:$PATH`. Its CLI
resolves the index from the **current directory**, so `dexter references` run from
elsewhere silently reports nothing.

Backlog, follow-ups and open questions live on the board:
<https://github.com/users/qwexvf/projects/7>. Don't keep a second list.

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
  ...both accept --verify lsp [--verify-budget 2s]      # upgrade calls from a language server
risk <symbol|file> [--root P] [--json]
eval [--commits N] [--root P]        # static vs co-change recall on N held-out commits
```

- `--in` = callers/importers (what breaks if this changes). `--out` = dependencies.
- `impact` seeds by name and ranks by confidence-weighted diffusion; `neighbors` is
  a raw traversal. Use `impact` to decide, `neighbors` to understand.
- `risk` fuses churn / bug-density / ownership from git with structural fan-out.
- A server's answer is attributed to a symbol by the **call's position**
  (`fromRanges`), never by the caller name the server chose — dexter credits a call
  inside an ExUnit `test` block to the preceding `defp`, and trusting that added 145
  false edges before it was checked.
- `--verify lsp` asks the language server about the seed files plus one hop, then
  **persists** what it learns (confirmed → 1.0, server-only → 0.7, both
  `LspVerified`). It never blocks the answer: past `--verify-budget` it prints the
  files it skipped. Contradictions are reported only — a server denial is not
  evidence of absence, measured. `--floor-contradicted` / `--drop-contradicted`
  act on them if you mean it. Needs the server on PATH (see dexter note above).

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
- A call outside every function (module body, ExUnit `test` block, `.exs` script) is
  attributed to its module, so `impact` shows `[file] path` or a `defmodule` symbol as
  a caller. Those are the `file-granular` count in the index summary — real edges, one
  level coarser.
- Elixir multi-clause functions collapse to one symbol; arity is not distinguished.
  Every clause's span is kept (`Node::extra_spans`), so "which symbol contains this
  line?" still works — but an LSP answer must be unioned across clauses before it is
  compared, or comparing clause-by-clause invents contradictions.
- Elixir `@spec`/`@type` bodies are ignored (they parse as calls but name types).
- `dataloader(...)` and inline `fn` resolvers produce no edge — under-link over invent.
- `deps/`, `_build/`, `vendor/`, `target/`, `.venv/` are never indexed.
- Type-level Absinthe fields aren't joined to consumers: only *root* fields are, so a
  nested selection (`player { team { … } }`) does not reach `team`'s resolver yet.

## After every phase: the short loop (cheap)

Four steps, no prose. Run these when a phase lands.

**1. Gates.** `cargo fmt --all --check`, `cargo clippy --all-targets` (0 warnings),
`cargo test`. Non-negotiable.

**2. Re-measure and write the numbers down.** Index the 5noobs stack; record
files / nodes / edges / graphql / db / cold + warm. Build first (`cargo build
--release`) or compile time lands inside the timing — that mistake has been made
twice. **A count that moves unexpectedly is the cheapest bug detector here** — the
dependency-indexing bug, the duplicate-edge bug and the typespec bug were all first
seen as a number that didn't add up. If a count changes, explain why before moving on.

**3. Sanity-check ripple's risk output on your own change.**

```
cargo run -p ripple-cli -- risk <file-you-changed> --root <p>
cargo run -p ripple-cli -- review --root <indexed repo>
```

Does the ranking match where the danger actually was? Something risky ranking low,
or something trivial ranking top, **is a finding** — the weights are hand-set
constants that get no other feedback. This step is what exposed that `composite` was
missing three of its six inputs.

**4. Log anything surprising, commit, next phase.** One concern per commit.

## Every 2–3 phases: the full review (expensive — don't run it more often)

A proper review costs real context: reading the whole diff, re-deriving claims,
building repros. Running it after every phase wastes tokens on unchanged code, so
batch it. Trigger it when 2–3 phases have landed, or immediately if a phase touched
an invariant, changed a public API, or produced a number nobody can explain.

Review `git diff <last-reviewed>..HEAD`. Hunt these specific failure shapes, all of
which have already occurred here:

- a fix for one language silently damaging another (an Elixir guard deleted a
  TypeScript edge)
- a **silent zero** reported as a result (`eval` printed 0.0% recall)
- a **mislabelled number** (the summary counted `graphql + db` as "graphql")
- **inflated counts** from indexing code nobody will change (deps were 74% of the graph)
- **a claim the code doesn't honour** (`risk` documented six inputs, blended three)
- a test that passes whether or not the fix is present — disable the fix and confirm
  the test fails

### Output format

Fixed shape, so reviews are comparable over time and skimmable:

```markdown
# Code review — <scope>

**Purpose.** The decision this review informs (ship? build on? revert?).
**Scope.** `<range>` — N commits, N files, +X/−Y. Areas touched.
**Verdict.** Ship-ready / blocked, and on what.

## Must fix
One block per finding: `file:line`, what's wrong, how it FAILS (concrete input →
wrong output), and whether a test would have caught it.

## Should fix
Table: `#` | `file:line` | issue | failure.

## Nits
One line each. Non-blocking, labelled `nit:`.

## What I checked and trust
The claims actually verified, with the evidence (numbers, repros, byte-comparisons)
— not a list of everything that exists.

## Known-weak, honestly
Limitations shipped on purpose, so they don't get rediscovered as bugs.

## Recommended order
Numbered, by (damage × certainty) ÷ effort.
```

Rules: every finding names a **concrete failure**, not a worry. Severity is decided
by damage, not by how odd the code looks. No praise sections. Say plainly when a
finding is pre-existing rather than introduced by the diff under review — it changes
whether it blocks the push.

## Dogfood log — the point of all this

When ripple gives a wrong, missing, or confusing answer, append to
[`docs/12-dogfood-log.md`](../../../docs/12-dogfood-log.md): what you asked, what it
said, what was true, and what that implies (bug / missing feature / bad UX). One entry
is worth more than a guess about what to build next — every entry so far turned into a
committed fix.

Then decide: fix now if small and clearly wrong, otherwise log it and keep moving.
