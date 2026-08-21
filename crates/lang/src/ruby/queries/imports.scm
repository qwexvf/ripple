; `require "json"` / `require "active_record/base"` / `gem "rails"` — a load of
; an external library. Ruby's `require` runs a file for its side effects (it
; defines globals/constants), so there is no imported symbol to bind: this is
; exactly the side-effect-import shape, captured as @import.bare. The binding
; pass mints an External module node keyed by the first path segment
; (`resolve_import::ruby_dep_key`) and its import-level `Imports` edge.
;
; `require_relative` is intentionally excluded — it names a local file, not a
; dependency.
(call
  method: (identifier) @_m
  arguments: (argument_list (string (string_content) @import.bare))
  (#any-of? @_m "require" "gem"))
