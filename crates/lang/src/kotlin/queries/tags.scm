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

(class_declaration
  name: (identifier) @name
  (enum_class_body)) @def.enum

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
