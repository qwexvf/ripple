; PHP Tier-0 definition captures — enough to give an imported symbol a home and
; to answer "what does this file define". Members are not qualified by their
; class yet; PHP support is import-level (see the module doc).
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
