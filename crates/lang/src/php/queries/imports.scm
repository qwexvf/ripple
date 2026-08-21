; `use GuzzleHttp\Client;` / `use Foo;` / `use A\B as C;` — a Composer-namespace
; import. The path is the clause's first named child (the `alias:` name comes
; after, so the `.` anchor keeps it out). Captured as both the specifier and the
; namespace, so the binding pass mints an External module node and its
; import-level `Imports` edge. The dep-key is the top namespace segment
; (`GuzzleHttp`) — see `resolve_import::php_dep_key`.
(namespace_use_clause
  . (qualified_name) @import.source @import.namespace)

(namespace_use_clause
  . (name) @import.source @import.namespace)
