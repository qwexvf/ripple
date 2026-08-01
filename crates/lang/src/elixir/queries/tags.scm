; Elixir definitions are macro calls; predicates on the call target select them.

; defmodule Foo.Bar do ... end  → a module (mapped to Class)
(call
  target: (identifier) @_kw
  (arguments (alias) @name)
  (#eq? @_kw "defmodule")) @def.class

; Every macro that declares something callable, in one pattern: they all write the
; same three header shapes (`name(args)`, bare `name`, `header when guard`), and
; `is_exported` reads the target back off this same node to tell the private
; spellings apart.
;
; `defdelegate reverse(list), to: Enum` declares `reverse/1` *here* — `to:` says
; where the body lives, not where the symbol lives, and `as:` renames the target
; rather than this definition. `defguard`/`defguardp` are macros whose header is
; always the `when` shape.
(call
  target: (identifier) @_kw
  (arguments
    [
      (call target: (identifier) @name)
      (identifier) @name
      (binary_operator left: (call target: (identifier) @name))
    ])
  (#any-of? @_kw
    "def" "defp" "defmacro" "defmacrop" "defdelegate" "defguard" "defguardp")) @def.function

; defstruct [:name, :email] / defstruct name: nil / defstruct [:name, age: 0]
;
; The capture sits on the key, not on the `defstruct` call, so a field's span is
; the field — a one-line struct still attributes a changed key to the key that
; changed rather than to the whole declaration.
;
; Both spellings are punctuated single tokens: the grammar has no bare identifier
; inside an `(atom)` or a `(keyword)`, so the captured name is `:name` or `name: `
; (the keyword token keeps its colon and the space after it). `qualified_name` is
; what turns either into one `User.name` symbol — see mod.rs.
(call
  target: (identifier) @_kw
  (arguments
    [
      (list (atom) @name @def.field)
      (list (keywords (pair key: (keyword) @name @def.field)))
      (keywords (pair key: (keyword) @name @def.field))
    ])
  (#eq? @_kw "defstruct"))

; Module attributes are deliberately not definitions here. `@timeout 5_000` is a
; constant, but `@moduledoc`, `@doc`, `@spec`, `@impl`, `@behaviour`, `@derive`
; and every framework's own (`@primary_key`, `@moduletag`) parse identically — one
; call, one argument — so no shape separates a constant from a directive, and a
; denylist of reserved names grows with every library that ships one. Capturing
; the shape would put a `doc` and a `spec` symbol in every module in the repo, and
; nothing would ever reach them: a use site spells it `@timeout`, which refs.scm
; ignores wholesale (see the `@ref.ignore` rule there).
