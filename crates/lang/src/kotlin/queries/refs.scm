; Bare call:  helper(x)  — the callee is a direct identifier child.
(call_expression
  . (identifier) @ref.call)

; Navigation call:  recv.member(x)  — the operand is the receiver, the trailing
; identifier is the member being called. `recv` may itself be a nested navigation
; (`a.b.c()`) or a call (`make().chain()`); resolution decides whether it binds to
; an imported name (external member call) or a local value.
(call_expression
  . (navigation_expression
      (_) @ref.recv
      .
      (identifier) @ref.member))
