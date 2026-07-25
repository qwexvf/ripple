# CLAUDE.md — working in the ripple repo

Guidance for Claude Code (and humans) contributing to ripple. Read this before writing code.

## What ripple is

An AI-native code impact & review-targeting engine: given a change, return a risk-ranked blast radius; given a PR, return the hunks to review first. Language-agnostic core over a thin per-language adapter seam, in Rust. Design lives in [`docs/`](docs/) — start with [`README.md`](README.md), then [`docs/04-architecture.md`](docs/04-architecture.md) and [`docs/v0-plan.md`](docs/v0-plan.md).

## Repo layout

```
crates/
  ir/       normalized graph vocabulary (NodeKind/EdgeKind/Node/Edge/SymbolId). zero deps. the decoupling seam.
  parse/    tree-sitter driver: source → IR + pre-resolution records. language-agnostic (reads capture names).
  lang/     LanguageAdapter trait + per-language adapters (queries as .scm data). language-specific lives ONLY here.
  resolve/  cross-file linking: discover → index_defs → link (imports + calls).
  store/    GraphStore trait + RedbStore + in-memory graph (neighbors). query runs in RAM, never in SQL.
  cli/      `ripple` binary: parse / index / neighbors.
```

## Commands

```
cargo fmt --all                     # format (rustfmt defaults — non-negotiable)
cargo clippy --all-targets          # must be clean; CI denies warnings
cargo test                          # unit + golden-fixture + contract tests
cargo run -p ripple-cli -- <cmd>    # parse <file> | index <path> | neighbors <symbol>
```

## Picking the next task

Work is tracked on **GitHub Projects board 7** (`https://github.com/users/qwexvf/projects/7`),
not in any file in this repo. At the start of a session, read it — don't infer priorities
from the code or from `docs/`:

```
gh issue list --repo qwexvf/ripple --label next-up --state open   # ranked shortlist
gh project item-list 7 --owner qwexvf --format json               # full board + Status
gh issue view <n> --repo qwexvf/ripple                            # the actual context
```

- **`next-up`** is the ranked shortlist. Take the lowest-numbered unblocked one unless
  the user says otherwise, and re-rank openly if the reasoning has changed.
- **In Progress** items have partial work already committed; the issue comments say what
  landed and what's left.
- Each issue body carries the measurement or failure that motivated it. Read it before
  starting — several were opened precisely because a plausible-looking fix made a number
  worse.

When work finishes: comment the **evidence** on the issue (measured numbers, not "done"),
set Status, close it, and move `next-up` to whatever is now top. When something new is
found, open an issue instead of keeping a local list — `PROGRESS.txt` was deleted on
purpose and must not come back.

`docs/12-dogfood-log.md` is the running record of what ripple got wrong when used for
real; it has produced more committed fixes than the roadmap has, and its open entries are
usually the best candidates for new issues.

## Architecture invariants (do not break)

These are the load-bearing rules the design depends on. A change that violates one needs a design discussion, not a patch.

1. **The IR boundary.** Everything above `ir` (resolve, store, overlay, query, mcp) is blind to which language produced a node. Only `parse`/`resolve` touch a `LanguageAdapter`. Never leak a language-specific concept (a tree-sitter node kind, a TS-ism) above `ir`.
2. **The adapter seam.** Adding a language touches only `crates/lang/` (a module + `.scm` files + one `registry()` line). If a new language forces a change elsewhere, that's an abstraction leak — fix the abstraction.
3. **Store isolation.** Store-specific query dialect (Cypher/Datalog for Samyama) lives *inside* the concrete `*Store` impl. The rest of ripple speaks only the `GraphStore` trait, so a backend swap is one crate. See [`docs/04-architecture.md#store`](docs/04-architecture.md).
4. **Query in RAM, not in the DB.** Traversal runs over the in-memory graph. The store is a durable snapshot; never push blast-radius BFS into SQL/Cypher on the hot path.
5. **Confidence is first-class.** Every inferred edge carries a `confidence` (1.0 = extracted, `1/N` over N ambiguous candidates). Never emit a fabricated single edge where resolution is uncertain — emit candidates or drop.

## Code style

We adopt the conventions of established Rust projects rather than inventing our own:

- **Formatting:** `rustfmt` defaults. Run `cargo fmt --all`; no hand-formatting, no per-file overrides.
- **Linting:** `cargo clippy --all-targets` is clean before every commit; **CI denies warnings**. `clippy::pedantic` is advisory — apply its correctness/clarity hints, ignore pure noise (`must_use`/`# Errors` on pre-1.0 internal items).
- **Naming & API shape:** follow the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/) — RFC 430 casing, getters without a `get_` prefix, `AsRef`/`Into` where it reads well, iterators over index loops.
- **Style philosophy:** follow the [rust-analyzer style guide](https://github.com/rust-lang/rust-analyzer/blob/master/docs/dev/style.md):
  - Short, single-purpose functions; push complexity *down*, not sideways. (We split `resolve::build` into `discover`/`index_defs`/`link` for exactly this.)
  - Early returns and `let ... else` over nested `if let`.
  - Concrete types until generics earn their keep — no premature abstraction.
  - Group related data into a named struct rather than threading many args (see `DefIndex`).
  - Comments explain **why**, not what. Delete comments that restate the code.
- **Errors & panics:** `anyhow` + `?` + `.context()` at app/binary level; a dedicated `thiserror` error type when a crate's error surface stabilizes. **No `unwrap`/`expect`/`panic!` in non-test library code** — model failure with `Result`. Panics are reserved for genuine invariants and must be documented.
- **Determinism:** query/graph output must be reproducible across runs — stable sorts with a total tie-break key, fixed reduction order. A change that introduces run-to-run nondeterminism is a bug.
- **Docs:** every public item has a doc comment; every module has a `//!` header stating its job and its place in the layering.

## Review style

Modeled on ripgrep / tokio / rust-analyzer review culture:

- **Small, focused PRs.** One concern per PR. A refactor and a behavior change are two PRs.
- **Correctness over cleverness.** The reviewer's first question is "how does this fail?" — concrete failing input beats abstract worry.
- **Behavior ships with tests.** Every behavioral change adds or updates a test; extraction/resolution changes update the golden fixtures. No test, no merge (barring pure docs/format).
- **No new warnings.** `cargo fmt` clean, `cargo clippy --all-targets` clean, `cargo test` green — non-negotiable gates.
- **Public API / invariant changes** need a one-line rationale and a doc update. Anything touching the five invariants above gets extra scrutiny.
- **Reviewer comments are specific and kind.** State the problem, the failure it causes, and a suggested fix. Nits are labeled `nit:` and are non-blocking.

## Commits

Short, lowercase, imperative subject stating what changed (`v0 M2: member call resolution`). Body only when the *why* isn't obvious. No AI/assistant attribution lines. Stage only the files the change needs — never `git add -A` blindly.
