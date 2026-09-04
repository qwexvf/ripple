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

## The JVM and C-family adapters

Six adapters landed together — Java, Scala, Kotlin, C#, C and C++. All six are **Tier 2**:
`tags.scm` + `imports.scm` + `refs.scm`, with `resolve_import`/`external_dep_key` in Rust.
None of them has a `bindings_query`, so a receiver's *type* is never inferred: a
`recv.method()` call is resolved by name, not by what `recv` holds. That is the same
receiver-type blindness tracked for Go and Rust in
[#105](https://github.com/qwexvf/ripple/issues/105), not a per-language gap.

| Language | Globs | Tier | Import resolution | Test convention |
|---|---|---|---|---|
| **Java** (`tree-sitter-java`) | `*.java` | 2 | dotted FQN → `com/example/Foo.java`, probed against 8 ancestor dirs | `src/test/`, `*Test.java`, `*Tests.java` |
| **Scala** (`tree-sitter-scala`) | `*.scala`, `*.sc` | 2 | plain `import a.b.C` → `a/b/C.{scala,sc}`, same ancestor walk; selector/wildcard forms go external | `src/test/` |
| **Kotlin** (`tree-sitter-kotlin-ng`) | `*.kt`, `*.kts` | 2 | dotted path → `com/example/Foo.{kt,kts}`, same ancestor walk; `import … as X` binds the alias | `src/test/`, `src/androidTest/` |
| **C#** (`tree-sitter-c-sharp`) | `*.cs` | 2 | **none by design** — a `using` names a namespace, which spans many files, so it always binds an external namespace node | `*Test.cs`, `*Tests.cs`, a `test`/`tests` path segment |
| **C** (`tree-sitter-c`) | `*.c` | 2 | quoted `#include "foo.h"` relative to the including file, also probing `<ancestor>/include/foo.h`; `<stdio.h>` is always external | a `test`/`tests` path segment |
| **C++** (`tree-sitter-cpp`) | `*.cpp`, `*.cc`, `*.cxx`, `*.hpp`, `*.hh`, `*.hxx`, `*.h` | 2 | as C | a `test`/`tests` path segment |

Notes worth the line:

- **`.h` belongs to the C++ adapter, not the C one.** The extension is ambiguous and the
  dominant C++ convention uses it, while `tree-sitter-c` cannot parse a `namespace` or a
  `template` — so with C claiming the glob, a C++ header extracted *nothing*: identical
  content yielded 0 symbols as `.h` and 5 as `.hpp`, and fmt's `basic_format_arg` was
  absent from the index entirely
  ([#119](https://github.com/qwexvf/ripple/issues/119)). The C++ grammar is very nearly a
  superset of C, so a genuine C header still extracts correctly under it.
- **Members are qualified by their owner.** Java, Kotlin and C# prefix a method or field
  with its enclosing type declaration, and Scala prefixes a method or `val` with its
  owning `class`/`trait`/`object`/`enum`, so `Widget.Name` and `Order.Name` are
  distinct symbols and a field never hashes to the same `SymbolId` as a same-named
  function. C has no methods, so only struct/union fields are qualified (`Point.x`). C++
  reads the owner off the declarator: an out-of-line `void Foo::bar(){}` names it inline,
  an inline member takes the enclosing `class_specifier`.
- **The grammars force the tags query to do the sorting.** Scala shares
  `function_definition` between free functions and methods; Kotlin has no method or field
  node at all (a member is a `function_declaration` that happens to sit in a
  `class_body`); tree-sitter-cpp inherits C's `function_definition` for both free and
  member functions. In each case the split is by scope inside `tags.scm`, not by node kind.
- **Visibility maps to whatever the language actually has.** `public` for Java and C#;
  public-unless-`private`/`protected` for Scala; public-unless-`private`/`internal`/
  `protected` for Kotlin; and for C/C++ *linkage* stands in — a top-level symbol is
  exported unless it is `static`. C++ class access specifiers (`private:`) are not
  tracked; a member defaults to visible.
- **C and C++ bare calls resolve against the whole program.** An `#include` binds a
  *file*, never the names in it, so file scope plus imports left a `.h`/`.c` split
  project with a same-file-only call graph — in jq, all 68 callers of `jv_free` sat in
  `src/jv.c` while `src/execute.c` called it 59 times
  ([#116](https://github.com/qwexvf/ripple/issues/116)). C and C++ have one flat
  namespace for external linkage, so the C/C++ adapters answer `true` to
  `LanguageAdapter::bare_calls_resolve_globally` and an unqualified call that binds to
  nothing in scope falls back to a project-wide lookup over exported file-scope names,
  at confidence 0.8 divided by the number of candidates. Only *exported* definitions
  qualify, so a `static` function keeps its internal linkage and is unreachable from
  another file; a class member is excluded too, since its qualified name (`Foo.bar`)
  is not a linker name. Every other adapter answers `false`, and must: matching a bare
  name project-wide in a language whose names live in modules fabricates edges.
- **A C++ member prototype is not a definition.** `void bar();` in a header is a
  declaration, so it is not captured. It used to be, and because `SymbolId` is keyed by
  `(module_path, qualified_name)` the header's prototype and the source's
  `void Foo::bar(){}` became two rival nodes — a cross-file `f.bar()` split 1/N across
  them instead of pinning the definition.
- **Build-tool source roots are not discovered.** `resolve_import` walks up a bounded run
  of ancestor directories and probes for the file, which implicitly finds
  `src/main/java`-style roots when package == directory. A Maven/Gradle/CMake layout that
  breaks that assumption resolves to nothing and falls back to an external node —
  [#114](https://github.com/qwexvf/ripple/issues/114).
- **The Kotlin grammar errors on single-line class bodies.** `class Foo { fun bar() {} }`
  on one line parses with an error and under-captures its members —
  [#109](https://github.com/qwexvf/ripple/issues/109).

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

Predicates: `#eq?`, `#not-eq?`, `#any-of?`, `#not-any-of?`, `#match?` and
`#not-match?` are evaluated. Anything else is **rejected when the query is
compiled** rather than ignored at match time — an unevaluated predicate passes,
so a query that filters on one silently matches everything. That is not
hypothetical: the JSX capitalisation filter spent months matching every `<div>`,
and the edges it invented only showed up when a codebase had a function named
`main`.

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

## Files that hold more than one language

A single-file component — a `.vue` or `.svelte` file, or plain HTML with an inline
`<script>` — is a template plus a script block that is really TypeScript. One
adapter method carries that: `embedded_regions` returns the `(adapter id, range)`
of each foreign region in the host file's own byte+point coordinates. `parse`
re-parses each range with the named adapter using tree-sitter `included_ranges`, so
the region's nodes report positions in the host file — there is no span arithmetic
to get wrong, which is the whole risk. The region's symbols, imports, refs and
cross-service facts merge into the host file's extract as if they had been written
there. Import resolution stays keyed off the host path, so the host adapter answers
`resolve_import`; the HTML adapter delegates to TypeScript's, since a `<script>`
import names a `.ts` file. The `html` adapter is the minimal proof of this seam; the
Vue and Svelte adapters are `embedded_regions` plus a template `tags.scm`.

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

- **Review rule, not a CI check.** This was written as though a test enforced it; nothing does. `ci.yml` runs fmt, clippy, the test suite and the docs build. The property is real — the Python adapter (2026-07-31) touched `crates/lang/`, a fixture and a doc line, nothing else — but it is defended by reading the diff, not by the build. Writing the check is worth doing; claiming it exists is not.
- **Contract tests:** each adapter runs against a shared golden-fixture suite (a tiny repo per language) asserting expected nodes/edges per tier. Adding a language = adding a fixture + expected output.
- **Tier conformance:** an adapter declares its tier; the suite only runs the checks valid for that tier, so a Tier-0 language isn't failed for lacking a call graph.

Contract tests and tier conformance *are* in CI. The confinement rule above is not, and this section said otherwise until someone checked.
