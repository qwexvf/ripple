; Java imports. A `import com.google.gson.Gson;` fuses the module path and the
; class into one dotted `scoped_identifier`. The whole FQN is the specifier
; (`@import.source`), and its final `name:(identifier)` segment is the imported
; class (`@import.name`) — so this maps to a NAMED import, letting resolution
; find `com/google/gson/Gson.java` and bind `Gson` for later calls.
;
; A wildcard (`import a.b.*;`) has the outer scoped_identifier be the package
; (`a.b`) with `name` = `b`, and a static import (`import static a.b.C.d;`) makes
; `d` the name — neither will resolve to a local file, so both fall through to
; `external_dep_key` and mint an external node instead. The `name:` field of the
; outer scoped_identifier is matched only on the direct child of the import, so
; the nested scope segments are not captured.
(import_declaration
  (scoped_identifier
    name: (identifier) @import.name) @import.source)
