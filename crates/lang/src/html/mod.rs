//! HTML adapter — a host format, not a language of its own.
//!
//! HTML defines no code symbols, so its `tags.scm` captures nothing. What ripple
//! wants from an `.html` file is the `<script>` block, which is JavaScript/
//! TypeScript. `embedded_regions` reports each script body's byte+point range and
//! names the TypeScript adapter for it; `parse` re-parses just that range with
//! `included_ranges`, so the script's symbols and call edges come back at their
//! real positions in the host file. This is the minimal proof of the embedded-
//! region seam (#46); the Vue and Svelte adapters reuse it.

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
        "html"
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_html::LANGUAGE.into()
    }

    fn file_globs(&self) -> &'static [&'static str] {
        &["*.html", "*.htm"]
    }

    fn tags_query(&self) -> &'static str {
        include_str!("queries/tags.scm")
    }

    /// Every `<script>` body, as a range in this file's own coordinates, handed to
    /// the TypeScript adapter. A page can hold several scripts (a module and a
    /// classic one), so all of them are returned; an empty `<script></script>`
    /// has no body node and contributes nothing.
    fn embedded_regions(&self, root: Node, _src: &[u8]) -> Vec<(&'static str, Range)> {
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "script_element" {
                let mut c = node.walk();
                for child in node.children(&mut c) {
                    // an empty `<script></script>` has a zero-width body node; there
                    // is nothing to parse, and including it would count as a region
                    if child.kind() == "raw_text" && !child.byte_range().is_empty() {
                        out.push(("typescript", child.range()));
                    }
                }
            }
            let mut c = node.walk();
            for child in node.children(&mut c) {
                stack.push(child);
            }
        }
        // determinism: the tree walk order is not guaranteed source order
        out.sort_by_key(|(_, r)| r.start_byte);
        out
    }

    /// A `<script>` import (`import { x } from "./util"`) names a TypeScript file,
    /// not another `.html`, so it resolves exactly as TypeScript's own imports do —
    /// including tsconfig paths and workspace packages. Resolution is keyed off the
    /// host file's path, so `link` asks *this* adapter; delegating keeps the answer
    /// right without teaching `link` which region an import came from.
    fn resolve_import(&self, spec: &str, from_file: &Path, ws: &Workspace) -> Option<PathBuf> {
        crate::typescript::Adapter::new().resolve_import(spec, from_file, ws)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn regions(src: &str) -> Vec<Range> {
        let mut parser = Parser::new();
        parser.set_language(&Adapter::new().grammar()).unwrap();
        let tree = parser.parse(src, None).unwrap();
        Adapter::new()
            .embedded_regions(tree.root_node(), src.as_bytes())
            .into_iter()
            .map(|(id, r)| {
                assert_eq!(id, "typescript");
                r
            })
            .collect()
    }

    #[test]
    fn every_script_body_is_a_region_in_host_coordinates() {
        // two script blocks (a module and a classic one, as Vue's setup+plain) and
        // an empty one that contributes nothing
        let src = "<html>\n\
                   <body>\n\
                   <script type=\"module\">const a = 1;</script>\n\
                   <p>x</p>\n\
                   <script>const b = 2;</script>\n\
                   <script></script>\n\
                   </body>\n\
                   </html>\n";
        let rs = regions(src);
        assert_eq!(rs.len(), 2, "both non-empty scripts, and not the empty one");
        // sorted by position, and the points are the host file's own rows (0-based)
        assert_eq!(rs[0].start_point.row, 2);
        assert_eq!(rs[1].start_point.row, 4);
        // the byte range slices back to exactly the script body
        assert_eq!(&src[rs[0].start_byte..rs[0].end_byte], "const a = 1;");
        assert_eq!(&src[rs[1].start_byte..rs[1].end_byte], "const b = 2;");
    }
}
