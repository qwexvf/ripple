; Scala Tier-0 definition captures.
;
; Scala has no syntactic split between a top-level function and a method the way
; Go does — both are `function_definition`. Context does the splitting: a def
; directly inside a `template_body` (a class/trait/object body) is a method, one
; directly inside the `compilation_unit` is a free function. `qualified_name`
; then prefixes members with their owning type so same-named methods and members
; on different owners stay distinct and don't collide with a top-level def.

; --- types ---

; a `case class` is still a `class_definition`; an `object` is a singleton — both
; are classes for our purposes.
(class_definition
  name: (identifier) @name) @def.class

(object_definition
  name: (identifier) @name) @def.class

(trait_definition
  name: (identifier) @name) @def.interface

(enum_definition
  name: (identifier) @name) @def.enum

(type_definition
  name: (type_identifier) @name) @def.type

; --- free functions (module scope) ---

(compilation_unit
  (function_definition
    name: (identifier) @name) @def.function)

; --- methods (inside a class/trait/object/enum body) ---
; `function_declaration` is the abstract form (a `def` with no body, as in a
; trait); `function_definition` is the concrete one.
(template_body
  (function_definition
    name: (identifier) @name) @def.method)

(template_body
  (function_declaration
    name: (identifier) @name) @def.method)

(enum_body
  (function_definition
    name: (identifier) @name) @def.method)

(enum_body
  (function_declaration
    name: (identifier) @name) @def.method)

; --- values (module scope + members only) ---
; A `val`/`var` inside a method body sits under a `block`, not a `template_body`
; or the `compilation_unit`, so those local bindings are deliberately left out —
; nothing else can reference them.
(compilation_unit
  (val_definition
    pattern: (identifier) @name) @def.variable)

(compilation_unit
  (var_definition
    pattern: (identifier) @name) @def.variable)

(template_body
  (val_definition
    pattern: (identifier) @name) @def.variable)

(template_body
  (var_definition
    pattern: (identifier) @name) @def.variable)
