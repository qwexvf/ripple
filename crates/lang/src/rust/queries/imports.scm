; Rust `use` declarations. The specifier is the path before the imported name; a
; name imported through a nested list keeps only the outer path, which is enough
; because `resolve_import` maps no deeper than the crate (see `rust/mod.rs`).
;
;   use crate::store::Client;           → `Client`        from `crate::store`
;   use aegis_lockfile::{parse_file, X}; → `parse_file`, `X` from `aegis_lockfile`
;   use a::b as c;                       → `b` bound as `c` from `a`

(use_declaration
  argument: (scoped_identifier
    path: (_) @import.source
    name: (identifier) @import.name))

(use_declaration
  argument: (use_as_clause
    path: (scoped_identifier
      path: (_) @import.source
      name: (identifier) @import.name)
    alias: (identifier) @import.alias))

(use_declaration
  argument: (scoped_use_list
    path: (_) @import.source
    list: (use_list (identifier) @import.name)))

(use_declaration
  argument: (scoped_use_list
    path: (_) @import.source
    list: (use_list (scoped_identifier name: (identifier) @import.name))))

(use_declaration
  argument: (scoped_use_list
    path: (_) @import.source
    list: (use_list (use_as_clause
      path: (identifier) @import.name
      alias: (identifier) @import.alias))))
