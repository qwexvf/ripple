; Bare call:  helper(x)
(call_expression
  function: (identifier) @ref.call)

; Selector call:  pkg.Foo(x)  /  recv.Method(x)  — the operand is the receiver,
; the field is the name being called. A package-qualified call and a method call
; share this shape; resolution tells them apart by whether the receiver binds to
; an imported package (external member call) or a local value.
(call_expression
  function: (selector_expression
    operand: (_) @ref.recv
    field: (field_identifier) @ref.member))
