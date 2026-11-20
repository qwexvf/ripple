; Rust definitions. Free functions and methods share `function_item`; the adapter
; qualifies a method by its enclosing `impl` type, so `Client::start` and
; `Server::start` stay distinct symbols.
;
; Ordering is load-bearing: a method also matches the general `function_item`
; pattern, both captures produce the same SymbolId, and the first kind seen is kept.
; So the method patterns come first. `NodeKind::Method` is what member-call
; resolution keys off; as plain functions, `self.step()` and `site.step()` linked
; to nothing at all (#138).
(impl_item body: (declaration_list (function_item name: (identifier) @name) @def.method))
(trait_item body: (declaration_list (function_item name: (identifier) @name) @def.method))
; a required trait method has no body, so it is a signature, not a `function_item`
(trait_item body: (declaration_list (function_signature_item name: (identifier) @name) @def.method))

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
