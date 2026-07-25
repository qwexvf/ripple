//! Rust adapter (Tier 0–2 syntactic). Exists so ripple can index its own source
//! and be used on itself — see `.claude/skills/use-ripple`.
//!
//! Cross-file resolution is limited on purpose: `use` paths are not mapped to
//! files, because that needs the module tree (`mod` declarations, `crate::`/
//! `super::` prefixes, `lib.rs`/`mod.rs` conventions). Same-file calls resolve;
//! cross-file ones await that work. Under-link, never wrong-link.

use crate::LanguageAdapter;
use tree_sitter::Node;

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
        "rust"
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_rust::LANGUAGE.into()
    }

    fn file_globs(&self) -> &'static [&'static str] {
        &["*.rs"]
    }

    fn tags_query(&self) -> &'static str {
        include_str!("queries/tags.scm")
    }

    fn refs_query(&self) -> Option<&'static str> {
        Some(include_str!("queries/refs.scm"))
    }

    /// `pub` in any form (`pub`, `pub(crate)`, …). A function in a private module
    /// still reads as exported; module-level privacy needs the module tree.
    fn is_exported(&self, def: Node, src: &[u8]) -> bool {
        let mut c = def.walk();
        let found = def.children(&mut c).any(|n| {
            n.kind() == "visibility_modifier"
                && n.utf8_text(src).is_ok_and(|v| v.starts_with("pub"))
        });
        found
    }

    /// Methods are qualified by their `impl` type, so same-named methods on
    /// different types don't collide on one `SymbolId`.
    fn qualified_name(&self, kind: ir::NodeKind, name: &str, def: Node, src: &[u8]) -> String {
        if kind != ir::NodeKind::Function {
            return name.to_owned();
        }
        match impl_type(def, src) {
            Some(ty) => format!("{ty}::{name}"),
            None => name.to_owned(),
        }
    }
}

/// The type of the `impl` block enclosing `def`, if any. Uses the `type` field so
/// `impl Trait for Type` yields `Type` (the trait is a separate node).
fn impl_type<'a>(def: Node, src: &'a [u8]) -> Option<&'a str> {
    let mut node = def;
    while let Some(parent) = node.parent() {
        if parent.kind() == "impl_item" {
            let ty = parent.child_by_field_name("type")?;
            return ty.utf8_text(src).ok().map(short_type);
        }
        node = parent;
    }
    None
}

/// `Vec<Edge>` → `Vec`: a generic argument list isn't part of the type's identity
/// for symbol purposes.
fn short_type(text: &str) -> &str {
    text.split(['<', ' ']).next().unwrap_or(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ir::NodeKind;

    fn parse(src: &str) -> tree_sitter::Tree {
        let mut p = tree_sitter::Parser::new();
        p.set_language(&Adapter::new().grammar())
            .expect("rust grammar");
        p.parse(src, None).expect("parse")
    }

    /// Every node of `kind`, in document order.
    fn find<'a>(tree: &'a tree_sitter::Tree, kind: &str) -> Vec<Node<'a>> {
        fn walk<'a>(node: Node<'a>, kind: &str, out: &mut Vec<Node<'a>>) {
            if node.kind() == kind {
                out.push(node);
            }
            let mut c = node.walk();
            for child in node.children(&mut c) {
                walk(child, kind, out);
            }
        }
        let mut out = Vec::new();
        walk(tree.root_node(), kind, &mut out);
        out
    }

    fn qualified(src: &str) -> Vec<String> {
        let tree = parse(src);
        let adapter = Adapter::new();
        let bytes = src.as_bytes();
        find(&tree, "function_item")
            .into_iter()
            .map(|f| {
                let name = f
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(bytes).ok())
                    .unwrap_or_default();
                adapter.qualified_name(NodeKind::Function, name, f, bytes)
            })
            .collect()
    }

    #[test]
    fn queries_compile() {
        let adapter = Adapter::new();
        let lang = adapter.grammar();
        tree_sitter::Query::new(&lang, adapter.tags_query()).expect("tags.scm");
        tree_sitter::Query::new(&lang, adapter.refs_query().expect("refs.scm present"))
            .expect("refs.scm");
    }

    #[test]
    fn methods_are_qualified_by_impl_type() {
        let qns = qualified(
            "struct A;\nstruct B;\nimpl A { pub fn start(&self) {} }\nimpl B { fn start(&self) {} }\nfn free() {}\n",
        );
        assert!(qns.contains(&"A::start".to_owned()), "got {qns:?}");
        assert!(qns.contains(&"B::start".to_owned()), "got {qns:?}");
        // a free function keeps its bare name
        assert!(qns.contains(&"free".to_owned()), "got {qns:?}");
    }

    #[test]
    fn trait_impls_are_qualified_by_the_type_not_the_trait() {
        let qns = qualified("struct Client;\nimpl Drop for Client { fn drop(&mut self) {} }\n");
        assert_eq!(qns, ["Client::drop"]);
    }

    #[test]
    fn generic_impl_types_lose_their_arguments() {
        let qns = qualified("struct W<T>(T);\nimpl<T> W<T> { fn get(&self) {} }\n");
        assert_eq!(qns, ["W::get"]);
    }

    #[test]
    fn visibility_drives_is_exported() {
        let src = "pub fn public() {}\nfn private() {}\npub(crate) fn crate_wide() {}\n";
        let tree = parse(src);
        let adapter = Adapter::new();
        let bytes = src.as_bytes();
        let flags: Vec<(String, bool)> = find(&tree, "function_item")
            .into_iter()
            .map(|f| {
                let name = f
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(bytes).ok())
                    .unwrap_or_default()
                    .to_owned();
                (name, adapter.is_exported(f, bytes))
            })
            .collect();
        assert_eq!(
            flags,
            [
                ("public".to_owned(), true),
                ("private".to_owned(), false),
                ("crate_wide".to_owned(), true),
            ]
        );
    }
}
