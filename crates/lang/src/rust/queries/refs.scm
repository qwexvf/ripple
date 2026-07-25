; Unqualified calls: `helper(x)`.
(call_expression function: (identifier) @ref.call)

; Path calls: `Type::new(x)`, `module::helper(x)`. The last segment is the name;
; resolution treats it like any other call, so `Foo::new` links to a `new` defined
; in scope. Coarse but honest — real path resolution needs the module tree.
(call_expression
  function: (scoped_identifier name: (identifier) @ref.call))

; Method calls: `client.request(...)` — receiver captured for type-based resolution.
(call_expression
  function: (field_expression
    value: (_) @ref.recv
    field: (field_identifier) @ref.member))
