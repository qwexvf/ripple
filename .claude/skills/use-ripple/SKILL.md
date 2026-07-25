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
