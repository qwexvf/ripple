; GraphQL definition captures.
;
; A `.graphql`/`.gql` file is a schema (types and their fields) or a document
; (operations and fragments). Both halves are referenced by name from code — a
; resolver answers for `User.email`, codegen turns `query CurrentPlayer` into
; `CurrentPlayerDocument` — so both are symbols worth having somewhere to land.
;
; The grammar names no fields, so every pattern below matches a *direct* child.
; That is what keeps `(object_type_definition (name) @name)` off the names nested
; inside `fields_definition`, which is a separate node.

(object_type_definition
  (name) @name) @def.type

(interface_type_definition
  (name) @name) @def.interface

(union_type_definition
  (name) @name) @def.type

(scalar_type_definition
  (name) @name) @def.type

(input_object_type_definition
  (name) @name) @def.type

(enum_type_definition
  (name) @name) @def.enum

; A field on a type or interface — including one added by `extend type` — is the
; unit a resolver serves, so it is the unit an impact answer has to name.
; Qualified by its declaring type in `qualified_name`: `id` is on nearly every
; type in a real schema.
(field_definition
  (name) @name) @def.field

; Fields of an `input` type. A field's *arguments* are `input_value_definition`s
; too, but those are parameters — nothing references them across files — so only
; the ones directly under `input_fields_definition` are captured.
(input_fields_definition
  (input_value_definition
    (name) @name) @def.field)

; An enum's members. The IR has no variant kind and an enum value is a member of
; its enum, so it lands as a field (the same choice TypeScript makes for a class
; member).
(enum_value_definition
  (enum_value
    (name) @name)) @def.field

; A named operation and a fragment are the document's addressable units: an
; operation is what `<Name>Document` refers to and what cross-service linking
; matches on, a fragment is what `...Name` spreads. Both are invoked by name, so
; `def.function` is the kind that fits; an anonymous `{ … }` operation has no
; name child and so captures nothing.
(operation_definition
  (name) @name) @def.function

(fragment_definition
  (fragment_name
    (name) @name)) @def.function

; `directive @auth(requires: Role!) on FIELD_DEFINITION` — declared once, applied
; by name wherever it is used.
(directive_definition
  (name) @name) @def.function
