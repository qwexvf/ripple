; Named imports:  import { a, b } from "x"
(import_statement
  (import_clause (named_imports (import_specifier name: (identifier) @import.name)))
  source: (string (string_fragment) @import.source))

; Default import:  import a from "x"
(import_statement
  (import_clause (identifier) @import.default)
  source: (string (string_fragment) @import.source))
