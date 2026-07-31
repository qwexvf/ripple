; Bare call:  foo()
(call
  function: (identifier) @ref.call)

; Attribute call:  recv.foo()  — receiver plus the attribute being called
(call
  function: (attribute
    object: (_) @ref.recv
    attribute: (identifier) @ref.member))
