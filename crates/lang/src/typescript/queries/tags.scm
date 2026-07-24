; TypeScript Tier-0 definition captures.
; The parse layer maps `@def.<kind>` → ir::NodeKind and reads `@name` for the symbol name.

(function_declaration
  name: (identifier) @name) @def.function

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
