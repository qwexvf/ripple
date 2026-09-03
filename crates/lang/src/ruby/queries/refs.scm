; Ruby Tier-2 call *sites*. Ruby dispatch is dynamic, so this query's job is to
; say "a call named X happens here", not to pick a target — the resolver binds by
; name and splits confidence across candidates.
;
; Only `call` nodes are captured. A paren-less bare call (`other_method`) parses
; as a lone `(identifier)`, which is exactly how a local variable read parses; the
; two are indistinguishable in the grammar, so capturing identifiers would mint a
; call for every variable mention. Those calls are dropped on purpose.

; Bare call:  helper(x)
; `!receiver` is load-bearing — without it this pattern also matches `obj.foo`,
; since a pattern that omits a field still matches a node that has it.
(call
  !receiver
  method: (identifier) @ref.call)

; Receiver call:  obj.foo(x)  /  Foo.bar(1)  /  x&.foo(1)
; The receiver is any expression (an identifier, a constant, `self`, a
; `Foo::Bar` scope resolution); resolution decides what, if anything, it binds to.
(call
  receiver: (_) @ref.recv
  method: (identifier) @ref.member)
