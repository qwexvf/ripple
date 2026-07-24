; Elixir definitions are macro calls; predicates on the call target select them.

; defmodule Foo.Bar do ... end  → a module (mapped to Class)
(call
  target: (identifier) @_kw
  (arguments (alias) @name)
  (#eq? @_kw "defmodule")) @def.class

; def / defp / defmacro name(args) do ... end  → a function
(call
  target: (identifier) @_kw
  (arguments
    [
      (call target: (identifier) @name)
      (identifier) @name
      (binary_operator left: (call target: (identifier) @name))
    ])
  (#any-of? @_kw "def" "defp" "defmacro" "defmacrop")) @def.function
