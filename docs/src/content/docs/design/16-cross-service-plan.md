---
title: "16 — Cross-service plan: one vocabulary, framework detectors"
description: "Execution plan for making cross-service resolution framework-agnostic — RouteKey vocabulary, detector seam, generic linker, HTTP as the proof"
sidebar:
  label: "16 — Cross-service plan"
  order: 16
---
Execution plan for [issue #32](https://github.com/qwexvf/ripple/issues/32) — tracked
there; phases get ticked on the issue as they land. This doc is the durable design
record, the way [`v0-plan.md`](v0-plan.md) was for the first slice.

## Task order — start at 1, one commit per phase

**Phase 1 (vocabulary, zero behavior change)**
1. `ir`: `Transport`/`Segment`/`RouteKey` + segment matcher + unit tests
2. `lang/cross.rs`: new facts (`Provides`/`Consumes`/`HandlerRef`/`GraphqlFacts`);
   rewrite the GraphQL + TS emitters; delete `ElixirFacts`/`GqlOp`/`GqlSpread`
3. `lang/elixir/dsl.rs`: emit `Provides`; add `pascalize`; `camelize` moves here;
   `scope_includes` wire-spelled
4. `resolve/crossservice.rs`: mechanical re-read of the new field names — logic untouched
5. Fix tests (`cross.rs`, `dsl.rs`) → full gates → **5noobs parity check:
   9179 nodes / 20337 edges / 846 graphql / 941 db, byte-identical** → commit

**Phase 2 (generic linker)**
6. Rework the linker around a `(transport, method, bucket)` Provides index + the
   confidence ladder from [`10-cross-service-resolution.md`](10-cross-service-resolution.md);
   GraphQL protocol module (descent/fragments/includes) in wire names only
7. Unmatched counters (`consumes with no provider` / `providers never consumed`)
   + print in the index summary
8. Fixtures `crossservice`/`nested`/`opcase` unchanged → 5noobs parity again → commit

**Phase 3 (HTTP detector — proof of the seam)**
9. Producer: Phoenix router table over the existing generic macro scanner
   (`scope` prefixes compose; `get/post/... "/path", Controller, :action`)
10. Consumer: TS `fetch`/`axios` detector, template-literal → `Param`,
    confidence_hint by literal ratio
11. New `fixtures/http`, mutation-checked
12. 5noobs measurement: HttpCall count + unmatched counts;
    **`git diff --stat` must be confined to `crates/lang/` + fixtures** → commit
13. Baselines (`use-ripple` skill), tick phases on #32, note on #13

## Context

The differentiator of ripple is cross-service edges (846 GraphqlCall + 941 DbQuery
today) — the thing no language server can produce
([`15-two-tools-two-jobs.md`](15-two-tools-two-jobs.md)). But measurement shows the
current implementation is **not** framework-agnostic:
`crates/resolve/src/crossservice.rs` contains 18 framework/language references
(`cross.elixir` ×5, `document_key` ×5, `Absinthe`, `decamelize`, `gql_*`, `is_schema`).
It is shaped as "Absinthe ↔ graphql-codegen ↔ Ecto", so adding
Strawberry/gqlgen/REST/gRPC would mean editing `resolve` — which invariant 2 (the
adapter seam) forbids. [`10-cross-service-resolution.md`](10-cross-service-resolution.md)
already designed the right shape on paper (`RouteKey` + `FrameworkDetector` + generic
linker); the implementation never followed it.

The goal, by analogy: **what LSP did for editors, do for cross-service** — one fixed
vocabulary that any framework maps onto, so adding a framework is a detector
(tree-sitter queries + a small normalizer) in `crates/lang/`, never a core change.
Explicitly *not* actual LSP: servers cannot see across process/language boundaries
(measured, doc 15).

Second driver: the naming-convention bug class (`2d62f14` — 11 operations lost to
codegen casing). Root cause was that convention normalizers (`camelize`, `decamelize`,
`document_key`) live in `resolve`, where they can only ever encode one framework's
conventions. The rule that fixes this class permanently: **each detector owns the
normalization of its own side of the boundary and emits wire-format keys**; the linker
compares wire names only.

## Design

### Vocabulary (new, in `ir`)

```rust
// crates/ir/src/lib.rs — language-blind, like everything else in ir
pub enum Transport { Http, Graphql, Grpc, Rpc, PubSub, Db }
//                                        ^^^ JSON-RPC etc.: method-name keyed

pub enum Segment { Literal(String), Param, Wildcard }

pub struct RouteKey {
    pub transport: Transport,
    /// "GET"/"POST"; gRPC/RPC method; None where the transport has no method axis
    /// (pub/sub topics, DB entities, GraphQL — its root scope is a path segment).
    pub method: Option<String>,
    /// URL segments, GraphQL selection path, topic split on '.', DB entity name.
    /// Both sides normalize identically so matching is a lookup.
    pub path: Vec<Segment>,
}
```

`EdgeKind::HttpCall / AsyncCall / Emits / GraphqlCall / DbQuery` already exist — no
edge-vocabulary change.

### Facts (reworked, in `lang::cross`)

`CrossFacts` loses the `elixir: Option<ElixirFacts>` field entirely:

```rust
pub struct CrossFacts {
    pub provides: Vec<Provides>,   // { key: RouteKey, handler: HandlerRef, returns: Option<String> }
    pub consumes: Vec<Consumes>,   // { key: RouteKey, line, confidence_hint }
    pub graphql: GraphqlFacts,     // operations, fragments, scope_includes, op_refs
    // late-resolution facts, generically named (was ElixirFacts fields):
    pub star_imports: Vec<String>,                    // was: imports
    pub qualified_calls: Vec<(String, String, u32)>,  // was: remote_calls
    pub entity_refs: Vec<(String, u32)>,              // was: schema_refs
    pub entity_def: bool,                             // was: is_schema
}
pub enum HandlerRef { Function { module: String, name: String }, Module(String) }
```

Mapping of everything that exists today (no information lost):

| today | becomes |
|---|---|
| `AbsintheField` | `Provides { Graphql, ["query","currentPlayer"] / ["Player","team"], Function, returns }` |
| `AbsintheContextField` (dataloader) | `Provides { …, Module(fqn), returns }` |
| `GqlOp` / `GqlFragment` / `GqlSpread` / `scope_includes` / `ts_docs` | `GraphqlFacts` (protocol-level, framework-free) |
| `imports` / `remote_calls` / `schema_refs` / `is_schema` | generic names, resolve logic unchanged |
| `aliases` | already adapter-internal; leaves the public struct |

### Spec files are detectors too (OpenAPI/Swagger, .proto, JSON-RPC)

The boundary contract is often declared in a **spec file**, not code — and that is not
a new mechanism: the `.gql` adapter already *is* a spec-file detector (file globs +
parse + emit facts). OpenAPI and proto follow the same seam:

- an adapter with `file_globs: ["openapi.yaml", "openapi.json", "*.proto"]` parses the
  spec and emits `Provides` (and for generated clients, `Consumes`) — pure `lang/` work;
- OpenAPI paths → `Http` RouteKeys (`{id}` → `Param`); proto `service/rpc` → `Grpc`;
  JSON-RPC method registries → `Rpc` method-name keys;
- a spec that names no code handler emits `HandlerRef::Module` — the dataloader
  precedent: module-granular, priced lower, never dropped;
- spec-vs-code drift shows up in the unmatched counters, which is a *feature*.

These are easier than GraphQL (no type graph, no fragments). GraphQL stays the hard
case the protocol module exists for. No OpenAPI/proto detector is in this plan's scope
— the vocabulary and seam must simply not preclude them, and `Rpc` + spec-file globs
are what that requires.

### Who owns normalization (the naming-convention rule)

- **Absinthe detector** (`lang/elixir/dsl.rs`): `:current_player` → `currentPlayer`,
  `:lfg_post` → `LfgPost`. `camelize`/`pascalize` live here; the linker never sees an atom.
- **TS detector**: `UpdateLfgRequestDocument` → op name; codegen suffix/casing
  conventions live here.
- **Linker** (`resolve/crossservice.rs`): compares wire-format keys only. Op-name
  compare stays first-char-case-insensitive (codegen convention, commented, kept honest
  by the `opcase` test). Knows *protocols* (GraphQL type-graph descent, fragment
  expansion, HTTP segment matching) but **zero framework names** — protocol semantics
  are the same for Strawberry/gqlgen/Absinthe.

### Diagnostics (the unjoined counter)

The linker counts and the index summary prints, per transport: **consumes that matched
no provider, providers never consumed**. This is what would have caught the `2d62f14`
casing bug on day one; any future unknown convention shows up as a number instead of
silence.

## Verification

1. `cargo fmt --all --check && cargo clippy --all-targets && cargo test` per phase.
2. After Phases 1 & 2: `ripple index <web> <api>` must equal
   9179 / 20337 / 846 graphql / 941 db exactly; `eval --oracle lsp --sample 30`
   unchanged (ts 34/35, tsx 32/35, elixir 94/95).
3. After Phase 3: `git diff --stat` confined to `crates/lang/` + fixtures; spot-check
   one HttpCall edge by hand against the router and a fetch site.
4. Mutation checks: break segment matching (Param → must-equal) → http fixture fails;
   remove spread expansion → nested fixture fails (already proven).
5. One commit per phase, evidence in the message.

## Risks / honest limits

- The `CrossFacts` schema change invalidates the extract cache once (by design: no
  `serde(default)` on new fact fields; old rows = cache miss, one cold re-parse ~1.6s).
- GraphQL op-name casing fold stays a linker special case (codegen convention) —
  smallest honest wart, covered by the `opcase` test.
- Phoenix `resources` shorthand: out of scope, under-linked, counted by the diagnostics.
- gRPC/PubSub/OpenAPI get vocabulary but no detector yet — #13 stays open for them,
  unblocked by the seam this builds.
- Consumer base-URL resolution (doc 10's `ServiceId`) deferred: path-only matching at
  0.6 confidence, stated on the edge.
