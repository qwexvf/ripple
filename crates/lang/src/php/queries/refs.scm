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

; Static call through a class name:  Utils::chooseHandler($x)  /  Psr7\Utils::f($x)
;
; The class is the call's *qualifier*, not a receiver: it names the owner the
; target must be declared on, which `qualified_name` now records
; (`Utils.chooseHandler`). That resolves to the one right method instead of every
; same-named method in the repo — and, when the class is third-party, to nothing
; at all rather than to a local look-alike. Guzzle's `Psr7\Utils::modifyRequest`
; is the case that matters: on the by-name path it bound, confidently and wrongly,
; to the unrelated `RedirectMiddleware::modifyRequest` in this repo.
(scoped_call_expression
  scope: (name) @ref.qualifier
  name: (name) @ref.call)

(scoped_call_expression
  scope: (qualified_name) @ref.qualifier
  name: (name) @ref.call)

; Static call whose scope names no class:  self::bar($x), parent::bar($x),
; static::bar($x), $class::bar($x). Resolving `self` needs the enclosing class,
; which the receiver vocabulary can't express for PHP, so these stay on the
; by-name member path and split confidence across candidates.
;
; Scopes are enumerated rather than matched with `(_)` so a class-name scope is
; captured once, by the qualifier patterns above, and not a second time here.
(scoped_call_expression
  scope: (relative_scope) @ref.recv
  name: (name) @ref.member)

(scoped_call_expression
  scope: (variable_name) @ref.recv
  name: (name) @ref.member)
