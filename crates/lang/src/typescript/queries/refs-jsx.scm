; JSX usage:  <Input />, <Foo.Bar />
;
; Rendering a component invokes it, so this is a call — without it a TSX blast
; radius stops at the import edge, which says the file imported the component, not
; that anything renders it (issue #26).
;
; Only capitalised names: `<div>` and friends are intrinsic elements that resolve to
; nothing, and emitting refs for them would be noise the resolver has to reject.
((jsx_opening_element
  name: (identifier) @ref.call)
 (#match? @ref.call "^[A-Z]"))

((jsx_self_closing_element
  name: (identifier) @ref.call)
 (#match? @ref.call "^[A-Z]"))

; <Foo.Bar /> — the qualifier resolves like a member call's receiver
(jsx_opening_element
  name: (member_expression
    object: (_) @ref.recv
    property: (property_identifier) @ref.member))

(jsx_self_closing_element
  name: (member_expression
    object: (_) @ref.recv
    property: (property_identifier) @ref.member))
