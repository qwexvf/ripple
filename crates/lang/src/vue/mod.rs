//! Vue adapter — a single-file component (SFC): `<template>` markup plus one or two
//! `<script>` blocks.
//!
//! Shares the SFC machinery with Svelte (`crate::sfc`): the component is a symbol
//! named by its file, minted by `synthetic_defs`, and every `<script>` body is a
//! TypeScript region handed to the TypeScript adapter (#46). Vue routinely has *two*
//! script blocks — `<script setup>` and a plain `<script>` — and both are returned,
//! which is the case #46's design exists for. A component rendered in the template
//! (`<Child />`) is a call, exactly as in Svelte and JSX (#26).
//!
//! The grammar is vendored (`tree-sitter-vue-vendored`): both published Vue crates
//! bundle an HTML scanner whose exported symbols collide with `tree-sitter-html`, so
//! ripple carries a copy with those dead symbols renamed. See #48.
//!
//! Out of scope for now, on purpose: kebab-case tags (`<my-widget/>`) as the same
//! component (needs name normalisation the resolver doesn't do yet — #48),
//! `defineProps`/`defineEmits` as an interface, `<style>` blocks, and options-API
//! `components: { … }` registration. Under-link, never wrong-link.

use crate::{LanguageAdapter, Workspace};
use std::path::{Path, PathBuf};
use tree_sitter::{Node, Range};

pub struct Adapter;

impl Adapter {
    pub fn new() -> Self {
        Adapter
    }
}

impl Default for Adapter {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAdapter for Adapter {
    fn id(&self) -> &'static str {
        "vue"
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_vue_vendored::language()
    }

    fn file_globs(&self) -> &'static [&'static str] {
        &["*.vue"]
    }

    /// The only symbol a `.vue` file defines is the component itself, which has no
    /// naming AST node and comes from `synthetic_defs`.
    fn tags_query(&self) -> &'static str {
        ""
    }

    /// `<Child />` / `<Child>…</Child>` in the template renders a component — a call.
    /// Capitalised names only (the shared SFC filter, #26/#51).
    fn refs_query(&self) -> Option<&'static str> {
        Some(include_str!("../sfc_component_refs.scm"))
    }

    /// The component symbol, named by the file (`sfc::component_def`).
    fn synthetic_defs(&self, module_path: &str, root: Node, _src: &[u8]) -> Vec<ir::Node> {
        vec![crate::sfc::component_def(module_path, root)]
    }

    /// Every `<script>` body, handed to the TypeScript adapter (`sfc::script_regions`) —
    /// both the `<script setup>` and a plain `<script>` when a file has both.
    fn embedded_regions(&self, root: Node, _src: &[u8]) -> Vec<(&'static str, Range)> {
        crate::sfc::script_regions(root)
    }

    /// A `<script>` import names a TypeScript file, so it resolves as TypeScript's
    /// own imports do. Delegated for the same reason the Svelte/HTML adapters do it.
    fn resolve_import(&self, spec: &str, from_file: &Path, ws: &Workspace) -> Option<PathBuf> {
        crate::typescript::Adapter::new().resolve_import(spec, from_file, ws)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse(src: &str) -> tree_sitter::Tree {
        let mut p = Parser::new();
        p.set_language(&Adapter::new().grammar()).unwrap();
        p.parse(src, None).unwrap()
    }

    #[test]
    fn the_component_is_a_file_named_symbol() {
        let tree = parse(
            "<script setup lang=\"ts\">let x = 1;</script>\n<template><p>{{x}}</p></template>\n",
        );
        let defs = Adapter::new().synthetic_defs("src/Child.vue", tree.root_node(), b"");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "Child");
        assert_eq!(defs[0].kind, ir::NodeKind::Component);
        assert!(defs[0].is_exported, "a default import must resolve to it");
    }

    #[test]
    fn both_script_blocks_become_typescript_regions() {
        // Vue's distinguishing case: a module `<script>` and a `<script setup>`
        let src = "<script lang=\"ts\">\nexport const meta = 1;\n</script>\n\
                   <script setup lang=\"ts\">\nimport { util } from './util';\nutil();\n</script>\n\
                   <template><Child /></template>\n";
        let tree = parse(src);
        let regions = Adapter::new().embedded_regions(tree.root_node(), src.as_bytes());
        assert_eq!(regions.len(), 2, "both script bodies are regions");
        for (id, _) in &regions {
            assert_eq!(*id, "typescript");
        }
        // sorted by position: the module block first, the setup block second
        let first = &src[regions[0].1.start_byte..regions[0].1.end_byte];
        let second = &src[regions[1].1.start_byte..regions[1].1.end_byte];
        assert!(first.contains("export const meta"));
        assert!(second.contains("import { util }"));
    }
}
