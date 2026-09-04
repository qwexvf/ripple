; Scala imports. An `import_declaration` spells the package path as a run of
; `path:` identifiers separated by anonymous `.` tokens (`scala.collection.mutable`),
; optionally ending in a selector list (`{Map, Set}` / `{C => D}`) or a wildcard
; (`_` / `*`). No single node holds the whole dotted path, so the whole
; declaration is captured as `@import.source` ("import a.b.C") and the adapter's
; `resolve_import`/`external_dep_key` strip the `import` keyword back off it.
;
; The builder emits one record per `@import.name`, all sharing the `@import.source`
; specifier — so a plain `import a.b.C` becomes a NAMED import of `C` from the
; `a.b.C` path (resolve_import maps that to `a/b/C.scala`), while selector and
; wildcard forms are captured as `@import.namespace` and fall through to
; `external_dep_key`, minting an external node keyed by the package prefix.
;
; The identifier patterns deliberately omit the `path:` field name: a trailing `.`
; anchor only pins "the last child" when the node is matched positionally, not by
; field (a field-qualified child + trailing anchor matches nothing here).

; `import a.b.C` — the trailing `.` anchor pins the LAST identifier child, so this
; only fires when no selector/wildcard follows. That last segment is the imported
; name; the whole declaration text is the specifier.
(import_declaration
  (identifier) @import.name
  .) @import.source

; `import a.b.{Map, Set}` / `import a.b.{C => D}` — selector list. The path
; segment right before the braces (the `.` token pins it as the last) names the
; local package; the form resolves to an external node.
(import_declaration
  (identifier) @import.namespace
  .
  "."
  .
  (namespace_selectors)) @import.source

; `import a.b._` / `import a.b.C.*` — wildcard. Same as selectors: an external
; namespace import keyed by the package prefix.
(import_declaration
  (identifier) @import.namespace
  .
  "."
  .
  (namespace_wildcard)) @import.source
