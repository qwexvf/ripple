# ripple — an AI-native code impact & review-targeting engine

> Working codename: **`ripple`** (a change ripples outward through the graph — its blast radius). Final name TBD.

## The two questions

Everything here exists to answer two questions that AI-assisted development actually needs — and that no shipping tool answers well:

1. **Blast radius** — "If I change `X`, what is likely to break?" — a *risk-ranked* set of impacted code, not a flat reachability dump.
2. **Review targeting** — "For this PR, where do I look first?" — the risky hunks ordered by how likely they are to hide a defect, each with its downstream impact and a reason.

## Why it doesn't exist yet

Four ingredients are each proven in isolation, but nobody has fused them:

| Ingredient | Who has it | Who's missing it |
|---|---|---|
| Function-level static graph | Sourcegraph, graphify, codebase-memory-mcp, code-graph-mcp | — |
| Git co-change / logical coupling | CodeScene, code-maat, *codebase-memory-mcp (partial)* | every static-graph tool |
| Risk scoring (churn × complexity × bug-density × ownership) | CodeScene, JIT-defect research | every graph tool, every MCP server |
| Budget-aware LLM/MCP output | code-graph-mcp, codebase-memory-mcp | the risk-scoring tools |

The whitespace is the **join + ranking function** across all four, served through an incremental, budget-aware MCP interface. See [`docs/02-gap.md`](docs/02-gap.md).

## Design in one breath

A **language-agnostic core** (graph model, git overlay, risk scoring, impact/review queries, MCP server) sits above a **thin per-language adapter** seam. Adding a language touches only the adapter layer — the six layers above never change. Git-history signals work at *file* granularity, so a barely-supported new language still gets useful impact/review from day one. Built in **Rust** for safe parallelism and maintainability. See [`docs/04-architecture.md`](docs/04-architecture.md).

## Document map

| Doc | What it covers |
|---|---|
| [`docs/01-landscape.md`](docs/01-landscape.md) | What existing tools actually do — verified against real code, binaries, and MCP schemas |
| [`docs/02-gap.md`](docs/02-gap.md) | The unfilled combination; possibilities and what's still not done |
| [`docs/03-why-rust.md`](docs/03-why-rust.md) | Rust merits: performance, maintainability vs C, readability, ecosystem, honest trade-offs |
| [`docs/04-architecture.md`](docs/04-architecture.md) | Clean layered architecture, normalized IR, `LanguageAdapter` trait, crate layout (with Rust skeletons) |
| [`docs/05-language-support.md`](docs/05-language-support.md) | The Tier system, `.scm` conventions, monorepo handling, the extensibility guarantee |
| [`docs/06-risk-and-queries.md`](docs/06-risk-and-queries.md) | Risk-scoring formula, `impact()` / `review_focus()`, budget-aware ranking, MCP tool schemas |
| [`docs/07-ai-integration.md`](docs/07-ai-integration.md) | Optimizing *decisions* not tokens; how LLM agents consume it; AI-native protocol angle |
| [`docs/08-roadmap.md`](docs/08-roadmap.md) | Phased delivery (v0 TypeScript → v1 git overlay → v2 impact/review MCP → v3 multi-language) |
| [`docs/09-review-and-corrections.md`](docs/09-review-and-corrections.md) | Architecture review + primary-source fact-check audit trail; every external claim traced, corrections & design fixes logged |
| [`docs/10-cross-service-resolution.md`](docs/10-cross-service-resolution.md) | Call-site ↔ route matching across services: `RouteKey` normalization, `FrameworkDetector` seam, matching + confidence, co-change safety net |
| [`docs/v0-plan.md`](docs/v0-plan.md) | **Execution plan** for the first slice (TypeScript, Tier 2): crate build order, the concrete TS reference-resolution algorithm, store spike, testing & done criteria |

## Status

**v0 complete** (TypeScript, Tier 2 — see [`docs/v0-plan.md`](docs/v0-plan.md)). Indexes a real 1078-file monorepo to 7.5k nodes / 4.7k edges in ~0.5s; warm incremental re-index ~0.2s.

```
cargo run -p ripple-cli -- index <path>         # build the graph → .ripple/graph.redb
cargo run -p ripple-cli -- neighbors <symbol>   # callers/importers (--in|--out, --depth N)
cargo run -p ripple-cli -- parse <file.ts>      # dump extracted symbols
cargo test && cargo clippy --all-targets        # golden fixtures + contract tests, lint-clean
```

Milestones: **M0** parse+symbols ✅ · **M1** index+neighbors (redb) ✅ · **M2** member/candidate resolution ✅ · **M3** incremental + tsconfig/workspace + perf ✅ (Samyama store deferred behind the `GraphStore` trait).

Next: **v1** — git overlay (churn / co-change / bug-density / ownership → risk scoring), where ripple passes the incumbents. See [`docs/08-roadmap.md`](docs/08-roadmap.md).
