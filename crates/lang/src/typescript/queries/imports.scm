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
