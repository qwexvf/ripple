//! Shared machinery for single-file-component adapters (Svelte, Vue).
//!
//! An SFC is one file that is both a component and a `<script>` block of another
//! language. Two things are identical across the frameworks: the component is a
//! symbol named by the file (no defining AST node), and every `<script>` body is a
//! TypeScript region in the host file's own coordinates. Only the grammar and the
//! template-call syntax differ, so those stay in each adapter; this is the rest.

use tree_sitter::{Node, Range};

/// The component symbol a `.svelte`/`.vue` file defines: named by the file stem,
/// exported (so a default import resolves to it), spanning the whole file (so a
/// render call in the markup attributes to it). There is no AST node carrying the
/// name, which is why this can't come from a `tags.scm` query — see #47.
pub fn component_def(module_path: &str, root: Node) -> ir::Node {
    let name = std::path::Path::new(module_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(module_path)
        .to_owned();
    ir::Node {
        id: ir::SymbolId::of(module_path, &name),
        kind: ir::NodeKind::Component,
        name: name.clone(),
        qualified_name: name,
        module_path: module_path.to_owned(),
        span: span_of(root),
        extra_spans: Vec::new(),
        is_exported: true,
        risk: ir::RiskScores::default(),
        doc: None,
        route_path: None,
    }
}

/// Every `<script>` body in the file, as a range in the host file's coordinates,
/// tagged for the TypeScript adapter. An SFC may hold more than one (Vue's
/// `<script setup>` plus a plain `<script>`, Svelte's `context="module"` block), so
/// all are returned; an empty `<script></script>` has a zero-width body and
/// contributes nothing. `parse` re-parses each with `included_ranges`, so the
/// script's symbols and edges land at their real host-file positions (#46).
pub fn script_regions(root: Node) -> Vec<(&'static str, Range)> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "script_element" {
            let mut c = node.walk();
            for child in node.children(&mut c) {
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

fn span_of(node: Node) -> ir::Span {
    let s = node.start_position();
    let e = node.end_position();
    ir::Span {
        start_line: s.row as u32 + 1,
        start_col: s.column as u32 + 1,
        end_line: e.row as u32 + 1,
        end_col: e.column as u32 + 1,
    }
}
