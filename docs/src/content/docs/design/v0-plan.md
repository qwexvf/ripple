---
title: "v0 — execution plan (TypeScript, Tier 2)"
description: "The build plan for the first slice: crate order, TS reference resolution, testing and done criteria"
sidebar:
  label: "v0 — Execution plan"
  order: 15
---
Concrete build plan for the first shippable slice: a correct, fast static graph for TypeScript, queryable via `ripple index` and `ripple neighbors`. Planning v0 forces the design of the one piece the spec left thin — **Tier-2 reference resolution** (review gap #1) — so that algorithm is specified here for TS.

## Scope

**In:** TS/TSX parsing → defs, imports, call/reference graph; `redb` + `Samyama` stores behind `GraphStore`; in-RAM `petgraph`; BLAKE3 incremental; `ripple index` / `ripple neighbors`; monorepo + `tsconfig` resolution.

**Out (later phases):** git overlay & risk (v1); `impact`/`review_focus` & MCP (v2); other languages, cross-service, daemon (v3); full type inference (never — shallow + candidates by design).

## Crate build order (each milestone yields a runnable artifact)

| Milestone | Crates | Runnable result |
|---|---|---|
| **M0** | `ir`, `parse` | `ripple parse <file>` dumps symbols — proves tree-sitter + `tags.scm` |
| **M1** | `lang/typescript` (imports), `resolve`, `store` (`redb`) | `ripple index` + `ripple neighbors` for imports + local/imported calls |
| **M2** | `resolve` (member/this/candidate) | fuller call graph incl. `obj.method()` candidate edges |
| **M3** | `store` (`SamyamaStore`), incremental, tsconfig/workspaces | store spike decision + real-repo perf + incremental re-index |

## The core design v0 forces: TS reference resolution

Two phases. Phase A is per-file and parallel (`rayon`); Phase B links across files.

**Phase A — extract (per file, no shared state):**
- `tags.scm` → def nodes (`Function`/`Method`/`Class`/`Interface`/`Type`/`Enum`/`Field`/`Variable`), with `is_exported`, span, signature.
- `imports.scm` → import records `{ local_name, imported_name, module_specifier }`.
- `refs.scm` → unresolved refs `{ name, ref_kind (call|value|type), receiver?, scope_span }`.
- Build a **lexical scope tree** from tree-sitter (module → function → block), so a ref knows its enclosing scopes.

**Phase B — resolve (link refs → defs):**

1. **Imports first.** `resolve_import(specifier, from, workspace)` → target `FileId`; bind each `local_name` to the target file's matching exported symbol. Resolution order (TS reality): relative paths → `tsconfig` `paths`/`baseUrl` → `package.json` `exports`/`imports` → workspace package → `node_modules`; extension probing `.ts/.tsx/.d.ts/index.*`; re-exports (`export * from`, `export { x } from`) followed transitively.
2. **Name resolution.** For a ref, walk the scope tree outward: local binding → enclosing scopes → module scope → imported bindings → global. Resolves plain `foo()` and type refs with **high confidence**.
3. **Member calls `obj.foo()`** — the hard case, handled shallowly but honestly (no type inference):
   - `this.foo()` → resolve within the enclosing class → **0.9**.
   - `new Bar().foo()` / `const b = new Bar(); b.foo()` (binding traceable to a `new`) → link to `Bar.foo` → **0.9**.
   - typed param `(b: Bar) => b.foo()` (annotation readable syntactically) → **0.85**.
   - bare `x.foo()` with `x` untypeable → **candidate edges** to *all* methods named `foo`, each confidence **1/N** (the `AMBIGUOUS` rule from [`04`](04-architecture.md)). Never emit a fake single edge.
   - genuinely unresolvable → **drop** (no dangling fake edge), record for diagnostics.
4. **Emit** `Calls`/`References`/`Imports` edges with `confidence` + `site`.

**Confidence ladder (v0):** local/imported call 0.95 · this/new-instance method 0.9 · typed-param method 0.85 · bare candidate 1/N · unresolved dropped. This is the "shallow static + candidates" bet made concrete; v1's git co-change later compensates where confidence is low.

## TS specifics to get right

- `tsconfig` `paths`/`baseUrl`; nested tsconfigs (`references`).
- Monorepo workspaces: `pnpm-workspace.yaml` / npm|yarn workspaces; `package.json` `exports`/`imports` maps.
- Barrel files (`index.ts` re-exports) — follow transitively but cap depth.
- `.d.ts` — link declarations (no body).
- JSX/TSX components — function/`const` components are `Function` nodes; `<Comp/>` usage → `References` edge.
- `export default`, namespace imports (`import * as ns`), aliased imports.

## Store & incremental

- `GraphStore` trait: `upsert_file`, `changed_files`, `load_graph`, `snapshot`. **Two impls from day one** — `RedbStore` (pure-Rust baseline) and `SamyamaStore` (via `samyama-sdk` embedded) — so the trait is exercised and the store decision is measured (the M3 spike).
- Query: hydrate `petgraph` from the store; `neighbors` traverses in-RAM (sub-ms target).
- **Incremental invalidation (subtle):** a file's resolved edges depend on *other files' exports*. So when file X's exported surface changes, importers of X must be re-resolved, not just X. Track a **reverse-import dependency set**; on change, re-parse the changed file and re-resolve it + its importers. BLAKE3 content hash gates "did the file actually change." Symbol identity per [`04`](04-architecture.md) (module-relative path + signature, `git log --follow` reconciliation deferred to v1 when git is wired).

## Indexing model (fills gap #7)

- Respect `.gitignore`; always skip `node_modules`, `dist`/`build`/`out`, `.git`, declared generated dirs. Configurable ignore globs. Lockfiles parsed only for workspace/dep mapping, not as source.

## CLI

```
ripple index <path> [--store redb|samyama] [--ignore <glob>...]
ripple neighbors <symbol> [--kind calls|imports|all] [--depth N] [--json]
ripple parse <file>          # M0 debug: dump extracted symbols
```

## Testing & acceptance

- **Golden fixtures:** a tiny TS repo (imports, re-exports, `this`/`new`/typed-param/bare member calls, JSX) with a checked-in expected node/edge set. This *is* the resolution-precision measurement.
- **GraphStore contract test:** `RedbStore` and `SamyamaStore` must produce identical graphs from the same input.
- **Determinism test:** index twice → byte-identical graph (stable ordering, per [`06`](06-risk-and-queries.md)).
- **Incremental test:** edit one file → only it + its importers re-resolved; result equals a full re-index.
- **Perf smoke:** index a real TS repo (e.g. the omni client); `neighbors` sub-ms; record index time + RAM.

## v0 risks / decisions to close

1. **`samyama-sdk` embedded API maturity** — **spiked (2026-07): not on crates.io** (only `samyama-graph-algorithms`/`samyama-optimization` sub-crates are). Embedding needs a git dep on the full RocksDB/C++ repo, so v0 ships on `RedbStore` and defers `SamyamaStore` behind the `GraphStore` trait. See [`09-review-and-corrections.md`](09-review-and-corrections.md).
2. **TS member-call precision** — accept candidate-sets; the golden fixtures quantify it. If candidate explosion is bad, tighten with lightweight local type-flow (still no full inference).
3. **Incremental cross-file invalidation** — reverse-import tracking is the tricky bit; the incremental test guards it.

## Definition of done (v0) — ✅ complete (2026-07)

- ✅ `ripple index` builds a TS graph for a real monorepo — omnicampus-client: **1078 files → 7500 nodes / 4739 edges in ~0.5s** (14-core parallel parse).
- ✅ incremental re-index matches a full rebuild and skips unchanged files — **warm re-index ~0.2s** (all unchanged), verified byte-identical by `incremental_matches_full_and_reuses_cache`.
- ✅ `ripple neighbors` returns correct callers/importers; traversal is sub-ms (graph loads from redb in ~0.5s on the CLI cold path; a resident daemon amortizes this — v3).
- ✅ `GraphStore` contract test passes on `RedbStore`, written generically so `SamyamaStore` runs the same suite when added. (Samyama SDK unpublished → deferred behind the trait; see [`09`](09-review-and-corrections.md).)
- ✅ golden fixtures pass (parse + resolve), `cargo clippy --all-targets` clean, output deterministic.

No git, no risk, no MCP yet — that's v1/v2.

### Key perf lesson
The first real-repo run took 37s wall / 540s CPU. Root causes: (1) compiling the tree-sitter queries **per file** (28k compilations) — fixed by compiling once per language and sharing `&Queries` across rayon threads; (2) `.claude/worktrees/` duplicated the whole tree ~6.5×. After both: **~75× faster**. Lesson: cache compiled queries; never `Query::new` in a hot loop.

### Known follow-ups (not v0 blockers)
- Import alias / namespace imports; function-scope (not file-wide) type map for member calls.
- Graph load time on the CLI cold path (resident daemon in v3).
- `git log --follow` rename reconciliation for stable identity across moves (v1, with git).
