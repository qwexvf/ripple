; Bare call:  helper(x)
(call_expression
  function: (identifier) @ref.call)

; Member call:  obj.m(x)  /  obj->m(x)  — both parse as a field_expression whose
; argument is the receiver and whose field is the method name.
(call_expression
  function: (field_expression
    argument: (_) @ref.recv
    field: (field_identifier) @ref.member))
