; Kotlin receiver types, read straight off what the source writes down. No
; inference: `variable_declaration` carries no named fields in this grammar, so
; the shapes are pinned with anchors — the name is the first child, an explicit
; type the second. A nullable (`Foo?`) or function type is not a `user_type` and
; is deliberately not captured.

; `val x: Foo = …` / `var x: Foo` — a property or a local, same node either way.
(variable_declaration
  . (identifier) @bind.name
  . (user_type
      (identifier) @bind.type))

; `val x = Foo()` — no written type, so the constructor is it. The anchor after
; the name requires the declaration to be untyped, so this can never contradict
; the pattern above. `val x = mk()` names a *function*, and there the class lookup
; simply misses, leaving resolution where it is today.
(property_declaration
  (variable_declaration
    . (identifier) @bind.name .)
  (call_expression
    . (identifier) @bind.ctor))

; `fun m(x: Foo)`
(parameter
  . (identifier) @bind.name
  . (user_type
      (identifier) @bind.type))
