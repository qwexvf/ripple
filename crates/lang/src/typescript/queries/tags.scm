; TypeScript Tier-0 definition captures.
; The parse layer maps `@def.<kind>` → ir::NodeKind and reads `@name` for the symbol name.

(function_declaration
  name: (identifier) @name) @def.function

; `const load = () => …` / `const load = function () {}`.
;
; A function is a function however it was bound. Extracted as a variable, an
; arrow-assigned function is not a caller, so every call it makes falls back to
; the file — a generated API client of 39 functions collapsed into one node (#53).
;
; Listed before the general variable pattern on purpose: both match, the two
; captures share a SymbolId, and `extract_defs` keeps the first kind it saw.
(lexical_declaration
  (variable_declarator
    name: (identifier) @name
    value: [(arrow_function) (function_expression)])) @def.function

(method_definition
  name: (property_identifier) @name) @def.method

(class_declaration
  name: (type_identifier) @name) @def.class

(interface_declaration
  name: (type_identifier) @name) @def.interface

(type_alias_declaration
  name: (type_identifier) @name) @def.type

(enum_declaration
  name: (identifier) @name) @def.enum

(public_field_definition
  name: (property_identifier) @name) @def.field

(lexical_declaration
  (variable_declarator
    name: (identifier) @name)) @def.variable
