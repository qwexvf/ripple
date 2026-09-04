; Scala receiver types, read straight off what the source writes down. Only the
; ascribed forms: `val x: Foo`, `var x: Foo`, `def m(x: Foo)`. An inferred `val x
; = …` is deliberately not captured — Scala's whole idiom is to leave the type
; off, and guessing it from the initializer is inference, not reading. A
; `generic_type`/`compound_type` ascription is likewise skipped.

(val_definition
  pattern: (identifier) @bind.name
  type: (type_identifier) @bind.type)

(var_definition
  pattern: (identifier) @bind.name
  type: (type_identifier) @bind.type)

(val_declaration
  name: (identifier) @bind.name
  type: (type_identifier) @bind.type)

(parameter
  name: (identifier) @bind.name
  type: (type_identifier) @bind.type)
