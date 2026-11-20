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

; Side-effect import:  import "polyfill"  (no clause, binds nothing)
;
; The leading anchor requires the string to be the first named child, so this only
; matches a clause-less import — `import a from "x"` has an import_clause first.
; Still worth an External module node so the import-level floor sees the dep.
(import_statement
  . (string (string_fragment) @import.bare))

; --- CommonJS require ---------------------------------------------------------
;
; `.js`/`.cjs` still lean on CommonJS. `require("x")` is a call, and the destructure
; or binding beside it names the locals — the same shapes ESM `import` produces, so
; these reuse the `@import.*` captures and bind identically (#93). The `#eq?` guard
; keeps this from matching any other one-arg call.

; Namespace bind:  const ns = require("x")   → ns.foo() is a member call on the module
(variable_declarator
  name: (identifier) @import.namespace
  value: (call_expression
    function: (identifier) @_req
    arguments: (arguments (string (string_fragment) @import.source)))
  (#eq? @_req "require"))

; Named destructure:  const { a, b } = require("x")
(variable_declarator
  name: (object_pattern
    (shorthand_property_identifier_pattern) @import.name)
  value: (call_expression
    function: (identifier) @_req
    arguments: (arguments (string (string_fragment) @import.source)))
  (#eq? @_req "require"))

; Aliased destructure:  const { a: b } = require("x")   → local `b`, source knows `a`
(variable_declarator
  name: (object_pattern
    (pair_pattern
      key: (property_identifier) @import.name
      value: (identifier) @import.alias))
  value: (call_expression
    function: (identifier) @_req
    arguments: (arguments (string (string_fragment) @import.source)))
  (#eq? @_req "require"))

; Dynamic import read through `.then`: `import("./x").then((m) => m.X)` and the
; code-splitting form `lazy(() => import("./x").then((m) => ({ default: m.X })))`.
; The member the callback reads off the module is the imported name, so the
; component reached only through a lazy route is not dead code (#145). A query
; cannot search an arbitrary body, so only these two shapes are recognised.
(call_expression
  function: (member_expression
    object: (call_expression
      function: (import)
      arguments: (arguments (string (string_fragment) @import.source)))
    property: (property_identifier) @_then)
  arguments: (arguments
    (arrow_function
      [parameter: (identifier) @_m
       parameters: (formal_parameters
         (required_parameter pattern: (identifier) @_m))]
      body: [(member_expression
               object: (identifier) @_obj
               property: (property_identifier) @import.name)
             (parenthesized_expression
               (object (pair
                 value: (member_expression
                   object: (identifier) @_obj
                   property: (property_identifier) @import.name))))]))
  (#eq? @_then "then")
  (#eq? @_m @_obj))
