; Bare call:  Helper(x)
(invocation_expression
  function: (identifier) @ref.call)

; Member call:  recv.Method(x)  —  the operand is the receiver, the field is
; the name being called. Resolution decides whether the receiver binds to an
; imported namespace (external member call) or a local value.
(invocation_expression
  function: (member_access_expression
    expression: (_) @ref.recv
    name: (identifier) @ref.member))
