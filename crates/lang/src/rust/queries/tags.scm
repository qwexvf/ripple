; Rust definitions. Free functions and methods share `function_item`; the adapter
; qualifies a method by its enclosing `impl` type, so `Client::start` and
; `Server::start` stay distinct symbols.
(function_item name: (identifier) @name) @def.function

(struct_item name: (type_identifier) @name) @def.class
(enum_item name: (type_identifier) @name) @def.enum
(union_item name: (type_identifier) @name) @def.class
(trait_item name: (type_identifier) @name) @def.interface
(type_item name: (type_identifier) @name) @def.type

; module-level bindings, not locals
(const_item name: (identifier) @name) @def.variable
(static_item name: (identifier) @name) @def.variable

; a Rust unit test sits in the file it tests, so only the attribute distinguishes
; it. Everything defined inside this span is test-side (see `is_test_path`).
((attribute_item
   (attribute (identifier) @_cfg arguments: (token_tree) @_args))
 .
 (mod_item body: (declaration_list)) @scope.test
 (#eq? @_cfg "cfg")
 (#eq? @_args "(test)"))
