; C includes, modeled as namespace imports over the header file: the include
; binds the header as a whole, the way `import * as ns` does. Captured as both
; @import.source (the specifier) and @import.namespace (so an ImportRec is built —
; @import.source alone yields no record).
;
;   #include "jv.h"      → local header, resolved to a file
;   #include <stdio.h>   → system header, an external dependency

(preproc_include
  path: (string_literal
    (string_content) @import.source @import.namespace))

(preproc_include
  path: (system_lib_string) @import.source @import.namespace)
