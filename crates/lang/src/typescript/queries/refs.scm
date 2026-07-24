; Bare call:  foo()
(call_expression
  function: (identifier) @ref.call)

; Member call:  <recv>.foo()  — capture receiver + method name
(call_expression
  function: (member_expression
    object: (_) @ref.recv
    property: (property_identifier) @ref.member))
