# 14 — Demo: one question, across two repos and two languages

A walkthrough of ripple on a real full-stack app: a TypeScript/React frontend and an
Elixir umbrella backend, in **separate git repositories**, indexed as one graph.

Every output below is copied from an actual run (2026-07-26, `5noobs` stack:
382 TS/TSX files + 771 Elixir files). Numbers will differ on other code; the shapes
won't. Where ripple is still wrong or coarse, this doc says so — the last section is
the honest list.

## Setup

```bash
cargo build --release                      # measure the release binary, always
ripple index <web-repo> <api-repo>         # one graph; the database lands under the first root
```

```
indexed 1153 files across 2 root(s) (1153 added, 0 changed, 0 unchanged, 0 removed)
  → 9179 nodes, 19538 edges (2461 co-change, 343 graphql, 941 db, 134 imported,
    1634 file-granular, 4441 with dependents)
```

Cold ~1.5s, warm ~0.8s. Module paths are namespaced by root (`5noobs-web/src/…`,
`5noobs-api/apps/…`) so same-named files in different repos can't collide.

## 1. Change a backend function — who notices?

```bash
ripple impact filter_posts --budget 6
```

```
blast radius of filter_posts — 6 of 90 hits (ranked) — 84 more cut by --budget 6:
  1.30  Calls<0.76> list (5noobs-api/…/resolvers/lfg_post_resolver.ex)
  1.05  GraphqlCall<0.59> [file] 5noobs-web/src/routes/app/index.tsx
  1.03  GraphqlCall<0.59> [file] 5noobs-web/src/generated/graphql/graphql.ts
  1.01  GraphqlCall<0.59> [file] 5noobs-web/src/routes/app/lfgs/index.tsx
  1.01  GraphqlCall<0.59> [file] 5noobs-web/src/urql/cache-lfg.ts
  1.01  GraphqlCall<0.59> [file] 5noobs-web/src/components/app/lfg/lfg-list-page.tsx
```

One Elixir context function, and the answer crosses a repository boundary: the
GraphQL resolver that exposes it, then the frontend files whose queries reach that
resolver. Nothing in either repo declares this relationship — it's inferred by
matching TS GraphQL operations to Absinthe root fields.

Read the annotations rather than just the order:

- `Calls<0.76>` / `GraphqlCall<0.59>` — the edge kind, and the *propagated weight*
  after per-hop decay. A `[file]` hit is file-granular: the call sits outside every
  function (a module body, a `test` block), so the file is the most precise answer
  available.
- `6 of 90 … 84 more cut` — the budget is a display cap, not the size of the blast
  radius. It always says what it dropped.

## 2. Change a React component — who renders it?

```bash
ripple impact SearchInput --budget 4
```

```
blast radius of SearchInput — 4 of 125 hits (ranked) — 121 more cut by --budget 4:
  1.32  Calls<0.81> TeamInvitePage (5noobs-web/src/components/app/teams/team-invite-page.tsx)
  1.30  Calls<0.81> PlayersListPage (5noobs-web/src/components/app/players/players-list-page.tsx)
  1.26  Calls<0.81> TeamsListPage (5noobs-web/src/components/app/teams/teams-list-page.tsx)
  1.26  Calls<0.81> MessageComposePage (5noobs-web/src/components/app/messages/message-compose-page.tsx)
```

`<SearchInput />` is a call: rendering a component invokes it. An import edge would
only say a file imported the component — not that anything uses it.

## 3. Walk the other way: page → backend

```bash
ripple neighbors 5noobs-web/src/components/app/lfg/lfg-list-page.tsx --out --depth 2
```

```
neighbors of 5noobs-web/src/components/app/lfg/lfg-list-page.tsx [Module]
  GraphqlCall<0.90> list (5noobs-api/…/resolvers/lfg_post_resolver.ex)
    GraphqlCall<0.90> list (5noobs-api/…/resolvers/team_resolver.ex)
    GraphqlCall<0.90> me  (5noobs-api/…/resolvers/player_resolver.ex)
```

`impact` decides (weighted, ranked, budgeted); `neighbors` explains (raw traversal,
one hop at a time). Use `impact` to answer "what breaks", `neighbors` to see why.

## 4. Change a database table — who writes it?

```bash
ripple impact LfgPost --budget 4
```

```
no exact match for 'LfgPost'; matched 1 symbol(s) by qualified-name suffix: FiveNoobs.Lfgs.LfgPost
blast radius of LfgPost — 4 of 569 hits (ranked) — 565 more cut by --budget 4:
  1.11  DbQuery<0.65> [file] 5noobs-api/apps/five_noobs/priv/repo/seeds.exs
  1.11  DbQuery<0.65> get_post (5noobs-api/…/lfgs/lfg_posts.ex)
  1.11  DbQuery<0.65> create_post (5noobs-api/…/lfgs/lfg_posts.ex)
  1.10  DbQuery<0.65> update_post (5noobs-api/…/lfgs/lfg_posts.ex)
```

Two things to point out. Ripple found the symbol from the name a human types — a
module's Elixir name *is* its fully-qualified name, so the lookup widens to a
qualified-name suffix and says that it did. And the seed script is in the answer: it
writes the table from a top-level expression, inside no function at all.

## 5. Why is this connected?

```bash
# via the MCP server, the interface an agent uses
{"method":"tools/call","params":{"name":"explain_edge",
  "arguments":{"from":"lfg-list-page","to":"list"}}}
```

```json
{"edges":[{"kind":"GraphqlCall","confidence":0.9,"source":"Extracted",
           "direction":"from→to","site_line":0}]}
```

Every edge carries a confidence and a provenance. `source` is `Extracted` (read from
the AST), `LspVerified` (a language server confirmed or supplied it), or `CoChange`
(mined from git history, not from code). An answer you can't interrogate is an
answer you can't act on.

## 6. Rank a file by risk

```bash
ripple risk 5noobs-api/apps/five_noobs/lib/five_noobs/lfgs/lfg_posts.ex
```

```
5noobs-api/…/lfgs/lfg_posts.ex
  composite 0.71 | churn 0.95 bug 0.51 ownership 0.04 fanout 1.00
```

Churn, bug-density and ownership come from git; fanout comes from the graph. See the
honest list below for what this score is and isn't.

## 7. Check the graph against a language server

```bash
ripple lsp doctor                          # which servers are usable here
ripple eval --oracle lsp --sample 30       # agreement, per language
ripple impact <symbol> --verify lsp        # upgrade this neighbourhood, then answer
```

30-file sample, `tsgo` 7.0.0-dev for TS/TSX and `dexter` 0.7.1 for Elixir:

| language | identical caller sets | ripple-only | server-only |
|---|---|---|---|
| typescript | 34/35 (97.1%) | 5 | 0 |
| tsx | 32/35 (91.4%) | 3 | 0 |
| elixir | 94/95 (98.9%) | 1 | 0 |

This is a *comparison*, not a score: `dexter` and ripple are both tree-sitter-based,
so agreement proves neither correct — but disagreement localises a bug in one of
them, and it has found bugs on both sides. `--granularity file` compares at file
level instead, which is the honest way to judge the file-granular edges.

## What this does not do yet

Kept here so nobody rediscovers these as surprises:

- **Recall is ~10%, not ~90%.** Held out properly (co-change mined only from commits
  older than the test window), the graph links 7.1% of same-commit file pairs by
  static edges, 10.5% fused with co-change. Most real coupling is not syntactic.
  The earlier "~40%" figure was leakage — see [`12-dogfood-log.md`](12-dogfood-log.md).
- **Risk weights have never been fit to data.** They are hand-set constants, so
  ranking *within* one edge kind carries little signal — the four `DbQuery` hits above
  differ by 0.01. Treat the ordering across kinds as meaningful and the ordering
  within a kind as arbitrary.
- **Only root GraphQL fields join to resolvers.** A nested selection
  (`lfgPosts { author { … } }`) does not reach `author`'s resolver.
- **`import { a as b }` and `import * as ns` are unresolved.** The export side of
  aliasing works (including barrels); the import side does not.
- **Elixir multi-clause functions collapse to one symbol**, and arity is not
  distinguished. Every clause's span is kept, so "which symbol contains this line?"
  still works.
- **No Rust adapter for its own source**, so ripple cannot yet dogfood on itself for
  call resolution.
- **A duplicate call site produces a duplicate edge**, which inflates its weight in
  ranking. Visible as the same caller listed twice in `neighbors`.

## The point

A language server knows one language, in one workspace, that compiles. Ripple knows
*N* repos, *N* languages, their git history, and what to look at first. The
`filter_posts` answer above — Elixir function to React page to the test that covers
it — is not a query any single-language tool can answer.
