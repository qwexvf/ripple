; C# `using` directives. A `using` imports a *namespace*, not a file — the
; namespace is both the specifier and the thing a later qualified reference
; resolves against, so it is captured as `@import.source` and `@import.namespace`
; on the same node (the ImportRec then binds `*`, a whole-namespace import).
;
;   using System;                       → System
;   using System.Collections.Generic;   → System.Collections.Generic
;   using static System.Math;           → System.Math
;
; `!name` keeps these off alias directives (`using X = Foo;`), where the `name`
; field holds the alias identifier — otherwise the alias name would be captured
; as a bogus namespace of its own.
(using_directive
  !name
  (identifier) @import.source @import.namespace)

(using_directive
  !name
  (qualified_name) @import.source @import.namespace)

; Alias directive: `using Json = Newtonsoft.Json;` binds a local name to a
; namespace. The alias is the ImportRec's local name; the target is still a
; whole-namespace import.
(using_directive
  name: (identifier) @import.alias
  (identifier) @import.source @import.namespace)

(using_directive
  name: (identifier) @import.alias
  (qualified_name) @import.source @import.namespace)
