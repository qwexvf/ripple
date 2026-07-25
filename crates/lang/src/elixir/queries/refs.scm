; Local (unqualified) function calls: `helper(x)`. Remote calls `M.f(x)` are
; handled by the cross-service extractor, which resolves the module alias.
;
; Elixir definitions are themselves calls, so this necessarily also matches
; definition macros and language forms — the ones that could never name a
; function are excluded here, and a DSL macro that survives only produces an
; edge if the module actually defines a function by that name. The remaining
; artifact (a def's own name in its header parses as a call) is dropped in
; `resolve`, which knows the enclosing definition.
; A module attribute's body is not code that runs here: `@spec get(id) :: t()`
; and `@type opts :: keyword(atom)` are made entirely of things that parse as
; calls but name types. Ignoring the whole attribute drops them all, at the cost
; of ignoring a call in an attribute value (`@default compute()`), which is
; module-load-time evaluation rather than a call site.
(unary_operator operator: "@" operand: (call)) @ref.ignore

(call
  target: (identifier) @ref.call
  (#not-any-of? @ref.call
    "def" "defp" "defmacro" "defmacrop" "defmodule" "defstruct" "defimpl"
    "defprotocol" "defdelegate" "defexception" "defguard" "defguardp" "defoverridable"
    "alias" "import" "require" "use" "quote" "unquote" "unquote_splicing"
    "if" "unless" "case" "cond" "with" "for" "try" "catch" "rescue" "after" "receive"
    "fn" "raise" "reraise" "throw" "exit" "super"))
