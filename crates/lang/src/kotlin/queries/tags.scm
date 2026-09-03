; Kotlin Tier-0 definition captures.
;
; Everything is parent-scoped: a symbol another file can reach is declared at the
; top level or as a member of a class/object/interface, never as a local inside a
; function body. Anchoring each def under `source_file` / `class_body` /
; `enum_class_body` drops locals the way the Go query restricts to package scope,
; and it splits top-level functions/properties (`@def.function`/`@def.variable`)
; from members (`@def.method`/`@def.field`) so `qualified_name` can prefix the
; owner onto the members and keep `Owner.member` distinct from a top-level name.

; ── types ──────────────────────────────────────────────────────────────────
; An `enum class` reuses `class_declaration`, told apart only by its
; `enum_class_body`. tree-sitter has no negated-child operator, so a plain
; `(class_declaration "class" …)` would capture an enum too and mint a second,
; colliding symbol. The grammar fixes the child order, so a non-enum class ends
; with one of the members below (or, bodyless, with its own name) while an enum
; always ends with `enum_class_body` — the end-anchor `.` is what keeps them apart.
(class_declaration
  "class"
  name: (identifier) @name
  [
    (class_body)
    (type_constraints)
    (delegation_specifiers)
    (primary_constructor)
    (type_parameters)
  ] .) @def.class

(class_declaration
  "class"
  name: (identifier) @name .) @def.class

(class_declaration
  "interface"
  name: (identifier) @name) @def.interface

; A real enum always declares at least one `enum_entry`. Requiring one matters
; because the grammar *misparses a single-line class body* as an `enum_class_body`
; wrapping an `ERROR` (`class C { fun m() {} }` →
; `(class_declaration name: (identifier) (enum_class_body (ERROR …)))`, see #109).
; Without the `enum_entry` requirement such a class was labelled `Enum`, and a
; single-line `interface` matched here *as well as* the interface pattern below —
; two nodes with one name in one module, so their `SymbolId`s collided and one
; silently clobbered the other. The cost of the requirement is that a pointlessly
; empty `enum class E {}` goes uncaptured, which beats mislabelling a real class.
(class_declaration
  name: (identifier) @name
  (enum_class_body (enum_entry))) @def.enum

; The other half of that misparse: a single-line class body arrives as an
; `enum_class_body` holding an `ERROR`, so neither class pattern above matches it
; (both expect a `class_body` or a bodyless declaration). Match the broken shape
; explicitly so the type itself is still indexed — its members are inside the
; `ERROR` and stay unreachable until the grammar is fixed upstream.
(class_declaration
  "class"
  name: (identifier) @name
  (enum_class_body (ERROR))) @def.class

(object_declaration
  name: (identifier) @name) @def.class

; ── functions ────────────────────────────────────────────────────────────────
(source_file
  (function_declaration
    name: (identifier) @name) @def.function)

(class_body
  (function_declaration
    name: (identifier) @name) @def.method)

(enum_class_body
  (function_declaration
    name: (identifier) @name) @def.method)

; ── properties ───────────────────────────────────────────────────────────────
(source_file
  (property_declaration
    (variable_declaration
      (identifier) @name)) @def.variable)

(class_body
  (property_declaration
    (variable_declaration
      (identifier) @name)) @def.field)

(enum_class_body
  (property_declaration
    (variable_declaration
      (identifier) @name)) @def.field)
