; Go imports. The import path is both the specifier (its dep-key is the path
; itself) and — split on `/` — the local package name a later `pkg.Foo()` call
; resolves against. An explicit alias wins over the last path segment.
;
;   import "github.com/gin-gonic/gin"   → local `gin`,  dep `github.com/gin-gonic/gin`
;   import m "math"                     → local `m`,     stdlib (no dep node)
;   import _ "github.com/x/y"           → side-effect;   dep node only
;
; The path content is captured as both @import.source (the specifier) and
; @import.namespace (whose last `/` segment becomes the local name), so a bare
; import binds a namespace the way `import * as ns` does in TypeScript.
(import_spec
  name: (package_identifier) @import.alias
  path: (interpreted_string_literal
          (interpreted_string_literal_content) @import.source @import.namespace))

(import_spec
  name: (blank_identifier)
  path: (interpreted_string_literal
          (interpreted_string_literal_content) @import.bare))

(import_spec
  !name
  path: (interpreted_string_literal
          (interpreted_string_literal_content) @import.source @import.namespace))
