# 03 — Why Rust

The nearest substrate (codebase-memory-mcp) is **C**. graphify is **Python**. Neither language choice is wrong for what it is — but for a *greenfield* impact/review engine, Rust is the right pick. Here is the honest case, including where the advantage is thin.

## Performance

- **Fearless parallelism.** The three heavy phases — parse, resolve, git-overlay mining — are embarrassingly parallel per file / per commit. `rayon` turns them into `par_iter()` with **data races excluded at compile time**. In C you hand-roll a thread pool and prove safety by testing (and tsan); in Rust the borrow checker proves it.
- **Zero-cost native bindings.** `tree-sitter`, `petgraph`, and `git2` are first-class Rust — no FFI boundary in the hot path (Go's cgo tax on tree-sitter is real; Python's tree-sitter is a C binding with GIL contention on the Python-side graph assembly).
- **The ceiling is already known.** codebase-memory-mcp indexes 28M LOC in ~3 min in C. That proves the substrate is not the bottleneck — **language choice buys safety, not a speed miracle.** Rust reaches the same class of throughput while keeping the safety.
- **In-memory graph, not SQL traversal.** Query-time traversal runs over an in-RAM graph (`petgraph` / CSR adjacency), sub-millisecond, exactly as codebase-memory's "RAM-first" pipeline does. The store is a durable snapshot, not the query engine — see [`04-architecture.md`](04-architecture.md#store) and [`01-landscape.md`](01-landscape.md).

## Maintainability — the real reason

This is where the C substrate carries debt that a greenfield shouldn't inherit:

| Concern | C (codebase-memory) | Rust (ripple) |
|---|---|---|
| Memory safety | manual malloc/free; use-after-free / leaks / overruns not caught by the compiler | ownership + borrow checker; whole class gone |
| Parallel safety | hand-rolled pthreads; races found only by tests/tsan | `Send`/`Sync` + borrow checker; races are compile errors |
| Refactor resilience | weak types — "compiles ≠ correct" on big changes | exhaustive `enum` + `Result` + no null; the compiler drives large refactors |
| Dependencies | vendors tree-sitter/SQLite/lz4/zstd → **manual CVE tracking** | `cargo` + `cargo audit`; supply-chain hygiene is tooling, not toil |
| Contributor pool | few people send PRs to a C analysis engine → OSS durability risk (the 40k★-solo-maintainer problem) | large Rust dev-tools community |
| Tests | unit tests in C are heavy | `#[test]`, `cargo test`, property tests (`proptest`) are cheap |

**But not a rewrite.** Rewriting the C substrate to "replace" it would discard years of tacit indexing know-how (RAM-first pipeline, custom LSP heuristics, LZ4 staging) for zero gain toward the actual goal. This is a **greenfield scoped to our languages + the overlay baked in from day one** — codebase-memory is a *reference implementation* to learn algorithms from, not code to port. (Joel Spolsky's "never rewrite" applies to the *substrate*, not to a differently-scoped new product.)

## Readability

Rust's type system makes the two things that matter most *declarative*:

- **The IR** is a pair of `enum`s (`NodeKind`, `EdgeKind`) — the whole vocabulary the upper layers speak, in 20 lines.
- **Language adapters** are a `trait` with data-driven defaults — a reader sees exactly what a language must provide vs. what it inherits.
- Pattern matching over the IR makes the risk/query logic read like the spec formula, not like pointer chasing.

## Ecosystem fit

| Need | Crate | Note |
|---|---|---|
| Parsing | `tree-sitter` + grammar crates | native, incremental |
| Parallelism | `rayon` | data-parallel phases |
| Git mining | `git2` (libgit2) | churn / co-change / blame / ownership |
| In-memory graph | `petgraph` | traversal, algorithms |
| Durable store | `samyama` (Rust embedded graph+vector, Apache-2.0, v1.1, LDBC-certified) | OpenCypher + HNSW + 14 algos + MVCC time-travel; `redb` pure-Rust fallback; `mnestic` alternative — all via the `GraphStore` trait (see [`04`](04-architecture.md#store)) |
| Serialization | `serde` + `rkyv` (or `postcard`) | fast snapshot I/O. **Not `bincode`** — unmaintained since a Dec-2025 incident; its last crates.io release is a deliberately non-compiling tombstone. |
| MCP server | `rmcp` (official `modelcontextprotocol/rust-sdk`, Tokio, stdio+SSE) | agent surface |
| Fuzzy/rank | `bm25`, custom RRF | budget-aware ranking |

## Honest trade-offs

- **Early index precision will trail the incumbents.** They have years of per-language resolution heuristics. Mitigation is architectural, not heroic: shallow resolution (candidate edges with confidence) **plus the git co-change signal**, which fills the precision gap for the impact/review use case specifically. This is a deliberate bet, documented in [`02-gap.md`](02-gap.md).
- **Compile times & learning curve** are real Rust costs. Acceptable for a long-lived dev-tools core; the workspace split (below) keeps incremental builds fast.
- **Store dependency (young ecosystem corner).** Primary is **Samyama** (Rust, Apache-2.0, v1.1, LDBC-certified, company-backed) — chosen over the earlier pick mnestic (pre-1.0, solo maintainer) once we made blast radius store-agnostic and no longer needed mnestic's built-in budgeted traversal. Its one cost: a RocksDB (C++) backend, so the pure-Rust single-binary property lives on the `redb` fallback path. Both isolated behind the `GraphStore` trait; v0 ships both so the choice is measured, not assumed. (KùzuDB ruled out — Apple acquisition, OSS dev stopped; LadybugDB is its living successor.) Detail in [`04`](04-architecture.md#store).
- **Perf crossover (RAM vs disk).** In-RAM `petgraph` BFS beats any disk DB *when the graph fits in RAM*. Above that (huge monorepos), you can't hold it — then you rely on the store's disk-resident traversal (mnestic `BudgetedTraversal` / LadybugDB columnar) and accept slower-but-feasible. State the crossover; don't imply RAM is always available.

**Net:** Rust doesn't make it faster than C in a way users would feel. It makes it **safe to parallelize, cheap to refactor, and possible for others to contribute** — which is what determines whether an ambitious analysis engine survives past its first author.
