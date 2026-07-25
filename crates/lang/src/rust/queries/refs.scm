; Unqualified calls: `helper(x)`.
(call_expression function: (identifier) @ref.call)

; Path calls: `Type::new(x)`, `module::helper(x)`. The qualifier is captured too,
; so resolution can prefer a definition that actually belongs to it rather than
; linking every `new()` to every `new` in the graph.
(call_expression
  function: (scoped_identifier
    path: (_) @ref.qualifier
    name: (identifier) @ref.call))

; Method calls: `client.request(...)` — receiver captured for type-based resolution.
(call_expression
  function: (field_expression
    value: (_) @ref.recv
    field: (field_identifier) @ref.member))
