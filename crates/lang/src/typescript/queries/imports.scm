; Named imports:  import { a, b } from "x"
(import_statement
  (import_clause (named_imports (import_specifier name: (identifier) @import.name)))
  source: (string (string_fragment) @import.source))

; Default import:  import a from "x"
(import_statement
  (import_clause (identifier) @import.default)
  source: (string (string_fragment) @import.source))

; Re-export everything:  export * from "x"
;
; A barrel file defines nothing of its own, so an import landing on it resolves to
; nothing unless the chain is followed (issue #27).
(export_statement
  "*" @reexport.star
  source: (string (string_fragment) @reexport.source))

; Re-export by name:  export { a, b as c } from "x"
(export_statement
  (export_clause
    (export_specifier
      name: (identifier) @reexport.name
      alias: (identifier)? @reexport.alias))
  source: (string (string_fragment) @reexport.source))

; Aliased named import:  import { a as b } from "x"
;
; The local name is `b`, the source knows it as `a`. Binding the wrong one made every
; call through an alias unresolvable (issue #1).
(import_statement
  (import_clause
    (named_imports
      (import_specifier
        name: (identifier) @import.name
        alias: (identifier) @import.alias)))
  source: (string (string_fragment) @import.source))

; Namespace import:  import * as ns from "x"
;
; Binds a whole module to one local name, so `ns.foo()` is a member call whose receiver
; is pinned by the import rather than inferred.
(import_statement
  (import_clause
    (namespace_import (identifier) @import.namespace))
  source: (string (string_fragment) @import.source))
