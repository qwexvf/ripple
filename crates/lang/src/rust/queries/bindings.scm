; Rust receiver types, read off what the source writes down. No inference: a
; receiver with no written type stays untyped, and for Rust an untyped member call
; resolves to nothing rather than to every same-named method in the project (see
; `member_calls_fall_back_by_name` in `rust/mod.rs`).

; `let x: Foo = …;` and `let x: Foo<T> = …;`
(let_declaration
  pattern: (identifier) @bind.name
  type: [(type_identifier) @bind.type
         (generic_type type: (type_identifier) @bind.type)])

; `let x = Foo::new(…);` — a path call on a capitalized segment is read as that
; type's constructor. `Foo::parse` returning `Result<Foo>` is the usual exception,
; and then the receiver is typed `Foo` where it is really a `Result`: a member call
; on it finds no method of that name on `Foo` and resolves to nothing.
(let_declaration
  pattern: (identifier) @bind.name
  value: (call_expression
    function: (scoped_identifier
      path: (identifier) @bind.ctor))
  (#match? @bind.ctor "^[A-Z]"))

; `let x = Foo { … };`
(let_declaration
  pattern: (identifier) @bind.name
  value: (struct_expression
    name: (type_identifier) @bind.ctor))

; `fn f(x: Foo)`, `x: &Foo`, `x: &mut Foo`, `x: Foo<T>`
(parameter
  pattern: (identifier) @bind.name
  type: [(type_identifier) @bind.type
         (generic_type type: (type_identifier) @bind.type)
         (reference_type type: [(type_identifier) @bind.type
                                (generic_type type: (type_identifier) @bind.type)])])
