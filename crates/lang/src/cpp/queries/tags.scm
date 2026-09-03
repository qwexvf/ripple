; C++ Tier-0 definition captures. tree-sitter-cpp is a superset of the C grammar:
; a member function is a `function_declarator` whose inner declarator is a
; `field_identifier` (defined inline) or a `qualified_identifier` (`Foo::bar`,
; defined out-of-line); a free function's is a plain `identifier`. That inner
; shape is the only thing separating @def.function from @def.method.

; --- types ------------------------------------------------------------------

(class_specifier
  name: (type_identifier) @name) @def.class

(struct_specifier
  name: (type_identifier) @name) @def.class

; A union is a type with fields, same as a struct. Captured here because this
; adapter also owns `.h` (#119), so a plain C header's unions must not be lost.
(union_specifier
  name: (type_identifier) @name) @def.class

(enum_specifier
  name: (type_identifier) @name) @def.enum

; `typedef int myint;`
(type_definition
  declarator: (type_identifier) @name) @def.type

; `using X = int;`
(alias_declaration
  name: (type_identifier) @name) @def.type

; --- free functions ---------------------------------------------------------
; Inner declarator is a bare identifier. A pointer/reference return type wraps
; the function_declarator, so those get their own patterns.

(function_definition
  declarator: (function_declarator
    declarator: (identifier) @name)) @def.function

(function_definition
  declarator: (pointer_declarator
    declarator: (function_declarator
      declarator: (identifier) @name))) @def.function

(function_definition
  declarator: (reference_declarator
    (function_declarator
      declarator: (identifier) @name))) @def.function

; --- member functions -------------------------------------------------------
; Inline definition inside a class/struct body: inner declarator is a
; field_identifier. Out-of-line definition (`void Foo::bar(){}`): inner
; declarator is a qualified_identifier whose `name` is the method identifier —
; qualified_name digs the owner off the `::` scope.

(function_definition
  declarator: (function_declarator
    declarator: (field_identifier) @name)) @def.method

(function_definition
  declarator: (function_declarator
    declarator: (qualified_identifier
      name: (identifier) @name))) @def.method

; A member function *declaration* (prototype) inside a class body is deliberately
; NOT captured. A prototype is a declaration, not a definition: `void bar();` in
; `app.hpp` and `void Foo::bar() {}` in `app.cc` are one function, but `SymbolId`
; is keyed by (module_path, qualified_name), so capturing both made them two nodes
; that then *competed* — a cross-file `f.bar()` split 1/N across the header and the
; source (0.3 each) instead of pinning the definition. The grammar makes them easy
; to keep apart: a definition is a `function_definition` (matched above), a
; prototype a bodyless `field_declaration`. A pure-virtual or never-defined member
; therefore contributes no symbol, which is correct — there is no code to reach.

; --- data members -----------------------------------------------------------
; A data member is a field_declaration whose declarator is the field_identifier
; directly (a function member's declarator is a function_declarator, matched
; above, so the two never both fire). qualified_name owns it by its class.

(field_declaration
  declarator: (field_identifier) @name) @def.field

(field_declaration
  declarator: (pointer_declarator
    declarator: (field_identifier) @name)) @def.field

; --- file-scope variables ---------------------------------------------------
; Anchored on translation_unit: a local inside a function body is not a symbol
; another file can reach. A function prototype is also a `declaration` but its
; declarator is a function_declarator, so it is not caught here.

(translation_unit
  (declaration
    declarator: (init_declarator
      declarator: (identifier) @name)) @def.variable)

(translation_unit
  (declaration
    declarator: (identifier) @name) @def.variable)

(translation_unit
  (declaration
    declarator: (init_declarator
      declarator: (pointer_declarator
        declarator: (identifier) @name))) @def.variable)

(translation_unit
  (declaration
    declarator: (pointer_declarator
      declarator: (identifier) @name)) @def.variable)
