; Python definitions.
;
; A method and a free function share `function_definition`; the adapter qualifies a
; method by its enclosing class, so `Client.send` and `Server.send` stay distinct.
(function_definition
  name: (identifier) @name) @def.function

(class_definition
  name: (identifier) @name) @def.class

; Module-level bindings only. A name bound inside a function is a local, and
; indexing locals would drown the graph in variables nobody can depend on.
(module
  (expression_statement
    (assignment
      left: (identifier) @name))) @def.variable
