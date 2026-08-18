; TypeScript Tier-0 definition captures.
; The parse layer maps `@def.<kind>` → ir::NodeKind and reads `@name` for the symbol name.

(function_declaration
  name: (identifier) @name) @def.function

; `const load = () => …` / `const load = function () {}`.
;
; A function is a function however it was bound. Extracted as a variable, an
; arrow-assigned function is not a caller, so every call it makes falls back to
; the file — a generated API client of 39 functions collapsed into one node (#53).
;
; Both this and the general variable pattern match, and the two captures share a
; SymbolId. Which one the query engine emits first is not this file's to decide —
; `index_defs` upgrades Variable to Function whenever both are seen, so the order
; here does not matter. Every *other* pair of kinds does keep whichever arrived
; first, so a new pattern that overlaps an existing one still needs thought.
(lexical_declaration
  (variable_declarator
    name: (identifier) @name
    value: [(arrow_function) (function_expression)])) @def.function

; `#secret()` is a member like any other — it is a call container, and without it
; every call in its body was attributed to the file instead of to the class.
(method_definition
  name: [(property_identifier) (private_property_identifier)] @name) @def.method

(class_declaration
  name: (type_identifier) @name) @def.class

(interface_declaration
  name: (type_identifier) @name) @def.interface

(type_alias_declaration
  name: (type_identifier) @name) @def.type

(enum_declaration
  name: (identifier) @name) @def.enum

(public_field_definition
  name: [(property_identifier) (private_property_identifier)] @name) @def.field

; A module-scope `const`/`let` is a real symbol — a config object, a singleton, a
; re-exported value. A *local* one is not: `const cache = makeCache()` inside a test
; can't be referenced across files, and capturing it at any depth put every function
; temporary into the graph, ranking test-block locals in `review` (#68). So the
; plain-value variable is anchored to module scope, directly or through `export`. The
; arrow-function pattern above stays depth-free on purpose: a local
; `const handler = () => …` is still a callable whose edges are worth keeping.
(program
  (lexical_declaration
    (variable_declarator
      name: (identifier) @name)) @def.variable)

(program
  (export_statement
    (lexical_declaration
      (variable_declarator
        name: (identifier) @name)) @def.variable))

; Everything below is appended on purpose: none of these patterns competes with
; another for the same node, so their position cannot disturb the arrow-function /
; variable ordering above.

; --- abstract and bodiless class members -----------------------------------
;
; `abstract class` is its own node kind, so `export abstract class Repo {}`
; produced no class node at all — even though `qualified_name` already knows how
; to qualify members by it.
(abstract_class_declaration
  name: (type_identifier) @name) @def.class

; `abstract run(): void` and the `m(): void;` of a `declare class` are neither a
; `method_definition` nor a `public_field_definition`. An abstract base class
; therefore contributed zero members, and an override had nothing to point at.
(class_body
  (abstract_method_signature
    name: (property_identifier) @name) @def.method)

(class_body
  (method_signature
    name: (property_identifier) @name) @def.method)

; --- enum members ------------------------------------------------------------
;
; `enum Color { Red, Green = 2 }`: a bare member is a lone `property_identifier`
; child of the body, an initialized one is an `enum_assignment`. Neither is
; reachable from the `enum_declaration` pattern, so no enum member was indexed —
; `Color.Red` had no node for a reference to land on.
; `qualified_name` walks to the enclosing `enum_declaration`, giving `Color.Red`.
(enum_body
  (property_identifier) @name @def.field)

(enum_body
  (enum_assignment
    name: (property_identifier) @name) @def.field)

; --- interface members -------------------------------------------------------
;
; `interface Foo { bar(): void; baz: string }` — captured as `Foo.bar` / `Foo.baz`
; via the enclosing `interface_declaration`.
;
; Anchored on `interface_body` rather than matching `method_signature` /
; `property_signature` anywhere: the same two node kinds make up every anonymous
; `object_type`, including inline parameter and return types, and a field of an
; unnamed inline type is not an addressable symbol.
(interface_body
  (method_signature
    name: (property_identifier) @name) @def.method)

(interface_body
  (property_signature
    name: (property_identifier) @name) @def.field)

; --- object-literal properties bound to a function ---------------------------
;
; #53 again, one level in: in `export const api = { load() {}, save: () => … }`
; the shorthand `load` is a `method_definition` and was already captured, while
; the arrow-valued `save` was not — so a generated client's methods collapsed
; into the single `api` variable. Captured as a method, like its shorthand
; sibling, so that calls it makes are attributed to it.
;
; Anchored on an exported top-level binding: an identical `pair` shape is every
; inline callback in a hook or config argument, and those are not addressable —
; capturing them would merge unrelated handlers of the same name into one node.
(export_statement
  (lexical_declaration
    (variable_declarator
      value: (object
        (pair
          key: (property_identifier) @name
          value: [(arrow_function) (function_expression)]) @def.method))))

; --- ambient (`declare`) declarations ----------------------------------------
;
; A `declare function` has no body, so it parses as `function_signature`, not
; `function_declaration` — every entry point of a hand-written `.d.ts` was
; invisible.
;
; Anchored on the ambient contexts on purpose. A bare `function_signature` also
; matches an overload declaration, which shares its implementation's SymbolId;
; the bodiless signature comes first in document order and would take over the
; primary span, so calls inside the implementation would stop being attributed
; to the function.
(ambient_declaration
  (function_signature
    name: (identifier) @name) @def.function)

; the same signature inside `declare module "x" { … }` / `declare namespace N { … }`,
; bare or re-exported
(module
  body: (statement_block
    (function_signature
      name: (identifier) @name) @def.function))

(module
  body: (statement_block
    (export_statement
      declaration: (function_signature
        name: (identifier) @name) @def.function)))

(internal_module
  body: (statement_block
    (function_signature
      name: (identifier) @name) @def.function))

(internal_module
  body: (statement_block
    (export_statement
      declaration: (function_signature
        name: (identifier) @name) @def.function)))
