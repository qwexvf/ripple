---
title: "06 — Risk scoring & query API"
description: "The risk-scoring formula, impact() and review_focus(), budget-aware ranking, MCP tool schemas"
sidebar:
  label: "06 — Risk & queries"
  order: 6
---
The differentiator is the **join + ranking function**, exposed as two diff-scoped queries. Everything here operates on the IR + git overlay — **language-agnostic**, written once, works for every language and tier.

## Risk score

Each node carries a `RiskScores` struct filled by the overlay at index time (defaults to zero pre-v1):

```rust
// crate: ir
pub struct RiskScores {
    pub churn: f32,          // change frequency (git log, decayed toward recent)
    pub complexity: f32,     // from AST shape (language-agnostic; see caveat below)
    pub bug_density: f32,    // heuristic: fraction of touching commits that look like fixes
    pub ownership: f32,      // author dispersion; low = bus-factor risk
    pub fanout: f32,         // |static dependents ∪ co-change dependents|
    pub test_proximity: f32, // Tests-edge linkage; >0 lowers risk (NOT line coverage — see below)
    pub composite: f32,      // the blended score below
}
```

### Formula

```
risk(n) =  ( w_churn·churn
           + w_cx·complexity
           + w_bug·bug_density
           + w_own·(1 − ownership)      // thinner ownership → higher risk
           + w_fan·fanout )
           / (1 + w_cov·test_proximity)  // test linkage dampens risk
```

- Each raw term is **normalized to [0,1]** across the repo (percentile rank, robust to outliers) before weighting — so the score is comparable within a repo, not an absolute. **Cost note:** percentile rank needs the whole distribution, so it recomputes on the *commit* trigger (the git overlay's cadence), not per edit; for very large repos maintain approximate quantiles (t-digest) instead of a full re-rank.
- Weights `w_*` were **fitted, not chosen** (#19), by `eval --risk` on the two corpora with enough held-out fix history to score — this repo, and a two-root Elixir + TypeScript stack:

  | weights (churn, bug, own, fanout) | Elixir+TS stack | this repo |
  |---|---|---|
  | `0.4, 0.4, 0.2, 0.4` (the old guess) | 0.94× | 2.80× |
  | **`0, 0, 0, 1` (shipped)** | **1.95×** | **2.80×** |
  | `0.5, 0, 0, 1` | 1.30× | 3.20× |

  Fanout-only is the only vector that beats the old blend on one corpus without losing on the other — the old one was *worse than no ranking at all* (0.94×) on the larger corpus. This is not "drop the git signal": `fanout` is the fusion, static dependents ∪ co-change dependents, so history still decides most of it. What it drops is churn / bug-density / ownership as blend *inputs*. They are still mined, still printed by `risk`, and still reachable through `--weights`, because the two corpora disagree about which of them matters (churn wins on one, ownership on the other) and two corpora cannot settle that. The literature (Nagappan/Ball 2005, relative churn discriminating fault-prone binaries at ~89%) says these terms carry signal; our measurement says our *blend* of them did not.
- **Signal provenance & honesty:**
  - `churn`, `bug_density`, `ownership` ← `git2` log/blame mining.
  - `bug_density` is a **heuristic**: fraction of touching commits whose message matches fix/revert patterns, refined by issue links where available. Message-based fix detection is noisy (many repos don't follow conventional commits) — treat as a signal, not ground truth.
  - `complexity` ← AST. Approximable language-agnostically (rust-code-analysis, arborist do this over tree-sitter) **but not zero-config**: each grammar names decision nodes differently (`if_statement` vs `if_expression`, …), and a faithful McCabe count must also add short-circuit `&&`/`||`, ternaries, `case`, and `catch`/`except`. Budget a **per-grammar decision-node map** per adapter; "count branch/loop nodes" alone undercounts.
  - `fanout` ← graph: **static dependents ∪ co-change dependents** (the fusion), minus test callers. Fanout asks "how much breaks if this changes"; a test breaking is how you find out, not damage, and counting it ranked a well-tested symbol above an untested one. A test is whatever has an outgoing `Tests` edge, so the rule stays language-blind.
  - `test_proximity` ← `Tests`/`TestsFile` edges. This is "a test references this symbol," **not line coverage** — do not present it as coverage. Still unpopulated and still absent from `blend`; the `Tests` edges it would read now exist (below).
  - A resolution that **crosses repositories** is multiplied by `CROSS_ROOT` (0.85): an import resolved through a package name is pinned exactly as well as an in-repo one, but the premise underneath it — that these two working trees are one program — is not something the syntax says. A consumer normally resolves a published artifact, so the file on disk is the right symbol at a version nobody checked. Cross-*service* edges are not discounted: there the repo boundary is the designed case and the wire contract is the evidence.
  - `Tests` edges are built by `resolve::link_tests` from calls that leave the test side of a repo, at `0.8 ×` the confidence of the call they rest on — the call is the evidence, "a call from a test exercises this" is the inference on top of it, and fixtures and `support/` helpers are why it isn't 1.0. Every such edge duplicates the endpoints of an existing call, so structural risk counts no new dependent and no ranking moves.
  - **Whose tests?** `tests_known` is asked per indexed root, not per index: in a multi-root graph one repository's tests used to answer for a repository that has none.
  - **No tests ripple can see?** `review` says nothing rather than flagging every row. A flag that is true for 100% of symbols carries no information (#36), so `untested` is suppressed entirely when the graph holds no `Tests` edge; `--json` reports `untested_known: false` so a client can tell "nothing untested" from "cannot tell".
  - **No git history?** (non-git project, shallow clone) → `churn`/`bug_density`/`ownership`/co-change degrade to 0; risk falls back to `complexity + static fanout` only. State this degradation, don't crash.

### The fusion, precisely

`fanout` is where static and git worlds join. Blast radius is a **weighted reachability** over a graph that unions two edge sources:

```
edge weight  w(e) = confidence(e) · kind_weight(e)
  - static:     Calls / References / Imports / Implements   (kind_weight ~1.0, high)
  - co-change:  ChangesWith                                 (kind_weight = coupling_score, the git signal)
```

**Do not compute this as a sum over paths.** Real graphs have cycles and exponentially many paths, so a path-enumeration `Σ Π` is both computationally explosive and mathematically divergent. Compute it instead as **bounded iterative propagation** from the changed seed set — a personalized-PageRank / shortest-cheapest-path style diffusion:

```
impact(seed) := 1.0
iterate (or BFS cheapest-first) with per-hop decay δ:
    impact(n) = max over in-edges (m→n) of  impact(m) · w(m→n) · δ
stop at a hop cap OR a distinct-node budget
```

Using **max-over-incoming** (not sum) keeps it convergent and gives each node a single dominant propagation path (the one reported as `path` in the output). This maps directly onto mnestic's verified **`BudgetedTraversal`** primitive — *"expands cheapest-first from a set of seeds, over non-negative weights, under a global distinct-node budget plus optional cost ceiling and exact hop bound"* — where cost = `−log(w·δ)` so cheapest-first = highest-impact-first. So the store gives us the algorithm; we supply the weights. (Fallback store `redb`: implement the same bounded diffusion by hand with a binary heap.)

A file with no call edge to the change but a high `coupling_score` still lands in the blast radius — the breakage static analysis structurally cannot see (config↔consumer, schema↔serializer). Signature-changing edits propagate strongly to callers; internal-only edits propagate weakly (edit-kind scales the seed weight).

**Determinism:** blast-radius and ranking output must be reproducible (agents/CI compare runs). Use a stable sort with a total tie-break key (`impact_weight`, then `risk`, then `symbol_id`) and a fixed reduction order, so parallel/float non-determinism can't reorder results.

**Co-change mining cost:** naive pairwise co-change over full history is O(commits × files-per-commit²). Bound it: a **sliding history window** (recent N commits, configurable), a minimum-shared-commits threshold before an edge is created, and **incremental update** on each new commit (add the commit's file pairs; age out the window) rather than a full re-scan.

## Query 1: `impact(diff, budget)`

**Input:** a diff (or a set of changed symbols/files), a token budget, optional path scope.
**Output:** risk-ranked blast radius.

```jsonc
// impact(diff, budget=2000)
{
  "changed": ["src/auth/token.ts:verifyToken"],
  "blast_radius": [
    {
      "symbol": "src/api/middleware.ts:requireAuth",
      "path": "verifyToken → requireAuth",     // how the impact reaches it
      "impact_weight": 0.91,
      "risk": 0.78,
      "signals": { "churn": "high", "test_proximity": "none", "via": "Calls" },
      "why": "direct caller; no tests; changed 14× last quarter"
    },
    {
      "symbol": "config/auth.yaml",
      "impact_weight": 0.63,
      "risk": 0.55,
      "signals": { "via": "ChangesWith", "coupling_score": 0.7 },
      "why": "no code edge, but co-changed with token.ts in 7/10 past commits"
    }
  ],
  "untested_in_radius": ["src/api/middleware.ts:requireAuth"],   // impacted AND no test edge
  "truncated": { "shown": 12, "total": 47, "reason": "budget" }   // never silent
}
```

Ranking key = `impact_weight · risk`. The list is cut to `budget` and the truncation is **reported, never silent** (a silent cap reads as "covered everything" when it didn't).

## Query 2: `review_focus(pr, budget)`

The one no MCP server offers. **Input:** a PR (base..head), a budget. **Output:** the hunks to review first, ordered, each with downstream impact and a reason.

```jsonc
// review_focus(pr, budget=3000)
{
  "focus": [
    {
      "hunk": "src/billing/charge.ts:L40-72",
      "review_priority": 0.94,
      "downstream": ["invoice.ts:finalize", "webhook.ts:onPaid"],  // blast radius of THIS hunk
      "reasons": [
        "highest-churn file in PR (bug-density 0.4)",
        "changes a signature with 6 callers",
        "author has never touched this file (ownership 0.1)"
      ]
    }
  ],
  "missing_cochange": [
    {
      "expected": "test/billing/charge.test.ts",
      "why": "co-changed with charge.ts in 9/11 past commits but absent from this PR",
      "smell": "likely-missing-test-update"
    }
  ],
  "low_risk": ["docs/*", "src/util/format.ts:pad"],   // safe to skim
  "truncated": { "shown": 5, "total": 9, "reason": "budget" }
}
```

`missing_cochange` implements CodeScene's best idea — *the absence of an expected co-change is a bug smell* — as an agent-queryable primitive.

`review_priority` = `(1 + risk) × (1 + ln(1 + downstream weight)) × (1 + ln(1 + changed lines) × (½ + ½ · rewritten share))`. Three properties it has to hold at once, learned the hard way:

- **Reach is logarithmic, not linear.** Multiplied raw, a one-line edit to a hub outranked every real change in the diff: on ripple's own `v0.1.2..v0.2.0`, `registry` — a single `Box::new(gleam::Adapter::new()),` — ranked first at 34.4 on 46 dependents, while the release's largest new function ranked 11th.
- **The change itself is a term.** Every other signal (dependents, churn, bug-density, ownership) looks backwards, so code the diff *adds* has no history and scores at the floor — exactly the code a reviewer opens first.
- **A test is not reach.** `down_weight` skips hits on the test side, the same rule `score_structure` applies to `fanout`, so the two counters agree. The hit stays in `impact`'s own answer — "your test will break" is worth knowing, it just isn't blast radius.
- **Every definition site counts.** `changed_lines` and the rewritten share read all of a symbol's spans; a multi-clause Elixir function edited in its second clause is still that function, not its module.
- **Size and share both matter.** 60 lines of a 60-line function is a rewrite; 60 lines of a 2000-line file is a patch. Hence the share factor multiplying the log of the count.

These weights are argued, not fitted — the fitting is [#19](https://github.com/qwexvf/ripple/issues/19), and until it lands this ranking is defensible rather than measured.

## Budget-aware output

- **Rank, then fill to budget.** Highest `impact_weight · risk` first; stop at the token budget.
- **Token-aware expansion.** Return `symbol` ids by default; expand to code snippets only for the top-k or on request (code-graph-mcp's technique), so the caller controls verbosity.
- **RRF where multiple signals rank.** When static proximity and co-change disagree on ordering, fuse via Reciprocal Rank Fusion rather than trusting one.
- **Always report truncation.** Any cap emits `{shown, total, reason}`.

## MCP tool surface

```
impact         { diff | symbols, budget?, scope? }        → blast_radius (above)
review_focus   { pr: {base, head}, budget? }              → focus + missing_cochange
neighbors      { symbol, edge_kinds?, depth? }            → raw traversal (escape hatch)
risk           { symbol | file }                          → RiskScores + provenance
explain_edge   { src, dst }                               → why this edge exists (confidence, site, via)
```

`impact` and `review_focus` are the headline decision-optimized tools. The rest are lower-level escape hatches so an agent can drill in without leaving the server. How agents actually use these: [`07-ai-integration.md`](07-ai-integration.md).
