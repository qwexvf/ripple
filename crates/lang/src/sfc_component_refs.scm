; Single-file-component template usage:  <Child />, <Child>…</Child>
;
; Rendering a component invokes it, so this is a call — without it an SFC blast
; radius stops at the import edge, which says the file imported the component, not
; that anything renders it (the JSX case, #26). Shared by the Svelte and Vue
; adapters: both grammars name the tag `tag_name` under `self_closing_tag` /
; `start_tag`.
;
; Capitalised names only. A lowercase `<div>`/`<span>` is an intrinsic element that
; names no symbol, and `<script>`/`<style>`/`<template>` are the SFC's own blocks;
; matching them would invent edges onto same-named functions (#51). Only the opening
; side is captured so `<Child></Child>` counts once. (Kebab-case component tags like
; `<my-widget/>` are out of scope until the resolver normalises them, #48.)
((self_closing_tag
  (tag_name) @ref.call)
 (#match? @ref.call "^[A-Z]"))

((start_tag
  (tag_name) @ref.call)
 (#match? @ref.call "^[A-Z]"))
