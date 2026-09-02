; Kotlin imports. `import com.example.Foo` fuses module + symbol in one dotted
; path: the whole `qualified_identifier` is the specifier (@import.source) and its
; last segment is the imported name (@import.name), so it maps to a named import.
; `import com.example.Bar as Baz` carries a trailing alias identifier
; (@import.alias) that becomes the local binding. A star import `import a.b.*`
; has no trailing `*` node here — its last segment is `b`, which won't resolve to
; a file and falls through to an external dep node.
(import
  (qualified_identifier
    (identifier) @import.name .) @import.source
  (identifier)? @import.alias)
