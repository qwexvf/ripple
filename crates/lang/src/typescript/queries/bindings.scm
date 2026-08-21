; const b = new Bar();
(variable_declarator
  name: (identifier) @bind.name
  value: (new_expression constructor: (identifier) @bind.ctor))

; const b: Bar = ...;
(variable_declarator
  name: (identifier) @bind.name
  type: (type_annotation (type_identifier) @bind.type))

; function f(b: Bar) { ... }
(required_parameter
  pattern: (identifier) @bind.name
  type: (type_annotation (type_identifier) @bind.type))

; Any local `const/let/var x = ...` or plain parameter — records the name with no
; type so a locally-declared identifier is visible to call resolution even when its
; type is unknown. A local `const React = makeFake()` must shadow an
; `import * as React` so `React.foo()` binds locally, not to the external module.
; The typed/ctor patterns above capture the same declarators too; call resolution
; prefers a non-empty type, so this never downgrades a known one.
(variable_declarator
  name: (identifier) @bind.name)

(required_parameter
  pattern: (identifier) @bind.name)
