; Bare call:  helper(x)  — no receiver.
(call_expression
  function: (identifier) @ref.call)

; Selector call:  recv.method(x)  /  Obj.method(x)  — the value is the receiver,
; the field is the method being called. Resolution tells a member call from a
; type/object-qualified call apart by what the receiver binds to.
(call_expression
  function: (field_expression
    value: (_) @ref.recv
    field: (identifier) @ref.member))
