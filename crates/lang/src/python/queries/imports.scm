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
(import_statement
  name: (aliased_import
          name: (dotted_name) @import.source
          alias: (identifier) @import.namespace))

; Plain `import x` — bind the module name so `x.f()` resolves against it. A single
; segment (`import os`) binds `os`; a dotted `import x.y` binds the whole `x.y`,
; and a deep-chain member call `x.y.f()` now resolves too: the receiver `x.y` is
; read as one dotted name (see `parse::receiver_of`) and looked up against this
; binding, so `import os.path` + `os.path.join()` links to the `os` dep. Residual:
; `import os.path` binds `os.path`, not the top-level `os`, so a bare `os.f()` in
; the same file (Python auto-exposes the parent package) is not linked.
(import_statement
  name: (dotted_name) @import.namespace @import.source)
