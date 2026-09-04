; C++ receiver types, read straight off what the source writes down. No
; inference: a declaration whose type is not a plain `type_identifier`
; (`std::vector<T>`, a template, a `decltype`) is deliberately not captured, and
; the member call falls back to by-name candidates at its existing confidence.

; --- locals -----------------------------------------------------------------
; `Foo x;` / `Foo* p;` / `Foo& r = …;` / `Foo x = make();`

(declaration
  type: (type_identifier) @bind.type
  declarator: (identifier) @bind.name)

(declaration
  type: (type_identifier) @bind.type
  declarator: (pointer_declarator
    declarator: (identifier) @bind.name))

(declaration
  type: (type_identifier) @bind.type
  declarator: (reference_declarator
    (identifier) @bind.name))

(declaration
  type: (type_identifier) @bind.type
  declarator: (init_declarator
    declarator: (identifier) @bind.name))

(declaration
  type: (type_identifier) @bind.type
  declarator: (init_declarator
    declarator: (pointer_declarator
      declarator: (identifier) @bind.name)))

(declaration
  type: (type_identifier) @bind.type
  declarator: (init_declarator
    declarator: (reference_declarator
      (identifier) @bind.name)))

; `auto x = Foo();` — `auto` writes no type, but the initializer names one. Only
; the direct `Foo(...)` form: `auto x = make()` names a *function*, and there the
; class lookup simply misses, so resolution stays where it is today.
(declaration
  type: (placeholder_type_specifier)
  declarator: (init_declarator
    declarator: (identifier) @bind.name
    value: (call_expression
      function: (identifier) @bind.ctor)))

; --- parameters -------------------------------------------------------------
; `void m(Foo x)` / `m(Foo& x)` / `m(const Foo* x)` — the qualifiers sit beside
; the `type:` field, so they need no patterns of their own.

(parameter_declaration
  type: (type_identifier) @bind.type
  declarator: (identifier) @bind.name)

(parameter_declaration
  type: (type_identifier) @bind.type
  declarator: (reference_declarator
    (identifier) @bind.name))

(parameter_declaration
  type: (type_identifier) @bind.type
  declarator: (pointer_declarator
    declarator: (identifier) @bind.name))

; --- data members -----------------------------------------------------------
; `Foo b;` / `Foo* p;` inside a class body. A member function's declarator is a
; `function_declarator`, so these never fire on one.

(field_declaration
  type: (type_identifier) @bind.type
  declarator: (field_identifier) @bind.name)

(field_declaration
  type: (type_identifier) @bind.type
  declarator: (pointer_declarator
    declarator: (field_identifier) @bind.name))
