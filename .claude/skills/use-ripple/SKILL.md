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
1153 files → 9179 nodes, 20316 edges
  (2569 co-change, 825 graphql, 941 db, 134 imported, 1634 file-granular, 4471 with dependents)
  ↑ +4313 edges on 2026-07-25/26. TS side: .tsx uses the TSX grammar, JSX rendering is
    a call, `export { X }` lists count as exports, barrels (`export * from`) are followed,
    aliased/namespace imports resolve. GraphQL side: nested selections descend the type
    graph, `dataloader(Mod)` links to its context module at 0.5, and fragment spreads
    expand (343 → 825 graphql edges)
cold index ~1.6s, warm ~0.8s
eval (5noobs-web, held out — co-change mined only from commits older than the test window):
  --commits 50 (503 train, 2078 pairs): static 11.8% | co-change 3.7% | fused 15.1%
  ↑ was static 7.1% / fused 10.5% before 2026-07-26's extraction work; a 300-commit
    test window starves co-change (15 trained pairs), so quote the 50 line
eval --oracle lsp vs dexter 0.7.1, 40 files (compare by position, state granularity):
  elixir  --granularity function: 165/165 (100.0%) | 0 ripple-only | 0 server-only
  elixir  --granularity file    : 153/165 (92.7%)  | 44 ripple-only | 0 server-only
  30 files, per language (tsgo 7.0.0-dev for ts/tsx, dexter 0.7.1 for elixir):
    typescript   34/35 (97.1%) | 5 ripple-only | 0 server-only
    tsx          32/35 (91.4%) | 3 ripple-only | 0 server-only
    elixir       94/95 (98.9%) | 1 ripple-only | 0 server-only
    server answers are unioned per ripple symbol first — overloads and Elixir clauses
    are several server symbols for one node, and judging each separately invented
    disagreements (45 of them, once barrels started resolving)
  the 44 are true edges dexter misses (it reports 1 caller for filter_posts and misses
  5 real call sites in lfg_posts_test.exs) — ripple-only at file granularity is capped
  by the oracle's own completeness, so read it with that in mind
153 call sites still sit inside no indexed symbol; they now link at file granularity
impact changeset --verify lsp (163 files, dexter 0.7.1):
  1516 confirmed | 0 added | 18 contradicted (reported, not applied)
  cold 7.6s → warm 238ms, all 163 files replayed from the verdict cache, no server hit
  (cache key = file content hash, so an edit invalidates just that file)
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

`.tsx` is a **separate adapter** (`tsx`) from `.ts` (`typescript`) because JSX only
exists in the TSX grammar — so a `.ripple/lsp.json` for a web root wants an entry for
each, and `lsp doctor` lists both.

**TypeScript oracle:** `typescript-language-server` is *not* installed; `tsgo`
(`npm i -g @typescript/native-preview`, TS 7 preview) is, and it answers
`callHierarchy`. It isn't the built-in default, so point ripple at it with
`<web-root>/.ripple/lsp.json` — and remember `rm -rf .ripple` wipes it:

```json
[{ "language": "typescript", "command": "tsgo", "args": ["--lsp", "--stdio"],
   "root_markers": ["tsconfig.json", "package.json"],
   "init_timeout_ms": 60000, "request_timeout_ms": 15000 },
 { "language": "tsx", "command": "tsgo", "args": ["--lsp", "--stdio"],
   "root_markers": ["tsconfig.json", "package.json"],
   "init_timeout_ms": 60000, "request_timeout_ms": 15000 }]
```

Handshake is ~2s (dexter is 40ms), and it needs its server→client
`client/registerCapability` request answered or it serves nothing — the client does
that now, but a server that "never answers" is worth suspecting there first.

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
path <from> <to> [--depth 6] [--limit 3] [--root P] [--json]  # how does A reach B?
risk <symbol|file> [--root P] [--json]
eval [--commits N] [--root P]        # static vs co-change recall on N held-out commits
```

- `--in` = callers/importers (what breaks if this changes). `--out` = dependencies.
- `impact` seeds by name and ranks by confidence-weighted diffusion; `neighbors` is
  a raw traversal. Use `impact` to decide, `neighbors` to understand.
- `risk` fuses churn / bug-density / ownership from git with structural fan-out.
  `eval --risk` measures whether it ranks the files a held-out fix later touched. Latest:
  ownership 2.09× / fanout 1.95× / churn 1.22× / bug_density 0.79× / **composite 0.94×**.
  The blend is worse than three of its four inputs — its heaviest weights sit on its
  weakest terms (#19). Read `risk`'s composite ordering as unreliable for now.
- `path` enumerates routes A→B along dependency direction, shortest first, and reports
  the product of the edge confidences. Co-change edges are excluded — a companion is
  not a route. This is the front-to-DB chain in one command.
- A server's answer is attributed to a symbol by the **call's position**
  (`fromRanges`), never by the caller name the server chose — dexter credits a call
  inside an ExUnit `test` block to the preceding `defp`, and trusting that added 145
  false edges before it was checked.
- A symbol lookup widens only when it must: exact, then qualified-name suffix
  (`impact LfgPost` finds `FiveNoobs.Lfgs.LfgPost`), then case-insensitive substring.
  The command prints which rule fired — a substring hit is a guess, not an answer.
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
- A nested Absinthe field is reached by descending the type graph, and a fragment spread
  is expanded against its type condition. Two gaps remain: an **inline** fragment
  (`... on Type { … }`) is skipped, and `batch(...)` resolvers are not linked at all.
- `dataloader(Mod)` names a context module and no function, so its edge targets that
  file's module node at **0.5** — `impact` shows it as `[file] …`, not as a symbol.

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
