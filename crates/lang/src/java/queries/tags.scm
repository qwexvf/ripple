; Java Tier-0 definition captures. Names sit on the `name:` field as an
; `(identifier)` (records/enums/interfaces/classes/methods/constructors) or, for
; fields, on the `variable_declarator` under the declaration — so `int a, b;`
; yields one capture per declarator.

(class_declaration
  name: (identifier) @name) @def.class

; A record is a class with a canonical constructor — capture it as a class.
(record_declaration
  name: (identifier) @name) @def.class

(interface_declaration
  name: (identifier) @name) @def.interface

(enum_declaration
  name: (identifier) @name) @def.enum

(method_declaration
  name: (identifier) @name) @def.method

; A constructor is a method named after its class.
(constructor_declaration
  name: (identifier) @name) @def.method

; Fields. `obj.field` is a reference a server can report, so each field needs a
; symbol; `qualified_name` prefixes the owning type so `Foo.count` and
; `Bar.count` stay distinct and neither collides with a same-named method. The
; declaration carries the modifiers `is_exported` reads, so @def.field lands on
; it, not on the declarator.
(field_declaration
  declarator: (variable_declarator
    name: (identifier) @name)) @def.field
