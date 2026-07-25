# 08 — Roadmap

Each phase ships something that works on its own. Ordering is driven by the primary real-world target: tracing a **frontend page → GraphQL operation → backend resolver → DB** across a **cross-repo, cross-language** stack (TS frontend + Elixir/Absinthe backend, separate git repos — e.g. 5noobs). That target reshapes the plan: git-history signals (co-change) **cannot bridge separate repos**, so cross-service *static* resolution is promoted ahead of the git overlay.

## v0 — TypeScript substrate (Tier 2) ✅ complete

A correct, fast static graph for TypeScript: `ir`/`parse`/`lang`(+typescript)/`resolve`/`store`(redb)/`cli`. `ripple index` + `neighbors`, incremental via BLAKE3 extract cache. Indexes a real 1078-file repo in ~0.5s; warm re-index ~0.2s; `GraphStore` contract test; clippy-clean. See [`v0-plan.md`](v0-plan.md).

*Tail polish (non-blocking, fold in opportunistically):* import alias / namespace imports; function-scope (not file-wide) type map for member calls.

## v1 — Cross-service chain (the real target)

**Goal:** trace `Page.tsx → GraphQL operation → Absinthe field → resolver` across separate TS and Elixir repos. This is the use case git co-change *cannot* serve (cross-repo = no shared commits), so it must be solved statically.

- **Multi-root indexing:** index N repos into one graph (`ripple index <rootA> <rootB> …`). Lifts the current single-workspace limit ([`10`](10-cross-service-resolution.md)). Symbol identity already keys on module-relative path + repo tag.
- **Elixir adapter** (`crates/lang/adapters/elixir`, Tier 1–2): tree-sitter-elixir + `tags`/`imports`/`refs` `.scm`; module/def resolution. Proves the "add a language = one folder" guarantee on a non-TS language (5noobs backend ≈ 771 `.ex`).
- **GraphQL cross-service detector** (Tier 3, [`10`](10-cross-service-resolution.md)): consumer side extracts TS operations (`gql\`query GetUser\``/generated hooks); producer side extracts Absinthe `field :user` / `object`; match by **operation name** (handle Absinthe snake_case ↔ GraphQL camelCase) → `GraphqlCall` edge with confidence.
- **Ecto detector** (Tier 3): resolver → `Repo.*` / query → completes the chain to the DB layer.

**Acceptance:** on 5noobs (two repos), `neighbors <Page> --out` transitively reaches the Elixir resolver for the operation it uses; `explain_edge` shows the matched operation name + confidence.

## v2 — Impact & review + MCP (the decision layer)

**Goal:** the two decision-optimized queries, over MCP.

- `crates/query`: `impact(diff, budget)` (bounded weighted blast-radius diffusion, [`06`](06-risk-and-queries.md)), `review_focus(pr, budget)` (ranked hunks), budget-aware truncation with `{shown,total,reason}`.
- **Risk scoring** from the signals available now — `complexity` + static/cross-service `fanout` + `test_proximity`. Git-derived terms (churn, bug-density, ownership, co-change) are **absent until v3** and default to 0; the formula already degrades gracefully.
- `crates/mcp`: stdio + HTTP server exposing `impact, review_focus, neighbors, risk, explain_edge`.

**Acceptance:** an LLM agent hits a real PR via MCP; `review_focus` ranks the actually-risky hunks (structural + cross-service impact) in the top-k, within a token budget.

## v3 — Git overlay (moved later)

**Goal:** enrich risk with evolutionary signal — the within-repo differentiator. Deprioritized from its original v1 slot because the primary target is cross-repo, where co-change can't bridge the boundary; it remains high-value *within* each repo.

- `crates/overlay`: `git2` mining → `churn`, `bug_density`, `ownership` per file; `ChangesWith` edges with `coupling_score` from commit co-occurrence; feed `RiskScores.composite`.
- Optional cross-repo correlation via shared ticket IDs / PR links (weak logical-coupling signal where separate repos share a work item).

**Acceptance:** top-risk files match hotspot intuition; co-change surfaces a coupling with no static edge (config↔consumer, test↔fixture) *within a repo*.

## v4 — Breadth & scale

- **LSP as the accuracy tier** ([`11`](11-lsp-integration.md)): take Tier-2 call resolution from language servers where one is configured, keep tree-sitter as the always-available base. Slow servers are handled by never blocking a query on one — budgeted verification, content-hash cache, churn-ordered background warm. Makes "add a language" mostly config, and gives the call graph its first real precision measurement (`eval --oracle lsp`).
- **More languages:** Gleam, Python, Go — each a folder under `adapters/`, diff confined to `crates/lang/` (guardrail test). With the LSP tier, a new language needs `tags.scm` plus a server entry, not hand-written call resolution.
- **More detectors:** HTTP/REST, pub-sub, gRPC cross-service edges.
- **Incremental daemon:** file-watch + resident in-RAM graph (LSP-style) — removes the CLI cold-load cost; warm queries for an agent session.
- **Scale escape hatch:** for graphs too large for RAM, swap `store` to a disk-resident backend (Samyama/LadybugDB) behind the unchanged `GraphStore` trait — [`04`](04-architecture.md#store).

## Sequencing rationale

- **Reordered around the real target.** The original plan led with the git overlay (v1); the cross-repo Elixir/GraphQL use case makes cross-service static resolution the higher-value next step, and makes co-change ineffective across the boundary — so git moves to v3.
- **v1 proves multi-language + cross-service**, the two hardest architectural claims, on a real stack.
- **v2 delivers the decision layer** even with git signals still absent — risk degrades gracefully.
- **v3 adds the within-repo differentiator** (git fusion) once the cross-repo spine exists.

## Non-goals (restated)

- Not 158-language coverage; not deep whole-program type inference; not the fastest raw index. The scarce thing is the fusion of static cross-service resolution with (later) git signal — see [`02-gap.md`](02-gap.md).
