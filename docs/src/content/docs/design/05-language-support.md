---
title: "05 — Language support: adding a language later is cheap"
description: "The Tier system, .scm conventions, monorepo handling, and the extensibility guarantee"
sidebar:
  label: "05 — Language support"
  order: 5
---
The core promise: **a new language is a self-contained folder under `crates/lang/adapters/` plus one line in the registry.** No layer above `ir` changes. This doc explains how that holds, and how each language becomes useful *incrementally* instead of all-or-nothing.

> **As-built honesty (see [`09`](09-review-and-corrections.md) round 2).** The promise is *fully* true for **Tier 0–1** (symbols + git overlay + imports): a new language is a folder + `.scm` + a registry line and gets same-day value. Per-language behaviors that used to leak into `parse` — `is_exported`, method name-qualification — are now `LanguageAdapter` methods, so the seam holds. **Tier 2** (call resolution) and **Tier 3** (cross-service) need a small per-language implementation, not free `.scm` data. But the framework part is now data too: for macro-based languages the *shape* is generic (`lang::elixir::macros` reads `name :atom, opts do ... end` blocks with no framework knowledge) and only the *mapping* is per-framework — a table in `lang::elixir::dsl` naming which macro opens a type block, declares a member, or names a resolver. Adding Ash, a Phoenix router, or LiveView is a table entry; see [`09` round 3](09-review-and-corrections.md).

## The Tier system

A language is not "supported" or "unsupported." It climbs tiers, and it's useful at every tier — even Tier 0.

| Tier | You add | You get | Effort |
|---|---|---|---|
| **0** | grammar + `tags.scm` | File & symbol nodes, `Defines` edges, complexity — **plus the entire git overlay** (churn, co-change, risk, review targeting at file granularity) | minutes |
| **1** | `imports.scm` + `resolve_import` | `Imports` edges, cross-file linking | hours |
| **2** | `refs.scm` + scoping policy | `Calls`/`References` edges — the call graph | 1–2 days |
| **2′** | a language server, via `index --calls lsp` | `Calls` edges at `LspVerified`/0.7 with no `refs.scm` at all (Go/gopls), or `References` edges where the server has no call hierarchy (Gleam/`gleam lsp`) — see [`11` phases 4–5](11-lsp-integration.md) | minutes, if a server exists |
| **3** | `FrameworkDetector`s | `HttpCall`/`GraphqlCall`/`Emits` cross-service edges — full design in [`10-cross-service-resolution.md`](10-cross-service-resolution.md) | per framework |

### Why Tier 0 already delivers the product

This is the decisive design synergy. The git overlay — churn, co-change, bug-density, ownership, and therefore the risk score and `review_focus` — operates at **file granularity** and is **completely language-agnostic** (it reads `git log`, not ASTs). So:

> A language with *only* Tier 0 support still answers `impact(diff)` and `review_focus(pr)` usefully, because co-change coupling fills in for the missing static edges.

Contrast with every other tool, which is worthless for a language until someone writes a full resolver. Here, dropping in a tree-sitter grammar + a `tags.scm` gives same-day value, and precision improves as the language climbs tiers. The shallow-static-analysis gap is covered by the git signal — exactly the bet in [`02-gap.md`](02-gap.md).

## `.scm` query conventions

Adapters are mostly **data**. A `tags.scm` maps tree-sitter captures to IR node kinds by capture name:

```scheme
; adapters/typescript/queries/tags.scm
(function_declaration name: (identifier) @name) @def.function
(method_definition   name: (property_identifier) @name) @def.method
(class_declaration   name: (type_identifier) @name) @def.class
(interface_declaration name: (type_identifier) @name) @def.interface
```

The `parse` layer reads the capture prefix (`def.function` → `NodeKind::Function`) generically — it doesn't know TypeScript. Convention table (fixed across all languages):

| capture prefix | IR |
|---|---|
| `def.function` / `def.method` / `def.class` / `def.interface` / `def.type` / `def.enum` / `def.field` | corresponding `NodeKind` |
| `import` (in `imports.scm`) | `Imports` edge (resolved via `resolve_import`) |
| `ref.call` / `ref.use` (in `refs.scm`) | `Calls` / `References` edge (resolved via scoping) |
| `@name` | the symbol name within a `@def.*` |

Test convention is the other piece of language knowledge here, and it comes in two
shapes because one doesn't cover the other. By path — `*.test.ts`, `*_test.go`,
`test/**` — it is `LanguageAdapter::is_test_path`, a string predicate. Inside the
file — Rust's `#[cfg(test)] mod tests` — no path can see it, so the adapter returns
the regions itself from `test_scopes`. That one is deliberately *not* a `.scm`
capture: the attribute is a preceding sibling of the module, so the pattern needs an
anchor, and the anchor then breaks on `#[cfg(test)] #[allow(…)] mod tests`; matching
the attribute text needs a regex predicate, and `predicates_hold` treats an
unsupported predicate as passing, which would mark every `mod` in a repo as tests.
Either way `resolve::link_tests` turns "a call leaves the test side" into a `Tests`
edge without knowing which language produced it.

Adding a Tier-0 language ≈ writing this one file. No Rust beyond the 4 required trait methods.

## Import resolution is the language-specific part

Module systems differ, so `resolve_import` is where per-language code actually lives. Defaults handle relative paths; override for the rest:

```rust
impl LanguageAdapter for typescript::Adapter {
    fn resolve_import(&self, spec: &str, from: &Path, ws: &Workspace) -> Option<PathBuf> {
        // ./foo, ../bar → relative; "@app/x" → tsconfig paths; "pkg" → node_modules or workspace pkg
        resolve_ts_module(spec, from, ws.tsconfig(), ws.package_json())
    }
}
```

This is the honest cost of a language: its module-resolution rules. Everything else is data or default.

## Monorepo handling

Monorepos are first-class, not an afterthought:

- **Workspace discovery.** The indexer detects package boundaries (`package.json`/`pnpm-workspace.yaml`, `Cargo.toml` workspaces, `go.work`, Nx/Turbo config, `gleam.toml`) and models each as a scope. `resolve_import` receives a `Workspace` so cross-package imports resolve to the right package's files.
- **Language mixing.** One repo, many adapters. File globs route each file to its adapter; the IR is uniform, so a TS frontend calling a Go service over HTTP becomes a single graph with `HttpCall` edges bridging them (Tier 3).
- **Cross-package co-change.** The git overlay naturally spans packages — it's the strongest signal for "these two packages always move together," which package DAG tools (Nx/Turbo) can't see because there's no declared dependency.
- **Scoped queries.** `impact(diff)` and `review_focus(pr)` accept a package/path filter so a query in one package doesn't drag the whole monorepo into RAM.

## Extensibility guardrail {#extensibility-guardrail}

The promise ("adding a language touches only `lang/`") is enforced mechanically, not by discipline:

- **CI check:** a test asserts that adding a fixture language's adapter produces graph output for a sample repo *without any diff outside `crates/lang/`*. If a new language forces a change in `resolve`/`store`/`overlay`/`query`, that's an abstraction leak — the build warns.
- **Contract tests:** each adapter runs against a shared golden-fixture suite (a tiny repo per language) asserting expected nodes/edges per tier. Adding a language = adding a fixture + expected output.
- **Tier conformance:** an adapter declares its tier; the suite only runs the checks valid for that tier, so a Tier-0 language isn't failed for lacking a call graph.

This turns "clean architecture" from an aspiration into a property the CI defends on every PR.
