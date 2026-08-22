//! Vendored Vue grammar.
//!
//! Why vendored rather than a crates.io dependency: both published Vue grammars
//! (`tree-sitter-vue-updated`, `tree-sitter-vue`) bundle an HTML sub-grammar whose
//! scanner exports `tree_sitter_html_external_scanner_*` — the exact symbols
//! `tree-sitter-html` 0.23 exports. Linking both into one binary merges those
//! symbols and silently breaks ripple's HTML adapter (its script regions vanish).
//! Those five functions are dead here — the Vue scanner uses the bundled `Scanner`
//! class directly — so this copy renames them to `tree_sitter_vue_vendored_html_*`.
//! That is the only change from upstream; the grammar is otherwise verbatim. See the
//! sibling NOTICE for provenance and license (MIT, c-gamble/tree-sitter-vue), and
//! ripple issue #48.

use tree_sitter::Language;

extern "C" {
    fn tree_sitter_vue() -> Language;
}

/// The Vue grammar (ABI 14, compatible with tree-sitter 0.25).
#[must_use]
pub fn language() -> Language {
    unsafe { tree_sitter_vue() }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_grammar_loads_and_parses() {
        let mut p = tree_sitter::Parser::new();
        p.set_language(&super::language()).unwrap();
        let tree = p
            .parse(
                "<script setup lang=\"ts\">const a = 1;</script>\n<template><Child /></template>\n",
                None,
            )
            .unwrap();
        assert!(!tree.root_node().has_error());
        assert_eq!(super::language().abi_version(), 14);
    }
}
