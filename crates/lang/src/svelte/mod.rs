//! Svelte adapter — a single-file component (SFC): a `<script>` block plus markup.
//!
//! The component is the file. Svelte has no `export default` declaration to capture,
//! so the component symbol is minted by `synthetic_defs`, named by the file stem and
//! exported, so a default import (`import Child from './Child.svelte'`) resolves to
//! it. The `<script>` body is TypeScript: `embedded_regions` hands its range to the
//! TypeScript adapter, so imports/refs/bindings there resolve with no new logic
//! (#46). A component rendered in the markup (`<Child />`) is a call, exactly as JSX
//! is in TSX (#26) — the edge that makes a Svelte blast radius mean anything.
//!
//! Out of scope for now, on purpose: `export let` props as a public interface,
//! `$:` reactive statements, and stores. Under-link, never wrong-link.

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
        "svelte"
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_svelte_next::LANGUAGE.into()
    }

    fn file_globs(&self) -> &'static [&'static str] {
        &["*.svelte"]
    }

    /// No query captures a definition: the only symbol a `.svelte` file defines is
    /// the component itself, which has no naming AST node and comes from
    /// `synthetic_defs`.
    fn tags_query(&self) -> &'static str {
        ""
    }

    /// `<Child />` / `<Child>…</Child>` in the markup renders a component, which is a
    /// call. Capitalised names only — `<div>` and friends are intrinsic elements
    /// that name no symbol (the same filter TSX's JSX refs use, #26/#51).
    fn refs_query(&self) -> Option<&'static str> {
        Some(include_str!("../sfc_component_refs.scm"))
    }

    /// The component symbol, named by the file (`sfc::component_def`).
    fn synthetic_defs(&self, module_path: &str, root: Node, _src: &[u8]) -> Vec<ir::Node> {
        vec![crate::sfc::component_def(module_path, root)]
    }

    /// Every `<script>` body, handed to the TypeScript adapter (`sfc::script_regions`).
    /// Svelte allows a `context="module"` block alongside the instance one; both are
    /// returned.
    fn embedded_regions(&self, root: Node, _src: &[u8]) -> Vec<(&'static str, Range)> {
        crate::sfc::script_regions(root)
    }

    /// A `<script>` import (`import { util } from "./util"`) names a TypeScript file,
    /// so it resolves exactly as TypeScript's own imports do — including tsconfig
    /// paths and workspace packages. Delegated for the same reason the HTML adapter
    /// delegates: `link` asks the host file's adapter, and the import is TypeScript's.
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
        let tree = parse("<script lang=\"ts\">let x = 1;</script>\n<p>{x}</p>\n");
        let defs = Adapter::new().synthetic_defs("src/Child.svelte", tree.root_node(), b"");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "Child");
        assert_eq!(defs[0].kind, ir::NodeKind::Component);
        assert!(defs[0].is_exported, "a default import must resolve to it");
    }

    #[test]
    fn the_script_body_is_a_typescript_region_in_host_coordinates() {
        let src = "<script lang=\"ts\">\n  import { util } from './util';\n</script>\n<Child />\n";
        let tree = parse(src);
        let regions = Adapter::new().embedded_regions(tree.root_node(), src.as_bytes());
        assert_eq!(regions.len(), 1);
        let (id, r) = regions[0];
        assert_eq!(id, "typescript");
        // the byte range slices back to the script body in the host file's own
        // coordinates — no offset arithmetic (the whole point of #46)
        let body = &src[r.start_byte..r.end_byte];
        assert!(body.contains("import { util } from './util';"));
        assert!(
            !body.contains("<script"),
            "the tag itself is not in the region"
        );
    }
}
