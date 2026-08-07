; Gleam imports. A module is imported by its path (`import nande_ingest/scrub`),
; which binds the last path segment as an alias (`scrub`) unless renamed with
; `as`. The `.{ ... }` clause additionally brings names into scope unqualified.

; The module alias: `import a/b/scrub` → `scrub`, `import a/b as x` → `x`. The
; whole `module` node is the specifier; the local name is derived from it (last
; segment) or from an explicit `as` alias. Matches every import, including one
; that also has a `.{ }` clause — Gleam keeps the alias usable in that case too.
(import
  module: (module) @import.source @import.namespace
  alias: (identifier)? @import.alias)

; Unqualified names: `import a/b.{ foo, bar as baz }`. Each brings one name into
; scope, aliased locally by `as` just like a TypeScript named import.
(import
  module: (module) @import.source
  (unqualified_imports
    (unqualified_import
      name: (identifier) @import.name
      alias: (identifier)? @import.alias)))
