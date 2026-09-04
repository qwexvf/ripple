; C# receiver types, read straight off what the source writes down. No
; inference: a type that is not a plain `identifier` (`List<Foo>` is a
; `generic_name`, `System.Text.Foo` a `qualified_name`, `int` a `predefined_type`)
; is deliberately not captured, and the member call falls back to by-name
; candidates at its existing confidence.

; `Foo x = …;` — one `variable_declaration` node serves both a local
; (`local_declaration_statement`) and a field (`field_declaration`), so this
; single pattern covers both.
(variable_declaration
  type: (identifier) @bind.type
  (variable_declarator
    name: (identifier) @bind.name))

; `var x = new Foo();` — `var` is its own node kind here, so there is no
; ambiguity with a real type: the constructor is the only type written down.
(variable_declaration
  type: (implicit_type)
  (variable_declarator
    name: (identifier) @bind.name
    (object_creation_expression
      type: (identifier) @bind.ctor)))

; `void M(Foo x)`
(parameter
  type: (identifier) @bind.type
  name: (identifier) @bind.name)
