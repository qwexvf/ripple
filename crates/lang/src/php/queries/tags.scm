; PHP Tier-0 definition captures — enough to give an imported symbol a home and
; to answer "what does this file define". A `method_declaration` is qualified by
; its enclosing type in `Adapter::qualified_name`, so `Utils.chooseHandler` is the
; name a `SymbolId` is hashed from and the owner a `Utils::chooseHandler()` call
; binds against. Properties and class constants are not captured yet.
(function_definition
  name: (name) @name) @def.function

(method_declaration
  name: (name) @name) @def.method

(class_declaration
  name: (name) @name) @def.class

(interface_declaration
  name: (name) @name) @def.interface

(trait_declaration
  name: (name) @name) @def.class
