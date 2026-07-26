---
title: "01 — Landscape: what existing tools actually do"
description: "What existing tools actually do — verified against real code, binaries, and MCP schemas"
sidebar:
  label: "01 — Landscape"
  order: 1
---
Verified against real sources: fresh git clones, the installed binary on disk, live MCP schemas, and vendor docs. Where a claim comes from evidence it is marked; where it's an inference it says so.

---

## The two headline tools

### graphify (Graphify-Labs) — Python

- **What it is:** PyPI `graphifyy`, CLI `graphify`. Parses a codebase/corpus into a knowledge graph. Ships mainly as a `/graphify` skill for AI assistants, plus a CLI and an MCP server. Code parsed deterministically with tree-sitter (no LLM); docs/PDFs/images get an LLM semantic pass. README: 40 languages / 36 tree-sitter grammars. *"No embeddings, no vector store: a real graph you traverse."*
- **Impact analysis — yes, but purely structural:**
  - `graphify affected "X"` — reverse-BFS over incoming edges (`calls, indirect_call, references, imports, imports_from, re_exports, inherits, extends, implements, uses, mixes_in, embeds`), default depth 2, reports each impacted node **with its call/import site**.
  - `compute_pr_impact(files, G)` → `(communities_touched, nodes_affected)` as a PR "blast_radius"; `graphify prs --conflicts` flags PRs sharing graph communities (merge-order risk).
- **Budget-aware query — yes:** `graphify query "..." --budget 2000` does BFS/DFS to a token budget. But matching is **substring/keyword**, not semantic (vocab mismatch degrades results).
- **Edges tagged** `EXTRACTED` / `INFERRED` / `AMBIGUOUS`.
- **MCP:** 10 tools — `query_graph, get_node, get_neighbors, get_community, god_nodes, graph_stats, shortest_path, list_prs, get_pr_impact, triage_prs`. stdio or HTTP.
- **Incremental:** SHA256 content cache + `manifest.json`; `graphify update` re-extracts changed files (AST-only, no LLM); `graphify watch` + a post-commit hook auto-rebuild.
- **Git usage — minimal; NO churn/co-change.** Grep for `git log`/`blame`/`rev-list` → none. Git is used only to (a) `git diff --name-only HEAD~1 HEAD` to trigger AST rebuilds, (b) a merge driver so parallel `graph.json` commits union-merge, (c) `gh` CLI for PR data. **No temporal churn / hotspot / co-change.**
- **Known limitations (open issues):** Bash sourced-file calls get no edge (#2141), `indirect_call` false edges (#2137), JS `imports_from` dangling refs (#2130), TS object-literal-export methods get no node (#2110); Postgres extraction has no column-level detail; graphs >5000 nodes must use `--no-viz`.

**Verdict:** has the impact API and the budget query — but **git-blind, structural-only, keyword search.**

### codebase-memory-mcp (DeusData) — C (+ C++)

- **Language, verified from the installed binary** (`~/.local/bin/codebase-memory-mcp`): ELF, `.comment` shows **GCC only**, **zero Rust runtime markers** (`/rustc/`, `core::panicking`, `__rust_*` all absent), dynamically links `libstdc++`. README confirms *"pure C, zero dependencies,"* vendoring tree-sitter + SQLite + LZ4 + zstd into one static binary. (The `Cargo.toml`/`nats.rs`/`go.mod`/`jakarta.ws.rs` strings inside the binary are **language-detection patterns**, not its own build system — they sit next to `go_module_types`, `composer.json`, `requirements.txt`.)
- **Performance claim:** Linux kernel 28M LOC indexed in ~3 min; sub-ms queries. 158 languages via vendored tree-sitter. → language is not the bottleneck.
- **Live graph schema** (from `get_graph_schema` on a real 15,616-node / 39,679-edge project). This tool is *richer than a plain call graph*:
  - Nodes: `Function, Method, Class, Interface, Type, Enum, Field, Variable, Module, File, Folder, Route, Channel, Resource`. `Function` carries `complexity, signature, param_types, return_type, is_test, is_exported, decorators`.
  - Edges: `CALLS {confidence, strategy, candidates, via}` (probabilistic!), `FILE_CHANGES_WITH {co_changes, coupling_score}` (**git co-change, already present!**), `HTTP_CALLS / GRAPHQL_CALLS / ASYNC_CALLS / EMITS` + `Route`/`Channel` nodes (**cross-service edges**), `TESTS / TESTS_FILE`, `SIMILAR_TO {jaccard}`, `SEMANTICALLY_RELATED {score}`, `CONFIGURES {config_key}`, `IMPLEMENTS, IMPORTS, THROWS, RAISES`.
- **Query:** read-only openCypher subset (WHERE, aggregates, label filters). `detect_changes` maps uncommitted diffs to affected symbols with a coarse risk classification.
- **MCP:** 15 tools incl. `index_repository, search_graph, trace_path, query_graph, detect_changes, get_architecture, get_code_snippet, manage_adr, ingest_traces`.
- **Incremental:** background watcher + git polling; fills local diffs from persisted snapshots. Persists to SQLite (WAL).

**Verdict:** the strongest *substrate* here — it already has co-change, confidence-weighted calls, and cross-service edges. But **no risk scoring, no packaged `impact(diff)` / `review_focus(pr)` API** (you hand-assemble via Cypher), and **C means real maintenance debt** (manual memory, hand-rolled parallelism, vendored-dep CVE tracking, thin contributor pool).

---

## The rest of the field, by category

### Static graph / semantic index — reachability only

| Tool | Technique | Impact propagation | Incremental |
|---|---|---|---|
| **Sourcegraph (SCIP)** | per-language indexers emit SCIP protobuf; GraphQL API | find-refs = one hop of reverse deps, no rollup/ranking | yes, per-commit |
| **GitHub stack-graphs** | name-binding DSL over tree-sitter (scope graphs); each file → isolated subgraph | reference edges only | **best-in-class file-incremental** |
| **Glean (Meta)** | compiler-derived facts in RocksDB; Angle (Datalog) queries | expressible in Angle, not packaged | yes, continuous |
| **CodeQL** | source → relational DB (AST+CFG+dataflow); QL queries | **semantic taint/dataflow** (deeper than call-reach) but expensive, security-framed | weak (DB rebuilt) |
| **ast-grep** | tree-sitter structural pattern match, Rust, multicore | none (single-file) | no persistent index |

Common limit: they produce **reference sets, not risk-ranked impact**, and mostly have **no PR-diff entry point**.

### Affected-detection — binary sets, coarse granularity

| Tool | Graph | Output |
|---|---|---|
| **Nx affected** | project DAG from TS imports + lockfile diff | affected projects (binary), task graph |
| **Bazel `rdeps` / `allrdeps`** | exact build graph | reverse deps (precise, BUILD-file granularity) |
| **Turborepo `--affected`** | workspace package DAG | affected packages |
| **MS Test Impact Analysis** | *dynamic* per-test coverage map | selected tests (catches reflection/DI) |
| **Google TAP** | Blaze dep graph at 2B-LOC scale | per-target AFFECTED/PASSED/SKIPPED + prioritization |

Common limit: **coarse (package/target) granularity, binary affected/not, no risk ranking, no LLM output.**

### Risk / review-targeting — git-history school

- **CodeScene (Adam Tornhill)** — the closest existing product to "where do I look first." *Hotspot* = complexity × change-frequency. *Change/logical coupling* = files/functions that co-change above a threshold (implicit deps invisible to static graphs). **PR Delta Analysis** computes delivery risk, detects Code-Health decline, and flags *the absence of an expected co-change* (a coupled file that should have changed but didn't → likely bug). **But:** temporal/statistical only (no call/type graph), **can't point to a function**, closed-source, not an MCP/agent primitive.
- **code-maat** (OSS predecessor) — mines `git log`; logical coupling = % shared commits. CSV output, no product.
- **Academic prior art** — Nagappan/Ball (ICSE 2005: relative churn measures **discriminated fault-prone Windows-Server-2003 binaries at ~89% accuracy** — a classification result, not a defect-density regression); JIT defect prediction (DeepJIT, CC2Vec); change-impact detectors combining churn + bug frequency + co-change + author + PR size. Proves the *risk recipe*; ships as research artifacts, unfused with a static graph.

### AI-native MCP servers (2025–2026) — token-optimized, risk-blind

| Tool | Impact | Git/risk | Incremental |
|---|---|---|---|
| **code-graph-mcp** (closest artifact) | yes — recursive dependents + HIGH/MED/LOW **from structural fan-out only** | **no git, no co-change** | BLAKE3 Merkle, dirty-propagation |
| **CodeGraph (~47k★) / GitNexus (~42k★)** | blast-radius traversal; 58–88% fewer tool calls | none | file watchers |
| **Codebadger (Joern CPG)** | deep dataflow to an LLM, security-leaning | none | — |
| **grepai / claude-context / Serena / Repomix** | retrieval / context packing | none | varies |

Common limit: they nailed **budget-aware output** but their risk is at best structural fan-out; **none ingest git history, none do co-change, none do PR-scoped review targeting.**

---

## Capability matrix

| | static graph | git co-change | cross-service edges | impact API | risk scoring | budget MCP | incremental |
|---|---|---|---|---|---|---|---|
| graphify | ✅ | ❌ | △ | ✅ | ❌ | ✅ | ✅ |
| codebase-memory-mcp | ✅ | ✅ | ✅ | △ (raw Cypher) | ❌ | ✅ | ✅ |
| Sourcegraph / stack-graphs | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ |
| CodeQL / Joern | ✅ (dataflow) | ❌ | ❌ | △ | ❌ (security) | ❌ | ❌ |
| Nx / Bazel / TAP | △ (coarse) | ❌ | ❌ | ✅ (binary) | ❌ | ❌ | ✅ |
| CodeScene | ❌ | ✅ | ❌ | △ (statistical) | ✅ | ❌ | ✅ |
| code-graph-mcp | ✅ | ❌ | △ | ✅ | △ (structural) | ✅ | ✅ |
| **ripple (target)** | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |

The last row is the point. No existing row has all seven. Details in [`02-gap.md`](02-gap.md).
