; Bare call:  helper(x)  — no receiver.
(method_invocation
  !object
  name: (identifier) @ref.call)

; Selector call:  recv.method(x)  /  Pkg.staticMethod(x)  — the object is the
; receiver, the name is the method being called. Resolution tells a member call
; from a static/type-qualified call apart by what the receiver binds to.
(method_invocation
  object: (_) @ref.recv
  name: (identifier) @ref.member)
