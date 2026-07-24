# 02 — The gap: what's possible and what's still missing

## The unfilled combination

Every ingredient below is proven and shipping *somewhere*. Nobody has fused them:

```
   function-level static graph      (Sourcegraph, code-graph-mcp, codebase-memory)
 ∪ git co-change / logical coupling  (CodeScene, code-maat)     ← different world
 → scored per node by evolutionary risk (churn × complexity × bug-density × ownership)  (CodeScene, JIT research)
 → served through an incremental, budget-aware MCP interface     (code-graph-mcp, codebase-memory)
 → answering two diff-scoped queries:
     impact(diff)      → risk-ranked blast radius
     review_focus(pr)  → risk-ranked hunks to review first, each with impact + reason
```

## Four concrete gaps

### 1. Static graph and git history live in separate worlds

Static-graph tools know *structural* reachability but are blind to co-change. They miss the coupling that causes most surprise breakage — **files that always change together with no call edge between them**:

- config ↔ its consumer
- schema/serializer ↔ its deserializer
- test ↔ fixture
- a constant ↔ every switch that branches on it

Git-mining tools (CodeScene, code-maat) see this coupling but have **no call/type graph**, so they can't say *which function* or trace a symbol. The only place both are combined is academic (CLIO, HIST) — never productized.

> **A blast radius that unions `static dependents ∪ statistical co-change dependents` does not exist as a shipping tool.** codebase-memory-mcp has *both signals in its graph* (`CALLS` and `FILE_CHANGES_WITH`) but no query that fuses and ranks them.

### 2. Impact is binary; risk is a separate silo

Nx / Bazel / Turbo / TAP give an *affected set* with no ordering. CodeScene gives a *risk score* but no function-level graph and no agent interface. **Nothing outputs a risk-ranked blast radius** — dependents sorted by `churn × complexity × historical bug-density × ownership-thinness × coupling-strength`.

### 3. AI-native tools optimize tokens, not decisions

The 2025–2026 MCP graph servers nailed budget-aware output (Merkle-incremental indexing, RRF retrieval, 100×+ token compression). Some expose "impact analysis." But their risk is at best a coarse HIGH/MED/LOW from **structural fan-out only**. None ingest git history, none do co-change, none do PR-diff-scoped review targeting. code-graph-mcp is the closest artifact and still misses the git signal and review-targeting entirely.

### 4. Review-targeting is not an agent primitive

CodeScene does PR delta risk — proprietary, human dashboard / Gerrit vote. **No MCP server takes a PR diff and returns, within a token budget, a ranked list of "review these hunks first, here's the downstream blast radius, here's *why* each is risky (churn + coupling + complexity + owner absent)"** — which is exactly the output an LLM reviewer needs.

## What's newly possible (the opportunity)

Because the substrate already exists (tree-sitter everywhere, codebase-memory-style graphs, git is right there), the differentiator is **not another index** — it's the **join + ranking function** and the **agent-native surface**:

- **Fused blast radius.** Union static reachability with co-change coupling, weight edges by confidence and decay by distance, rank by node risk. Surfaces breakage that pure static analysis structurally cannot see.
- **Risk-ranked review.** Turn "LGTM" into "these 3 hunks, in this order, here's why." Uses the *absence of an expected co-change* as a bug smell (CodeScene's best idea, made queryable by an agent).
- **Day-one value for new languages.** Because co-change/churn/risk are *file-level*, a language with only shallow static support still gets useful impact/review. (See [`05-language-support.md`](05-language-support.md).)
- **Decision-optimized output.** Budget-aware, but the thing being ranked and compressed carries a *risk* signal, not just structural proximity. (See [`07-ai-integration.md`](07-ai-integration.md).)

## What is explicitly *not* the differentiator

To avoid the multi-year trap that codebase-memory-mcp and Sourcegraph paid for:

- **Not 158-language coverage.** Start with the languages you use.
- **Not deep whole-program type inference.** Shallow resolution + git signal beats deep-but-narrow for the impact/review use case.
- **Not the fastest raw index.** codebase-memory already proves the substrate is fast; language/DB choice isn't the bottleneck (see [`03-why-rust.md`](03-why-rust.md)).

The scarce thing is the fusion. Build that; reuse everything else.
