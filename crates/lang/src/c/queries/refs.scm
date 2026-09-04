; Bare call:  helper(x)
(call_expression
  function: (identifier) @ref.call)

; Member call through `s.field(...)` or `p->field(...)`. C has no methods, but a
; struct member that holds a function pointer is called this way; the argument is
; the receiver, the field is the name being called.
(call_expression
  function: (field_expression
    argument: (_) @ref.recv
    field: (field_identifier) @ref.member))
