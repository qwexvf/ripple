; Gleam Tier-0 definition captures. Like Go (docs/11 phase 4) there is no
; refs.scm: call edges come from `gleam lsp` via `index --calls lsp`, and these
; captures exist to give those calls a symbol to land in.
;
; Gleam has no methods — every function is module-level — so nothing here needs
; qualifying by a receiver.

(function
  name: (identifier) @name) @def.function

(external_function
  name: (identifier) @name) @def.function

; `type Foo { Bar Baz }` — the type itself, then each constructor. Constructors
; are captured because Gleam code calls them like functions (`Bar(1)`), so a
; reported call has to have somewhere to land.
(type_definition
  (type_name
    name: (type_identifier) @name)) @def.type

(type_definition
  (data_constructors
    (data_constructor
      name: (constructor_name) @name) @def.function))

(type_alias
  (type_name
    name: (type_identifier) @name)) @def.type

(external_type
  (type_name
    name: (type_identifier) @name)) @def.type

(constant
  name: (identifier) @name) @def.variable
