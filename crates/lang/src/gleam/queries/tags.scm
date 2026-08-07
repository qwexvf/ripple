; Gleam definition captures. Call edges come from `refs.scm` (static, no LSP);
; these captures give those calls a symbol to land in.
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

; Labelled constructor arguments are record fields: `Person(name: String)` is
; what makes `person.name` legal, so the label is addressable and the positional
; form (`Circle(Float)`) declares nothing to address. Qualified by the declaring
; type in `qualified_name` — `name` on two types in one module is two symbols.
(type_definition
  (data_constructors
    (data_constructor
      (data_constructor_arguments
        (data_constructor_argument
          label: (label) @name) @def.field))))

(type_alias
  (type_name
    name: (type_identifier) @name)) @def.type

(external_type
  (type_name
    name: (type_identifier) @name)) @def.type

(constant
  name: (identifier) @name) @def.variable
