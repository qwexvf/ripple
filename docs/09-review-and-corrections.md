# 09 — Architecture review & fact-check audit

An architect/systems-engineer review of the spec, plus a primary-source fact-check of every load-bearing external claim. Purpose: nothing in this spec should be assumed or guessed — each factual claim is traced to a real artifact (a repo, a binary on disk, a live MCP schema, a paper, a license file). This doc is the audit trail; corrections have been applied to the other files.

Method: three independent verification passes against primary sources — (1) embedded DBs, (2) the two headline tools re-checked against the actual clone + installed binary, (3) competitors + Rust crates + cited numbers. Verdicts below are VERIFIED / corrected / ruled-out with evidence.

## Part A — Fact-check verdicts

### Verified against the actual artifact on this machine
- **codebase-memory-mcp is C/C++, not Rust.** Binary `~/.local/bin/codebase-memory-mcp`: `.comment` = GCC only, zero Rust runtime markers, links `libstdc++`. README confirms "pure C, zero dependencies." The `Cargo.toml`/`nats.rs`/`go.mod` strings are language-*detection* patterns (they sit beside `composer.json`, `requirements.txt`). Its edge types (`FILE_CHANGES_WITH{co_changes,coupling_score}`, `CALLS{confidence,strategy,candidates}`, `HTTP_CALLS`, …) appear verbatim in the binary and match the live `get_graph_schema`. Claims of 158 languages, 28M LOC / 3 min, 15 MCP tools, openCypher-read-subset — all verified against the README.
- **graphify (fresh clone, HEAD 0.9.25) is git-blind for churn/co-change.** Source grep: no `git log`/`rev-list`/`blame`/co-change/churn/hotspot logic. Git is used only for a diff-trigger, a merge driver, and `gh` PR data — confirmed. `affected` is reverse-BFS over incoming edges (`affected.py:145`); `compute_pr_impact` returns `(communities, node_count)` (`prs.py:252`); query matching is substring/trigram, not embeddings (`serve.py:951`); MCP exposes exactly 10 tools. All as stated.

### Verified against primary docs/papers
- SCIP (LSIF successor, protobuf), stack-graphs (scope-graph name binding, file-isolated subgraphs), CodeQL (relational AST+CFG+dataflow, taint), Glean (RocksDB + Angle), Nx affected (project DAG + lockfile diff), Bazel `rdeps`, Google TAP (Blaze graph), code-maat (% shared commits), code-graph-mcp (19 langs, structural-only risk buckets, BLAKE3 Merkle, **no git signal**). All VERIFIED.
- **CodeScene "Absence of Expected Change"** — the bug-smell feature the spec leans on — is real and documented (fires when a temporal-coupling cluster ≥ threshold, default 80%, is broken; "may be a sign of omission and a potential bug"). VERIFIED.
- mnestic: fork of Cozo, pure Rust, embedded; **`BudgetedTraversal`** exists with semantics matching our budget-aware blast radius (cheapest-first, distinct-node budget, cost ceiling, hop bound); bitemporal `:as_of`; hybrid retrieval (RRF+MMR over vector/FTS/graph); Datalog + Cypher(alpha) + 12 algorithms; v0.13.0, MPL-2.0, last commit 2026-07. All VERIFIED.

### Corrections applied
| Item | Was | Corrected to | Applied in |
|---|---|---|---|
| KùzuDB timing | "acquired by Apple in 2026" | acquired late 2025 (repo archived Oct 10 2025), disclosed Feb 2026 | 03, 04 |
| `bincode` | listed as a serialization option | **removed** — unmaintained since Dec-2025 incident; last release is a non-compiling tombstone. Use `rkyv`/`postcard` | 03 |
| `redb` maturity | "1.0+" | v4.x (2026-04), Apache-2.0, near-zero deps | 03, 04 |
| Nagappan/Ball 89% | "predicts defect density ~89%" | "discriminated fault-prone binaries at ~89% accuracy" (classification, not regression) | 01, 06 |
| Complexity metric | "count branch/loop nodes, language-agnostic" | true but needs a **per-grammar decision-node map** + short-circuit/ternary/catch counting | 06 |
| Samyama | (missing) | investigated → **promoted to primary store** (Rust, Apache-2.0, v1.1, LDBC-certified, company-backed VaidhyaMegha, OpenCypher/HNSW/14 algos, 74M nodes/1B edges). Has 1-axis MVCC time-travel (an earlier note wrongly said "lacks bitemporal"); mnestic keeps 2-axis (valid+tx) + built-in budgeted traversal | 03, 04, 08 |
| Store primary | mnestic | **flipped to Samyama.** mnestic's decisive edge (built-in budgeted traversal) was neutralized when finding #1 made blast radius store-agnostic; that left its pre-1.0 + solo-maintainer risk unjustified. Samyama removes both (post-1.0, company-backed). mnestic retained as alternative; RocksDB/C++ tradeoff accepted (redb path preserves pure-Rust single binary) | 03, 04, 08 |

### Confirmed (not guesses)
- **`rmcp`** IS the official `modelcontextprotocol/rust-sdk` crate — not invented.
- `tree-sitter`, `rayon`, `git2`, `petgraph`, `serde`, `rkyv` — all real, maintained, fit their stated roles as of 2026.

### Ruled out / non-existent
- **KùzuDB** as a build target (OSS dead). **MinnsDB** — no primary source found; treat as nonexistent. **bincode** — dead.

## Part B — Architect design findings (applied)

Design-level issues found in review and the fix now reflected in the spec:

| # | Finding | Severity | Fix (where) |
|---|---|---|---|
| 1 | `impact_weight` defined as sum-over-paths (`Σ Π`) — explodes on cycles, diverges | 🔴 | rewritten as bounded iterative propagation (max-over-incoming, per-hop decay, hop/budget cap); maps to mnestic `BudgetedTraversal` (06) |
| 2 | `SymbolId` fragile: file-path-in-id breaks on rename; overloads collide | 🔴 | identity rules added (signature discriminator, module-relative path, `git log --follow`); path is metadata not identity (04) |
| 3 | `coverage` overclaims line coverage | 🟠 | renamed `test_proximity`, "not line coverage" stated (06) |
| 4 | `bug_density` presented as fact | 🟠 | flagged as noisy message-based heuristic (06) |
| 5 | cross-service edge resolution hand-waved | 🟠 | to be designed; call-site↔route matching is hard, low-confidence (noted 06; Tier-3 in 05) |
| 6 | co-change mining cost O(commits × files²), no incremental | 🟠 | sliding window + threshold + incremental update (06) |
| 7 | percentile normalization recompute cost | 🟠 | commit-cadence recompute; t-digest for large repos (06) |
| 8 | AMBIGUOUS-call confidence undefined | 🟠 | `≈1/N` across candidates, type-narrowed higher (04) |
| 9 | git-absent projects not handled | 🟡 | graceful degradation to complexity + static fanout (06) |
| 10 | ranking non-determinism | 🟡 | stable sort + total tie-break + fixed reduction (06) |
| 11 | daemon reader/writer isolation unspecified | 🟡 | atomic `Arc<Graph>` swap; mnestic MVCC (04) |
| 12 | "RAM always fastest" vs disk-fallback tension | 🟡 | crossover stated explicitly (03, 04) |
| 13 | no RAM sizing model | 🟡 | rough sizing + scoped queries (04) |

## Part C — Design conclusions affirmed by the review

- The **layered IR decoupling** and the **Tier system × file-granularity git overlay** synergy hold up — no fact undermined the core thesis; the whitespace in [`02-gap.md`](02-gap.md) is real and unfilled.
- The **store choice flipped to Samyama** on review: once blast radius became store-agnostic (#1), mnestic's built-in budgeted traversal stopped being decisive, and Samyama's maturity (post-1.0, LDBC-certified, company-backed) beats mnestic's pre-1.0/solo-maintainer risk for a foundational dependency. mnestic is kept as an alternative (2-axis bitemporal + budgeted traversal) pending a v0 spike. The `GraphStore` trait + shipping two impls (Samyama + redb) in v0 keep the decision measured and reversible.
- The plan is **fact-based, not assumed** — every external claim now traces to a primary source, and the internal algorithm/identity bugs found in review are fixed in the spec rather than left implicit.

## Open items still requiring first-hand design (not yet fully specified)
1. ~~Cross-service call-site ↔ route matching (finding #5) — algorithm + confidence model.~~ **Designed** in [`10-cross-service-resolution.md`](10-cross-service-resolution.md) (RouteKey normalization, FrameworkDetector seam, matching + confidence, co-change fallback). Remaining: per-framework detector implementations (v3).
2. ~~Samyama-vs-mnestic-vs-redb spike in v0~~ **Spiked (2026-07).** Finding: `samyama-sdk` (the embedded client) is **not published to crates.io** — only `samyama-graph-algorithms` and `samyama-optimization` sub-crates are. Embedding Samyama would need a git dependency on the full RocksDB/C++ repo — a heavy integration, not a v0 wire-up. **Decision:** v0 ships on `RedbStore` (pure-Rust, working, zero external risk); `SamyamaStore` is deferred to a dedicated integration behind the unchanged `GraphStore` trait. Exactly the hedge the trait + redb-baseline were designed for — the store choice stays reversible and costs nothing to defer.
3. Rename/move reconciliation via `git log --follow` — concrete integration into incremental identity.
4. Exact `kind_weight` / decay / weight defaults — to be tuned against a labelled repo, not guessed.
5. **Tier-2 reference resolution** (the thinnest core piece) — **designed for TS** in [`v0-plan.md`](v0-plan.md) (scope-tree name resolution + shallow member-call resolution with `1/N` candidate edges, honest confidence ladder). Other languages inherit the pattern per adapter.

~~Still genuinely open: **evaluation methodology**~~ **Built** — `ripple eval` does historical co-change prediction (static-only baseline is leakage-free). On 5noobs-web: static edges alone link ~6.5% of same-commit file pairs, co-change lifts recall to ~40% — the doc-02 gap, measured. (Holdout for a leakage-free co-change number is a follow-up.)

## Design-review round 2 (post-v2, as-built)

A second review (correctness + readability + extensibility + MCP-usability) drove these changes:

**Correctness fixes:** percentile normalization mapped the minimum to a tie-fraction (inflated `bug_density` for zero-bug files) → now `count(x<v)/n`; diff hunks included 3 context lines → `context_lines(0)`; pure-deletion hunks emitted a bogus range → skipped; merge/root commits skewed mining → skipped; co-change/untested/missing outputs now sorted (determinism); cross-service `enclosing` now picks the innermost span.

**Extensibility (making "add a language" real):**
- `is_exported` and method name-qualification were hard-coded to TS node kinds in `parse` → moved to `LanguageAdapter::is_exported` / `qualified_name` (TS + Elixir `def`/`defp` implemented; sane defaults otherwise). `parse` no longer knows any language.
- Elixir alias→FQN resolution moved to extraction time (`lang::cross`), so `resolve`'s cross-service linker is a pure FQN/key join with **no** language-specific logic.
- **Honest Tier boundary (design decision):** Tier 0–1 (symbols, git overlay, imports) are genuinely generic — a new language is a folder + `.scm` + a registry line and gets same-day value. Tier 2 (call resolution) and Tier 3 (cross-service) are **per-language/framework by nature** (Absinthe/Ecto/urql-codegen are specific concepts) and are added as a small per-language implementation, not free data. A fully generic `Provides/Consumes` vocabulary was considered and **declined** — GraphQL's 3-way op→field→resolver shape doesn't fit a 2-key join, and speculative generalization before a second Tier-2 language would be premature. Refactor when a concrete second case arrives.

**MCP agent-usability:** the server now indexes-if-missing on startup, and exposes `search` (find symbols/paths by substring — the discovery entry point), `explain_edge` (edge provenance: kind/confidence/site), and `reindex` (rebuild after edits — fixes staleness), with bounded output (`{shown,total}`) so results fit an agent's token budget. Verified end-to-end over stdio JSON-RPC incl. error cases.

**Still open:** rename reconciliation (`git log --follow`); weight/decay tuning on labelled data; holdout eval; per-framework cross-service detectors beyond GraphQL/Ecto (HTTP/gRPC/pub-sub).
