; Go Tier-0 definition captures. There is deliberately no refs.scm: Go is the
; breadth-proof language (docs/11 phase 4), so its call edges come from gopls and
; these captures exist only to give those calls a symbol to land in.

(function_declaration
  name: (identifier) @name) @def.function

(method_declaration
  name: (field_identifier) @name) @def.method

(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (struct_type))) @def.class

(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (interface_type))) @def.interface

; Everything else a `type` can name (`type Offset int`, `type Fn func(...)`).
; Spelled as an alternation rather than a bare `type_spec` so a struct is not
; captured twice, once here and once above.
(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: [
      (type_identifier)
      (qualified_type)
      (pointer_type)
      (array_type)
      (slice_type)
      (map_type)
      (channel_type)
      (function_type)
    ])) @def.type

(type_declaration
  (type_alias
    name: (type_identifier) @name)) @def.type

; Package-level bindings only — a local `var` inside a function is not a symbol
; anything else can reference.
(source_file
  (const_declaration
    (const_spec
      name: (identifier) @name) @def.variable))

(source_file
  (var_declaration
    (var_spec
      name: (identifier) @name) @def.variable))
