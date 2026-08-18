---
title: "CLI reference"
description: "Every ripple command and flag, with what each one is actually for"
sidebar:
  label: "CLI"
  order: 1
---

```
ripple parse <file> [--json]
ripple index <path>... [--calls lsp [--calls-budget 120s]]
ripple neighbors <symbol> [--in|--out] [--depth N] [--in-file <substr>] [--root <path>] [--json]
ripple locate <task words...> [--budget N] [--root <path>] [--json]
ripple impact <symbol>... [--budget N] [--in-file <substr>] [--root <path>] [--json] [--verify lsp]
ripple review [<base>] [--budget N] [--root <path>] [--json] [--verify lsp]
ripple path <from> <to> [--depth 6] [--limit 3] [--root <path>] [--json]
ripple risk <symbol|file> [--root <path>] [--json]
ripple mcp [--root <path>]
ripple eval [--commits N] [--skip N] [--weights <spec>] [--root <path>]
ripple eval --risk | --review | --vs-grep | --oracle lsp [--sample N] [--granularity function|file]
ripple lsp doctor [--root <path>] [--budget 10s] [--json]
ripple lsp trust [--root <path>]
```

Flags shared by nearly every command:

| Flag | Meaning |
|---|---|
| `--root <path>` | Which repository to answer about. Defaults to `.`; the graph is read from `<root>/.ripple/graph.redb` |
| `--json` | Machine-readable output. Use this for anything scripted — the human format is not a stable interface |
| `--budget N` | Cap the number of hits. The output always reports `{shown, total}` so a cut is visible |

## `ripple index <path>...`

Builds the graph and writes it to `<root>/.ripple/graph.redb`. Defaults to `.` when no
path is given.

Languages: TypeScript, TSX, Python, Rust, Elixir, Go, Gleam, GraphQL. Go and Gleam
need `--calls lsp` for their call graph; the rest resolve calls from the source.
OpenAPI and Swagger documents are read too — as endpoint declarations, not as code.
An `.html` file's `<script>` block is parsed as TypeScript in place, so its symbols
and call edges resolve like a `.ts` file's — the seam single-file components
(`.vue`, `.svelte`) build on.

Pass **several paths** to index separate repositories as one graph — this is how
cross-repo tracing works, and it is the only way to get it, since git co-change cannot
bridge two histories. Module paths are namespaced per root so identical relative paths in
two repos do not collide.

Resolution runs once over every root, so an import that names a package another indexed
repo declares lands on that repo's source. Measured on a real pair (`apps/web` +
`packages/shared`): the `Tag` interface in `shared/src/types.ts` reaches 11 files in
`web`, through the package name and its barrel. Such an edge is priced at `0.85 ×` an
in-repo one — the two working trees being one program is an assumption, not something the
import says.

Two things deliberately stay inside a root: a repo's tsconfig `paths`/`baseUrl` (both
repos defining `@/*` mean different directories) and name-guessed resolution — a bare
identifier matching in the other repo. So indexing a second repository never changes the
first one's edges, which is asserted by a test.

The graph lives under the **first** root. Every other root gets a
`.ripple/index-root` pointer to it, so `cd packages/shared && ripple review` answers from
the shared graph instead of creating an empty one beside it. A pointer whose database has
gone is an error naming it, not a silently empty answer.

A graph written by an older ripple whose on-disk format this build cannot read is
deleted and rebuilt: indexing rewrites every table anyway, so there is nothing to
migrate. A *query* against one says so and names the command to run, rather than
surfacing redb's `Manual upgrade required`.

When a cross-service boundary is involved, the summary also reports what did *not*
match:

```
cross-service: 2599 consumer selection(s) matched no provider, 467 provider key(s) nothing consumes
```

Neither number is a defect count — a selection on a scalar field has no resolver to
find, and a schema field nothing calls is ordinary. They are there because the
alternative is silence: 11 GraphQL operations were once lost to a codegen casing
convention, and the edge count alone could not say so.

Re-running is incremental: unchanged files are reused from the content cache. The summary
line reports added / changed / unchanged / removed alongside the node and edge counts.

There is no watcher and no automatic re-index: after you edit code, re-index. What you do
get is a warning. `impact`, `neighbors` and `review` hash the files their answer rests on
and compare against the index, so an answer built from edited code says so on stderr —

```
⚠ 1 of 1 files in this answer changed since indexing (a.ts) — re-run `ripple index`
```

— instead of handing you a renamed function with a `0.95` next to it.

`impact` and `review` also accept **`--sync`**: instead of warning, they rebuild the
graph from the working tree in memory before answering — the extract cache is reused for
unchanged files, only the changed ones are re-parsed, and nothing is written to disk. So
`review --sync` reviews your *uncommitted* diff against a graph that already includes it,
and `impact newThing --sync` finds a symbol you just added without a re-index. It costs a
warm re-index today; the plan to make it proportional to the edit — and a resident daemon
that pays it ahead of time — is [17 — Keeping the graph in sync](../design/17-keeping-the-graph-in-sync.md).

Concurrent runs no longer collide either: redb allows a single writer at a time, so any
process — a second `ripple index`, or a query issued while one is running — waits up to
30s and then reports which database is held, rather than surfacing `Database already open.
Cannot acquire lock.` A query during an index is the common case, and it used to fail
instantly advising you to run `ripple index`, which was exactly what was running.

### `--calls lsp`

Ask each root's language server for the call edges of files whose language has **no
`refs.scm`** — where a server is the only possible source. Everywhere else LSP only grades
edges ripple already extracted (`impact --verify lsp`); here it produces them, at
confidence 0.7 with `source: LspVerified`.

Two languages use this today: **Go** (via `gopls`) and **Gleam** (via `gleam lsp`). Without the flag a Go index has symbols and
co-change but no call graph; with it, `impact` answers. `--calls-budget` caps the whole
pass (default 120s) and any file it could not reach is named in the report. Verdicts are
cached by file content hash, so a re-index of unchanged files contacts no server —
measured 12s cold, 0.33s warm on 81 files.

A server that cannot do `callHierarchy` but can do `references` — `gleam lsp` is the case —
produces `References` edges instead of `Calls`. A reference may be a type mention rather
than a call, so it is a weaker claim and gets its own edge kind; it still shows up in
`impact`, `path` and `neighbors`. Yield depends on the server having a project it can load:
232 Gleam files in aegis gave 2431 edges, while 208 `.gleam` files scattered through a repo
with no `gleam.toml` gave 98.

## `ripple locate <task words...>`

The front door for a task you can only describe, not name yet. Give it the task in plain
words and it returns the symbols to start from, ranked across every indexed repo. It
matches the words against symbol names, module paths, endpoint routes, and doc comments
(splitting `camelCase` and `snake_case`, and a trailing plural), then fuses that lexical
recall with graph centrality and per-symbol risk by Reciprocal Rank Fusion — so a word
landing on a central, risky symbol beats a bare substring hit.

Each seed line shows its kind, the fields that matched (`why`), its dependent count, and a
one-hop blast-radius preview of what it touches. `--budget` defaults to 10; when the cut
falls inside a run of equally-ranked candidates it says so, so a thin ranking is never
mistaken for a confident one. Use this before `impact`/`neighbors`, which want the exact
name `locate` hands you.

## `ripple impact <symbol>...`

The risk-ranked blast radius: what depends on this symbol, ordered by how much of the
change reaches it scaled by how risky it is. Accepts more than one symbol to model a
multi-symbol change.

Each line carries its edge kind, that edge's confidence, the depth it was found at, and
the file and line it lives on. `--budget` defaults to 20.

A name that matches several symbols exactly seeds all of them and says so — six unrelated
`run`s across three languages produce one union that otherwise reads like one symbol's
blast radius. `--in-file <substr>` narrows the seeds to symbols whose path contains the
substring, and no match is an error rather than an empty answer.

## `ripple review [<base>]`

Ranks the symbols changed in a diff by review priority — risk × downstream reach — with a
reason on each line. With no `<base>` it diffs the working tree against `HEAD`; give it a
rev (`HEAD~3`, a branch, a merge base) to review a range.

## `ripple neighbors <symbol>`

One hop, unranked. `--in` for callers and importers, `--out` for callees and imports;
`--depth N` to go further. Use this when you want the raw graph rather than a judgement
about it — `impact` is the ranked version.

Edge kinds surfaced: `Calls`, `Imports`, `ChangesWith`, `GraphqlCall`, `DbQuery`.

## `ripple path <from> <to>`

How does A reach B? Routes along the dependency direction, shortest first, each annotated
with the product of the edge confidences along it. `from` and `to` match exactly or
partially against symbol and file names. `--depth` defaults to 6 hops, `--limit` to 3
routes.

## `ripple risk <symbol|file>`

The git-derived score for one target, broken into its terms:

```
crates/resolve/src/lib.rs (crates/resolve/src/lib.rs)
  composite 0.92 | churn 0.96 bug 0.81 ownership 0.00 fanout 0.99
```

All terms are percentiles within this corpus, so they are relative to the repository, not
absolute. A term with no variance across the corpus is dropped from the composite rather
than counted — which is why `ownership` reads `0.00` in a single-author repository.

`bug` is the share of commits touching the file that matched a fix or revert pattern. It
is a heuristic about commit messages, not a defect count.

## `ripple parse <file>`

Dumps the symbols one file's adapter extracts. This is a debugging tool for adapter work —
if a symbol is missing from `impact`, check whether it was ever extracted.

## `ripple mcp`

Speaks MCP over stdio for AI agents. Indexes first if the graph is missing. See the
[MCP reference](mcp.md).

## `ripple eval`

Measures ripple against a held-out slice of git history: the newest `--commits N` commits
become the test set and the pair counts come only from older history, so co-change cannot
score itself on data it trained on. `--skip N` moves the window back.

Two other modes:

- `--oracle lsp` compares ripple's call edges against a language server's answers.
  `--sample N` (default 25) picks how many symbols to check, `--granularity function|file`
  how strictly to compare.
- `--risk` asks whether risk ranks the files that actually get fixed.
- `--review` asks the review-targeting question `--risk` can't: within the one change that
  introduced a bug, does `review` rank the defective symbol high? It mines an SZZ corpus —
  fix commits whose removed lines blame back to one introducing commit (`--converge`,
  default 0.6) that escaped for `--escape-days` (default 7) — indexes the tree checked out
  **at each introducing commit** (a throwaway `git worktree`, so the on-disk index is
  untouched), and reports the mean normalized rank of the defective symbol against the 0.5
  chance line. `--cases N` bounds the corpus, `--budget N` sets the hit@budget cutoff.
- `--vs-grep` asks the blunt question: does the graph beat plain grep? Ground truth is
  held-out co-change — for each file a test commit touched, the *other* files it touched.
  Ripple ranks the rest of the repo by dependency reach (`impact`); grep ranks it by shared
  identifiers (the files that mention a name the seed defines — what you'd grep for without
  ripple). Both get the same `--budget` k; it reports recall@k and MRR for each, plus the
  random floor. On small, consistently-named repos grep nearly ties; the graph's lead grows
  with size and cross-file/cross-language coupling (measured 3.3× grep on a real Elixir API,
  2× on a TS app).

This is how the numbers in the [dogfood log](../design/12-dogfood-log.md) were produced,
including the ones that turned out to be wrong. When there is nothing to evaluate it says
so rather than reporting `0.0%`.

## `ripple lsp doctor`

Reports which languages are indexed here and whether a language server is installed to
sharpen them:

```
language servers for /home/qwexvf/projects/ripple
  indexed languages: elixir, graphql, rust, tsx, typescript

  typescript (typescript-language-server)
    missing  not installed — this language is indexed, so its edges stay tree-sitter only
```

`--budget` (default `10s`) bounds how long a server is given to answer before it is
dropped, so a slow server cannot stall the run.

### Verification with `--verify lsp`

`impact` and `review` accept `--verify lsp`, which upgrades call edges using a language
server before ranking. `--verify-budget` (default `2s`) bounds the wait. When a server
contradicts an edge, `--floor-contradicted` keeps it at minimum confidence and
`--drop-contradicted` removes it. Design notes in
[11-lsp-integration.md](../design/11-lsp-integration.md).

## `ripple lsp trust [--root <path>]`

Configure servers in **`~/.config/ripple/lsp.json`** (or `$XDG_CONFIG_HOME/ripple/lsp.json`,
or `$RIPPLE_LSP_CONFIG`). That file is outside every repository and is always applied.

A `.ripple/lsp.json` *inside the repository being analysed* is different: it names a command
that ripple would execute, and the repository is not necessarily yours. It is therefore read
and reported but **not obeyed**:

```
⚠ ignored /path/to/repo/.ripple/lsp.json — a config inside the repository names commands to run,
  and this root is not trusted. Nothing from it was used:
    go: /bin/touch
  Move it to /home/you/.config/ripple/lsp.json to apply it everywhere,
  or run `ripple lsp trust` if you wrote this repository's copy yourself.
```

`ripple lsp trust` records the root in `~/.config/ripple/trusted-roots` — an exact path per
line, so trusting one checkout never trusts a sibling or a nested repo. `RIPPLE_TRUST_REPO_LSP=1`
does the same for CI. Nothing writes trust into the repository itself.
