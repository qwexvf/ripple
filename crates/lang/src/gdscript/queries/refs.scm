; `helper()`: a call on a bare identifier.
(call (identifier) @ref.call)

; `obj.method()` and `ClassName.method()`: the receiver is the identifier written
; immediately before the called attribute. A chain (`a.b.c()`) keeps only the
; first link's receiver, so later links fall back to by-name candidates.
(attribute (identifier) @ref.recv . (attribute_call (identifier) @ref.member))
