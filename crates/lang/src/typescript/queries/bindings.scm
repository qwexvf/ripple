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
