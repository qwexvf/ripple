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

The whitespace is the **join + ranking function** across all four, served through an incremental, budget-aware MCP interface. See [`02-gap.md`](docs/src/content/docs/design/02-gap.md).

## Design in one breath

A **language-agnostic core** (graph model, git overlay, risk scoring, impact/review queries, MCP server) sits above a **thin per-language adapter** seam. Adding a language touches only the adapter layer — the six layers above never change. Git-history signals work at *file* granularity, so a barely-supported new language still gets useful impact/review from day one. Built in **Rust** for safe parallelism and maintainability. See [`04-architecture.md`](docs/src/content/docs/design/04-architecture.md).

## Documentation

The prose lives in [`docs/`](docs/), an Astro site — `cd docs && bun install && bun dev`
to read it locally. Start at **Getting started**
([`docs/src/content/docs/getting-started.md`](docs/src/content/docs/getting-started.md)),
then the [CLI](docs/src/content/docs/reference/cli.md) and
[MCP](docs/src/content/docs/reference/mcp.md) references. The design docs below are the
same files, served under `/design/`.

### Document map

| Doc | What it covers |
|---|---|
| [`01-landscape.md`](docs/src/content/docs/design/01-landscape.md) | What existing tools actually do — verified against real code, binaries, and MCP schemas |
| [`02-gap.md`](docs/src/content/docs/design/02-gap.md) | The unfilled combination; possibilities and what's still not done |
| [`03-why-rust.md`](docs/src/content/docs/design/03-why-rust.md) | Rust merits: performance, maintainability vs C, readability, ecosystem, honest trade-offs |
| [`04-architecture.md`](docs/src/content/docs/design/04-architecture.md) | Clean layered architecture, normalized IR, `LanguageAdapter` trait, crate layout (with Rust skeletons) |
| [`05-language-support.md`](docs/src/content/docs/design/05-language-support.md) | The Tier system, `.scm` conventions, monorepo handling, the extensibility guarantee |
| [`06-risk-and-queries.md`](docs/src/content/docs/design/06-risk-and-queries.md) | Risk-scoring formula, `impact()` / `review_focus()`, budget-aware ranking, MCP tool schemas |
| [`07-ai-integration.md`](docs/src/content/docs/design/07-ai-integration.md) | Optimizing *decisions* not tokens; how LLM agents consume it; AI-native protocol angle |
| [`08-roadmap.md`](docs/src/content/docs/design/08-roadmap.md) | Phased delivery (v0 TypeScript → v1 git overlay → v2 impact/review MCP → v3 multi-language) |
| [`09-review-and-corrections.md`](docs/src/content/docs/design/09-review-and-corrections.md) | Architecture review + primary-source fact-check audit trail; every external claim traced, corrections & design fixes logged |
| [`10-cross-service-resolution.md`](docs/src/content/docs/design/10-cross-service-resolution.md) | Call-site ↔ route matching across services: `RouteKey` normalization, `FrameworkDetector` seam, matching + confidence, co-change safety net |
| [`11-lsp-integration.md`](docs/src/content/docs/design/11-lsp-integration.md) | LSP as the Tier-2 accuracy tier over the tree-sitter base: layering, reconciliation, how slow servers are kept off the critical path, provenance |
| [`12-dogfood-log.md`](docs/src/content/docs/design/12-dogfood-log.md) | What ripple got wrong when used for real, and what each mistake turned into — the log that has produced more fixes than the roadmap |
| [`13-engineering-review.md`](docs/src/content/docs/design/13-engineering-review.md) | 用語監査とロール別の評価（日本語）— which metric names overclaim, what the numbers survive re-measurement, and where the product gaps are |
| [`16-cross-service-plan.md`](docs/src/content/docs/design/16-cross-service-plan.md) | **Execution plan** for framework-agnostic cross-service resolution: `RouteKey` vocabulary, detector seam, generic linker, HTTP as the proof (issue #32) |
| [`15-two-tools-two-jobs.md`](docs/src/content/docs/design/15-two-tools-two-jobs.md) | Why tree-sitter **produces** the graph and LSP **grades** it — the measurement that killed the "accuracy tier" framing, and where the headroom actually is |
| [`14-demo.md`](docs/src/content/docs/design/14-demo.md) | **Walkthrough on a real full-stack app** (TS/React + Elixir, two repos, one graph), with actual output and an honest list of what it still gets wrong |
| [`v0-plan.md`](docs/src/content/docs/design/v0-plan.md) | **Execution plan** for the first slice (TypeScript, Tier 2): crate build order, the concrete TS reference-resolution algorithm, store spike, testing & done criteria |

## Status

**v0 complete** (TypeScript, Tier 2 — see [`v0-plan.md`](docs/src/content/docs/design/v0-plan.md)). Indexes a real 1078-file monorepo to 7.5k nodes / 4.7k edges in ~0.5s; warm incremental re-index ~0.2s.

```
cargo run -p ripple-cli -- index <path>         # build the graph → .ripple/graph.redb
cargo run -p ripple-cli -- neighbors <symbol>   # callers/importers (--in|--out, --depth N)
cargo run -p ripple-cli -- parse <file.ts>      # dump extracted symbols
cargo test && cargo clippy --all-targets        # golden fixtures + contract tests, lint-clean
```

Milestones: **M0** parse+symbols ✅ · **M1** index+neighbors (redb) ✅ · **M2** member/candidate resolution ✅ · **M3** incremental + tsconfig/workspace + perf ✅ (Samyama store deferred behind the `GraphStore` trait).

Next: **v1** — git overlay (churn / co-change / bug-density / ownership → risk scoring), where ripple passes the incumbents. See [`08-roadmap.md`](docs/src/content/docs/design/08-roadmap.md).
