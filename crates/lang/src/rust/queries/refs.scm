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

; Macro arguments. `assert_eq!(parse(x), 1)` is a token tree, not an expression, so
; none of the patterns above see the call inside it; a function whose only test
; calls it from `assert!` read as untested (#150). Tokens carry no structure, so
; a call is an identifier immediately followed by a parenthesized group. A token
; matched by more than one of these keeps the most specific reading (see
; `parse::extract_refs`).
(token_tree (identifier) @ref.call . (token_tree))
(token_tree
  (identifier) @ref.qualifier . "::" . (identifier) @ref.call . (token_tree))
(token_tree
  [(identifier) (self)] @ref.recv . "." . (identifier) @ref.member . (token_tree))
