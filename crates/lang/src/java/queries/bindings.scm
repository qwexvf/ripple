; Java receiver types, read straight off what the source writes down. No
; inference: a declaration whose type is not a plain `type_identifier`
; (`List<Foo>` is a `generic_type`, `Foo[]` an `array_type`, `int` a
; `integral_type`) is deliberately not captured, and the member call falls back to
; by-name candidates at its existing confidence.

; `Foo x = …;` / `Foo x;`. `var` is spelled as a `type_identifier` by this
; grammar, so exclude it here and let the constructor pattern below type it.
(local_variable_declaration
  type: (type_identifier) @bind.type
  declarator: (variable_declarator
    name: (identifier) @bind.name)
  (#not-eq? @bind.type "var"))

; `var x = new Foo();` — the constructor is the only written-down type.
(local_variable_declaration
  type: (type_identifier) @_var
  declarator: (variable_declarator
    name: (identifier) @bind.name
    value: (object_creation_expression
      type: (type_identifier) @bind.ctor))
  (#eq? @_var "var"))

; `void m(Foo x)`
(formal_parameter
  type: (type_identifier) @bind.type
  name: (identifier) @bind.name)

; `private Foo x;`
(field_declaration
  type: (type_identifier) @bind.type
  declarator: (variable_declarator
    name: (identifier) @bind.name))
