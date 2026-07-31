; `from x.y import z`  /  `from .mod import z as w`  /  `from . import z`
;
; The module part is the specifier; the imported names are what the file binds.
; A relative import spells its depth in leading dots, which `resolve_import` reads.
(import_from_statement
  module_name: [(dotted_name) (relative_import)] @import.source
  name: (dotted_name (identifier) @import.name))

(import_from_statement
  module_name: [(dotted_name) (relative_import)] @import.source
  name: (aliased_import
          name: (dotted_name (identifier) @import.name)
          alias: (identifier) @import.alias))

; `import x.y as z` — the whole module is bound to one local name.
;
; Plain `import x.y` is deliberately not read: the binding is the top package and
; every use spells the rest (`x.y.f()`), which member resolution cannot follow.
; Under-link rather than bind a name nothing will match.
(import_statement
  name: (aliased_import
          name: (dotted_name) @import.source
          alias: (identifier) @import.namespace))
