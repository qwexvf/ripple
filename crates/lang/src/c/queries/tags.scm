; C Tier-0 definition captures. C has no classes/methods/visibility keywords, so
; the vocabulary is mapped structurally: a struct/union is a "class", a typedef a
; "type", file-scope declarations are "variables".

; Functions. The name is nested inside the declarator — a `function_declarator`
; whose own `declarator:` is the identifier being defined. The captured def is the
; whole `function_definition` so `is_exported` can see a `static` storage class.
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @name)) @def.function

; Named struct / union → class. An anonymous `struct { ... }` (e.g. inside a
; typedef) has no `name:` and is not captured here; the typedef names it instead.
(struct_specifier
  name: (type_identifier) @name) @def.class

(union_specifier
  name: (type_identifier) @name) @def.class

; Named enum.
(enum_specifier
  name: (type_identifier) @name) @def.enum

; typedef — the new type name is the declarator. Restricted to typedefs whose
; underlying type is NOT a *named* struct/union/enum: `typedef struct Tag {...}
; Tag;` already captures `Tag` as a class above, and a second `@def.type Tag`
; would hash to the same `SymbolId` and non-deterministically clobber it. An
; anonymous record (`typedef struct {...} Foo;`) has no other capture, so it is
; kept — matched by the `!name` specifiers below.
(type_definition
  type: [
    (primitive_type)
    (type_identifier)
    (sized_type_specifier)
    (struct_specifier !name)
    (union_specifier !name)
    (enum_specifier !name)
  ]
  declarator: (type_identifier) @name) @def.type

; Struct/union members. Anchored on a *named* specifier: the fields of an
; anonymous struct have no single-segment owner to qualify them by.
; `qualified_name` prefixes the owning struct/union so `Point.x` and `Rect.x`
; stay distinct and neither collides with a file-scope `x`.
(struct_specifier
  name: (type_identifier)
  body: (field_declaration_list
    (field_declaration
      declarator: (field_identifier) @name) @def.field))

(union_specifier
  name: (type_identifier)
  body: (field_declaration_list
    (field_declaration
      declarator: (field_identifier) @name) @def.field))

; File-scope variables only — a local inside a function body is not a symbol
; another file can reach. Anchored on `translation_unit`. Both the plain
; `int g;` and the initialized `int g = 1;` (an `init_declarator`) forms.
(translation_unit
  (declaration
    declarator: (identifier) @name) @def.variable)

(translation_unit
  (declaration
    declarator: (init_declarator
      declarator: (identifier) @name)) @def.variable)
