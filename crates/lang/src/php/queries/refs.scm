; PHP Tier-2 call *sites*. A PHP call's target is Composer-autoload-dependent,
; so this query records that a call named X happens here and leaves binding to
; the resolver, which matches by name and splits confidence across candidates.
;
; Deliberately not captured: a dynamic callee (`$fn()`, `$obj->$m()`), whose name
; is not in the source; a namespace-qualified callee (`Foo\helper()`), whose
; `qualified_name` text never matches a bare def name; and `new Thing()`, which
; is a construction, not a call edge to a named function.

; Bare call:  helper($x)
(function_call_expression
  function: (name) @ref.call)

; Instance call:  $obj->method($x)
(member_call_expression
  object: (_) @ref.recv
  name: (name) @ref.member)

; Nullsafe instance call:  $obj?->method($x) — same dispatch, different syntax
(nullsafe_member_call_expression
  object: (_) @ref.recv
  name: (name) @ref.member)

; Static / scoped call:  Foo::bar($x)  /  self::bar($x)  /  \Foo\Bar::baz($x)
; The scope is the receiver: a class name, a `relative_scope` (`self`/`parent`/
; `static`), or a qualified name.
(scoped_call_expression
  scope: (_) @ref.recv
  name: (name) @ref.member)
