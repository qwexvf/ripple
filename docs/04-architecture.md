# 04 — Architecture

Clean, layered, language-agnostic core over a thin per-language seam. The whole design exists to make one invariant true:

> **Everything above the `ir` layer is blind to which programming language it's looking at.** Adding a language touches only `lang/` (plus data files). Nothing else recompiles its logic.

## Layers (bottom → up)

```
┌─ mcp ──────────── agent surface: impact() / review_focus() tools        │ language-agnostic
├─ query ────────── traversal + ranking + budget-aware output            │ language-agnostic
├─ overlay ──────── git mining: churn / co-change / bug-density / owner   │ language-agnostic (file-level)
├─ store ────────── in-memory graph + durable snapshot + incremental      │ language-agnostic
├─ resolve ──────── link references → definitions                        │ mostly agnostic + hooks
├─ ir ───────────── normalized Node/Edge vocabulary  ★ decoupling seam    │ THE boundary
├─ lang ─────────── LanguageAdapter trait + adapters/*  ★ language-specific lives ONLY here
└─ parse ────────── tree-sitter driver (grammars loaded as data)         │ language-agnostic
```

Dependency direction is strictly upward — no cycles. `ir` depends on nothing. `parse` depends on nothing but tree-sitter. Every higher crate depends only on those below it.

## The decoupling seam: normalized IR

Every language emits the **same** node and edge vocabulary. Python `def`, Gleam `fn`, and TS `function` all become `NodeKind::Function`. The upper layers only ever see this:

```rust
// crate: ir  — depends on nothing
pub struct SymbolId(pub u64);        // stable hash — see identity rules below
pub struct FileId(pub u32);

pub enum NodeKind {
    File, Module, Function, Method, Class, Interface,
    Type, Enum, Field, Variable, Route, Channel,
}

pub enum EdgeKind {
    Defines, Calls, References, Imports, Implements, Extends,
    // cross-service (populated by Tier-3 detectors, still language-agnostic here):
    HttpCall, GraphqlCall, AsyncCall, Emits,
    // tests & overlay-derived:
    Tests, ChangesWith,
}

pub struct Node {
    pub id: SymbolId,
    pub kind: NodeKind,
    pub name: String,
    pub qualified_name: String,
    pub file: FileId,
    pub span: Span,
    pub complexity: u32,          // language-agnostic (computed from AST shape)
    pub is_exported: bool,
    pub is_test: bool,
    // overlay-derived, filled at index time (default 0 until v1):
    pub risk: RiskScores,        // see 06
}

pub struct Edge {
    pub src: SymbolId,
    pub dst: SymbolId,
    pub kind: EdgeKind,
    pub confidence: f32,         // 1.0 = EXTRACTED, <1.0 = INFERRED/AMBIGUOUS
    pub site: Span,             // where the call/import appears
}
```

`RiskScores` and overlay fields default to zero, so the graph is valid at every tier and every phase — the overlay just fills them in later.

**Symbol identity rules (subtle, gets it wrong easily).** A `SymbolId` must be stable across edits and rename-tolerant, or incremental re-index and co-change linkage break:
- **Overloads:** hashing `(qualified_name, kind)` alone collides overloaded functions (same name, different signature). Include a signature discriminator.
- **Renames/moves:** putting the raw *file path* in the ID means moving a file changes every symbol's ID, orphaning history and co-change edges. Prefer identity keyed on `(package-relative module path, qualified_name, signature)` and reconcile file moves via `git log --follow` so history survives a rename. Path is metadata, not identity.
- **Confidence values:** `confidence` is 1.0 for `EXTRACTED`; for an `AMBIGUOUS` call with N candidate targets, assign each candidate edge `≈ 1/N` (or type-narrowed higher), so ranking can discount unresolved dispatch rather than treating a guess as fact.

## The language seam: `LanguageAdapter`

Most of an adapter is **data** (`.scm` tree-sitter queries), not code. Required methods are few; the rest have working defaults. This is what makes "add a language later" cheap — see [`05-language-support.md`](05-language-support.md).

```rust
// crate: lang
pub trait LanguageAdapter: Send + Sync {
    // ── required (Tier 0) ──
    fn id(&self) -> LangId;
    fn grammar(&self) -> tree_sitter::Language;
    fn file_globs(&self) -> &[&str];              // ["*.ts", "*.tsx"]
    fn tags_query(&self) -> &str;                 // tags.scm: captures → NodeKind

    // ── Tier 1: import resolution (has a default) ──
    fn imports_query(&self) -> Option<&str> { None }
    fn resolve_import(&self, spec: &str, from: &Path, ws: &Workspace) -> Option<PathBuf> {
        default_relative_resolve(spec, from)      // override per language module system
    }

    // ── Tier 2: call/reference resolution (optional) ──
    fn refs_query(&self) -> Option<&str> { None }
    fn scoping(&self) -> ScopePolicy { ScopePolicy::LexicalDefault }

    // ── Tier 3: framework detectors for cross-service edges (optional) ──
    fn detectors(&self) -> &[Box<dyn FrameworkDetector>] { &[] }
}
```

Registration is a table — no dynamic magic:

```rust
pub fn registry() -> Vec<Box<dyn LanguageAdapter>> {
    vec![
        Box::new(typescript::Adapter::new()),
        // Box::new(gleam::Adapter::new()),   // add a line, add a folder. Nothing above changes.
    ]
}
```

## Pipeline

```
        ┌──────────── per file, rayon par_iter, NO shared state ───────────┐
 parse ─┤ tree-sitter parse → run adapter.tags_query / imports_query / refs │
        └──────────────────────────────────────────────────────────────────┘
                         │  emits IR nodes + unresolved refs
                         ▼
 resolve  link refs → defs   (generic resolver + adapter.resolve_import / scoping hooks)
                         │  emits resolved Edges with confidence
                         ▼
 store    build in-memory graph + write durable snapshot (incremental by content hash)
                         │
                         ▼
 overlay  git2: mine churn / co-change / bug-density / ownership → fill Node.risk & ChangesWith edges
                         │   ← operates on IR + git ONLY. Never sees a language.
                         ▼
 query    impact(diff) / review_focus(pr): traverse in-RAM graph, rank by risk, emit within budget
                         ▼
 mcp      expose query results as agent tools
```

**Invariant:** only `parse`/`resolve` ever touch a `LanguageAdapter`. From `store` upward, code sees only `ir`. This is what a CI guard enforces (see [`05-language-support.md`](05-language-support.md#extensibility-guardrail)).

## Store — in-memory graph + durable snapshot {#store}

The storage question deserves care: **an RDBMS as a *query engine* is the wrong tool** for deep reverse-dependency BFS (recursive-CTE traversal is slow and awkward). But an RDBMS or KV store as a *durable snapshot* is fine. The proven pattern (codebase-memory's "RAM-first") separates the two:

```
query time:   in-memory graph (petgraph / CSR adjacency)   → BFS / blast radius, sub-ms
persistence:  embedded store                                → durability + incremental load
git analytics: aggregated once at index time, materialized onto nodes/edges → zero query-time cost
```

Query-time traversal always runs over the in-RAM graph — no disk DB beats it for hot BFS. The store's job is durability, incremental load, and (optionally) offloading queries that don't fit RAM. So "which DB" only decides *how much the DB gives us for free* vs. how much we hand-roll.

**Decision (2026-07, revised after review): primary = Samyama, isolated behind the `GraphStore` trait.**

The choice first landed on mnestic for its built-in `BudgetedTraversal`. Review reversed it: (a) we redesigned blast radius as a **store-agnostic bounded diffusion** ([`06`](06-risk-and-queries.md)), so budgeted traversal being built-in is now a convenience, not a requirement; (b) that removed mnestic's one decisive edge, leaving its real risks — **pre-1.0 and effectively one maintainer** — unjustified for a *foundational* dependency. Samyama neutralizes both.

- **Query engine:** in-memory `petgraph` (or CSR adjacency for large graphs). Never SQL traversal.
- **Primary durable store: [Samyama](https://samyama.dev)** ([repo](https://github.com/samyama-ai/samyama-graph)) — Rust-native graph-vector DB, Apache-2.0, embedded + remote SDK (`crates/samyama-sdk`). Chosen for **durability and maturity**, which matter most in a foundational store:
  - **post-1.0 (v1.1.0) and LDBC-certified** (SNB Interactive 21/21, FinBench 40/40, Graphalytics) — validated, not a pre-release.
  - **company-backed** (VaidhyaMegha) rather than a single maintainer — the key de-risk vs mnestic.
  - **~90% OpenCypher** (read + write), **HNSW** vector search + Graph RAG, **14 rayon-parallel graph algorithms**, **MVCC time-travel** (query as-of a past state — fits git-history/co-change-over-time).
  - RocksDB + WAL backend; benchmarked 74M nodes / 1B edges on one machine; open-core (Enterprise adds GPU/HA/PITR — we need none of that).
- **Pure-Rust fallback (same trait):** `redb` (pure-Rust KV, Apache-2.0, **v4.x — very mature**, near-zero deps). The choice when a **single static binary with no C/C++** matters; we hand-roll the bounded-diffusion traversal over it.
- **Kept as alternative:** **[mnestic](https://github.com/shuruheel/mnestic)** (Cozo fork, MPL-2.0) — retains two things Samyama lacks: **built-in budgeted weighted traversal** and **two-axis bitemporality** (valid + transaction time, vs Samyama's single-axis MVCC). If a v0 spike shows we lean hard on either, mnestic returns — but its pre-1.0 / solo-maintainer risk must then be accepted.
- **Also available:** LadybugDB (embedded columnar graph, living Kùzu successor) for monorepos too large for RAM.
- **Ruled out:** **KùzuDB** — acquired by Apple (repo archived Oct 2025, disclosed Feb 2026), OSS dev stopped. Any RDBMS as the *traversal* engine (SQLite acceptable only as a snapshot backend).

### The bet is reversible by construction

Two guarantees keep the store choice cheap to change — essential since this is a young, fast-moving corner of the ecosystem:

1. **Isolation:** all store-specific query dialect (Cypher/Datalog) lives *inside* the concrete `*Store` impl. The rest of ripple speaks only the `GraphStore` trait — never sprinkle a DB dialect across the codebase. A backend swap is one crate.
2. **v0 proves the abstraction:** v0 ships **two** implementations from day one — `SamyamaStore` (primary) and `RedbStore` (pure-Rust baseline) — so the trait is exercised, not theoretical, and the Samyama-vs-mnestic-vs-redb decision is made on **measured fit**, not description.

Tradeoff acknowledged: Samyama's RocksDB backend pulls **C++**, so the "single static Rust binary, no C" property is only fully held by the `redb` path. This is acceptable — we *depend on* RocksDB, we don't *maintain* it, so the "C maintenance debt" argument (about code we'd own) doesn't apply; only static-binary purity is traded, and `redb` preserves it when needed.

```rust
// crate: store — the abstraction that lets the backend swap
pub trait GraphStore {
    fn upsert_file(&mut self, file: FileId, hash: Blake3, nodes: &[Node], edges: &[Edge]);
    fn changed_files(&self, current_hashes: &HashMap<PathBuf, Blake3>) -> Vec<PathBuf>; // incremental
    fn load_graph(&self) -> InMemoryGraph;      // hydrate petgraph for querying
    fn snapshot(&self) -> io::Result<()>;
}
```

Incremental re-index = hash each file (BLAKE3), re-parse only changed files, patch their nodes/edges, re-run the overlay on affected files. Same approach as stack-graphs (file-isolated) and code-graph-mcp (Merkle).

## Operational lifecycle

```
initial    : full index (batch, once) ───────────────────► durable snapshot (mnestic)
agent start: snapshot ──load (no re-parse)──────────────► in-RAM graph      [fast: hydrate, not reindex]
on edit    : changed file → re-parse → patch RAM graph + write-through store  [static layer only]
on commit  : git overlay recompute → re-score risk → update RAM + store        [git layer]
```

Two subtleties that keep the loop cheap:

- **Load ≠ re-index.** Agent startup *hydrates* the graph from the snapshot; the one-time full-index cost is never paid again. With a resident daemon (v3), even the hydrate is skipped across sessions — the graph stays warm.
- **Static and git layers update on different triggers.** The static graph reflects the **working tree** (incl. uncommitted edits) and updates *per edit*. The git overlay (churn / co-change / bug-density / ownership) reads **commit history**, which uncommitted edits don't change — so it recomputes *on commit* (or periodically), not per keystroke. Per-edit work is limited to re-parsing the touched file, patching its nodes/edges, and re-scoring the risk `fanout` of directly-affected nodes. Recomputing the whole git overlay on every edit would make editing needlessly slow.
- **Write-through keeps RAM and store consistent.** Each edit patches the in-RAM graph and commits to the store in one transaction, so a crash resumes from a coherent snapshot.

**Concurrency.** One writer (the indexer) + many readers (agent queries). Readers must never see a half-applied incremental update, so queries run against an immutable snapshot of the in-RAM graph (swap the `Arc<Graph>` pointer atomically after an update completes; readers hold their snapshot for the query's duration). mnestic's MVCC / bitemporal reads give the same isolation at the store layer.

**Memory model.** Rough sizing from a real index (`codebase-memory` on a 15,616-node / 39,679-edge project ≈ 40 MB on disk). In-RAM, budget on the order of ~1–2 KB per node + edges after compact encoding (interned strings, `u32` ids, CSR adjacency). Millions of nodes → single-digit GB, which is why `codebase-memory` can hold 28M LOC RAM-first. Beyond a configurable RAM ceiling, fall back to disk-resident store traversal (see [Store](#store)) and use **scoped queries** (a package/path filter on `impact`/`review_focus`) so a single query never hydrates the whole monorepo.

## Cargo workspace

```
ripple/
  Cargo.toml                 # [workspace]
  crates/
    ir/                      # vocabulary. zero deps. everyone depends on it
    parse/                   # tree-sitter driver
    lang/                    # trait + registry
      adapters/
        typescript/          # mod.rs + queries/{tags,imports,refs}.scm
        # gleam/  python/  go/   ← added later, self-contained
    resolve/
    store/                   # GraphStore trait; samyama (default) + redb (pure-Rust) from v0; mnestic/ladybug behind feature flags
    overlay/                 # git2 mining + risk scoring
    query/                   # impact / review_focus / ranking / budget
    mcp/                     # server (stdio + http)
    cli/                     # `ripple index`, `ripple impact`, `ripple review`
```

Adding a language = add `crates/lang/adapters/<lang>/` and one line in `registry()`. Every other crate is untouched — enforced, not hoped. Details: [`05-language-support.md`](05-language-support.md).
