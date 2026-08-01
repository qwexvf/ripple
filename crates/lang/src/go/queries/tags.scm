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

; Struct fields. `item.Name` is a reference a server can report, so the field
; needs a symbol of its own; `qualified_name` prefixes the owning type, so
; `Item.Name` and `Order.Name` stay distinct and neither collides with a
; package-level `Name`.
;
; Anchored on the named `type_spec` on purpose: the fields of an anonymous
; `struct{...}` literal inside a function body are not addressable from
; anywhere else, and the fields of a struct nested inside another struct have
; no single-segment owner to qualify them by.
(type_declaration
  (type_spec
    name: (type_identifier)
    type: (struct_type
      (field_declaration_list
        (field_declaration
          name: (field_identifier) @name) @def.field))))

; Interface methods. Same reasoning as fields: a call through an interface is
; reported against `Doer.Do`, which has to exist as a symbol. `method_elem` has
; no receiver, so `qualified_name` falls back to the enclosing `type_spec`.
(type_declaration
  (type_spec
    name: (type_identifier)
    type: (interface_type
      (method_elem
        name: (field_identifier) @name) @def.method)))

; Package-level bindings only — a local `var` inside a function is not a symbol
; anything else can reference.
;
; The name is matched positionally rather than by field: `const A, B = 1, 2`
; puts both identifiers under one `name` field, and a field pattern yields only
; the first of them. Nothing else a `const_spec` holds directly is a bare
; `identifier` — the type is a `type_identifier` and the value is wrapped in an
; `expression_list` — so the looser pattern captures names and only names.
(source_file
  (const_declaration
    (const_spec
      (identifier) @name) @def.variable))

(source_file
  (var_declaration
    (var_spec
      name: (identifier) @name) @def.variable))

; `var ( … )` puts a `var_spec_list` between the declaration and its specs, so
; the pattern above misses every grouped package-level var.
(source_file
  (var_declaration
    (var_spec_list
      (var_spec
        name: (identifier) @name) @def.variable)))
