; Svelte template component usage:  <Child />, <Child>…</Child>
;
; Rendering a component invokes it, so this is a call — without it a Svelte blast
; radius stops at the import edge, which says the file imported the component, not
; that anything renders it (the JSX case, #26).
;
; Capitalised names only. A lowercase `<div>`/`<span>` is an intrinsic element that
; names no symbol, and `<script>`/`<style>` are the SFC's own blocks; matching them
; would invent edges onto same-named functions (#51). Only the opening side is
; captured (`start_tag`, `self_closing_tag`) so `<Child></Child>` counts once.
((self_closing_tag
  (tag_name) @ref.call)
 (#match? @ref.call "^[A-Z]"))

((start_tag
  (tag_name) @ref.call)
 (#match? @ref.call "^[A-Z]"))
