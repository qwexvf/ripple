; Python definitions.
;
; Ordering is load-bearing. A method matches a method pattern *and* the general
; `function_definition` pattern below; both captures produce the same SymbolId,
; because the adapter qualifies either one by its enclosing class, and the first
; kind seen is the one kept. So the method patterns come first — the same reason
; TypeScript lists its arrow-function pattern before its variable one.

; `class C:` / `def m(self)`. Without this a method is a plain Function, and
; `NodeKind::Method` is what member-call resolution keys off: no method kind means
; an empty `methods_by_class`/`methods_by_name`, so `obj.send()` resolves to
; nothing at all rather than to `Client.send`.
(class_definition
  body: (block
    (function_definition
      name: (identifier) @name) @def.method))

; The same method, decorated. `@property` / `@staticmethod` / `@abstractmethod`
; wrap the definition in a `decorated_definition`, so it is no longer a direct
; child of the class body and the pattern above cannot see it.
(class_definition
  body: (block
    (decorated_definition
      definition: (function_definition
        name: (identifier) @name) @def.method)))

; Free functions. Not anchored to a parent, so a decorated module-level `def` is
; still captured — the decorator only changes the shape above the definition.
(function_definition
  name: (identifier) @name) @def.function

(class_definition
  name: (identifier) @name) @def.class

; Class-level attributes: enum members (`class Color(Enum): RED = 1`), dataclass
; fields, class constants. Addressable as `Color.RED`, and the adapter qualifies a
; field by its class the way it qualifies a method. Annotated forms (`RED: int = 1`,
; a bare `name: str`) are the same `assignment` node in this grammar.
(class_definition
  body: (block
    (expression_statement
      (assignment
        left: (identifier) @name) @def.field)))

; Module-level bindings only. A name bound inside a function is a local, and
; indexing locals would drown the graph in variables nobody can depend on.
;
; The capture sits on the assignment, not on the enclosing `module`: it is the
; definition's span, and anchoring it on the module gave every module-level
; constant a span covering the whole file.
(module
  (expression_statement
    (assignment
      left: (identifier) @name) @def.variable))
