; C# Tier-0 definition captures. Names live on the `name:` field of each
; declaration; type members (methods, properties, fields) are qualified by their
; enclosing type in `qualified_name`, so `Widget.Name` and `Order.Name` stay
; distinct symbols.

(class_declaration
  name: (identifier) @name) @def.class

(interface_declaration
  name: (identifier) @name) @def.interface

; A struct is a nominal type like a class — same node role in the graph.
(struct_declaration
  name: (identifier) @name) @def.class

(record_declaration
  name: (identifier) @name) @def.class

(enum_declaration
  name: (identifier) @name) @def.enum

(method_declaration
  name: (identifier) @name) @def.method

; A constructor's name is the type name; qualified it becomes `Type.Type`.
(constructor_declaration
  name: (identifier) @name) @def.method

(property_declaration
  name: (identifier) @name) @def.field

; Fields hold their name a level down, on each declarator, so `int a, b;`
; yields one def per name. The def anchors on `field_declaration` so its
; modifiers stay a direct child for `is_exported`.
(field_declaration
  (variable_declaration
    (variable_declarator
      name: (identifier) @name))) @def.field
