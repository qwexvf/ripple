; Rust definitions. Free functions and methods share `function_item`; the adapter
; qualifies a method by its enclosing `impl` type, so `Client::start` and
; `Server::start` stay distinct symbols.
(function_item name: (identifier) @name) @def.function

(struct_item name: (type_identifier) @name) @def.class
(enum_item name: (type_identifier) @name) @def.enum
(union_item name: (type_identifier) @name) @def.class
(trait_item name: (type_identifier) @name) @def.interface
(type_item name: (type_identifier) @name) @def.type

; Members of the types above. A `match` on `Kind::Route` or a read of `node.span`
; depends on the variant/field, not on the whole type, so they are symbols of their
; own; without them a shape change looked like it touched nothing.
;
; No pattern above matches these nodes, so they carry no first-kind-wins ordering
; constraint of the kind TypeScript's tags.scm documents.
(enum_variant name: (identifier) @name) @def.field
; named fields only — `struct Id(u64)` has no name to address the field by. Covers
; struct, union and struct-variant bodies, which share `field_declaration`.
(field_declaration name: (field_identifier) @name) @def.field

; module-level bindings, not locals
(const_item name: (identifier) @name) @def.variable
(static_item name: (identifier) @name) @def.variable
