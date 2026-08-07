; Gleam call sites. Both forms Gleam allows:
;
; Bare call — `luhn(digits)`. The function is an `identifier` in call position;
; it resolves to a same-module definition or an unqualified import.
(function_call
  function: (identifier) @ref.call)

; Qualified call — `scrub.payload(x)`. Gleam parses `module.function` as a field
; access (it can't tell a module from a record at parse time), so the receiver is
; the module alias and the label is the function. Modelled as a member call whose
; receiver is pinned by the `import` that bound the alias — the same shape as a
; TypeScript `import * as ns` namespace call.
(function_call
  function: (field_access
    record: (identifier) @ref.recv
    field: (label) @ref.member))
