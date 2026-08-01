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

    /// Integration tests only. Rust's unit tests live in a `#[cfg(test)] mod tests`
    /// inside the file under test, which no path can see — `test_scopes` finds those.
    fn is_test_path(&self, rel: &str) -> bool {
        rel.starts_with("tests/") || rel.contains("/tests/")
    }

    fn test_scopes(&self, root: Node, src: &[u8]) -> Vec<ir::Span> {
        cfg_test_scopes(root, src)
    }

    fn tags_query(&self) -> &'static str {
        include_str!("queries/tags.scm")
    }

    fn refs_query(&self) -> Option<&'static str> {
        Some(include_str!("queries/refs.scm"))
    }

    /// `pub` in any form (`pub`, `pub(crate)`, …). A function in a private module
    /// still reads as exported; module-level privacy needs the module tree.
    ///
    /// A variant carries no visibility of its own — the grammar forbids it — so it
    /// takes the enum's. Struct fields do not: `pub struct S { a: u32 }` really does
    /// keep `a` private.
    fn is_exported(&self, def: Node, src: &[u8]) -> bool {
        let def = if def.kind() == "enum_variant" {
            enclosing(def, &["enum_item"]).unwrap_or(def)
        } else {
            def
        };
        let mut c = def.walk();
        let found = def.children(&mut c).any(|n| {
            n.kind() == "visibility_modifier"
                && n.utf8_text(src).is_ok_and(|v| v.starts_with("pub"))
        });
        found
    }

    /// Methods are qualified by their `impl` type, so same-named methods on
    /// different types don't collide on one `SymbolId`. Variants and fields are
    /// qualified by the type that owns them, for the same reason.
    fn qualified_name(&self, kind: ir::NodeKind, name: &str, def: Node, src: &[u8]) -> String {
        match kind {
            ir::NodeKind::Function => match impl_type(def, src) {
                Some(ty) => format!("{ty}::{name}"),
                None => name.to_owned(),
            },
            ir::NodeKind::Field => member_path(def, name, src).unwrap_or_else(|| name.to_owned()),
            _ => name.to_owned(),
        }
    }
}

/// Qualified name of a variant or field, written the way Rust writes the access:
/// `Kind::Route` for a variant, `Node.span` for a field, `Kind::Route.path` for a
/// field of a struct variant.
///
/// The separator is load-bearing. Identity is (path, qualified name), so a field
/// spelled `Config::path` would share a `SymbolId` with its own getter
/// `fn path(&self)` on `impl Config` — the two would collapse into one node and
/// whichever the query engine yielded first would swallow the other.
fn member_path(def: Node, name: &str, src: &[u8]) -> Option<String> {
    let sep = match def.kind() {
        "enum_variant" => "::",
        "field_declaration" => ".",
        _ => return None,
    };
    let owner = owner_path(def, src)?;
    Some(format!("{owner}{sep}{name}"))
}

/// Qualified name of the type-ish definition enclosing `node`. Itself qualified when
/// that is a variant, so `E::V.x` and `F::V.x` stay distinct.
fn owner_path(node: Node, src: &[u8]) -> Option<String> {
    let owner = enclosing(
        node,
        &["struct_item", "union_item", "enum_item", "enum_variant"],
    )?;
    let name = owner.child_by_field_name("name")?.utf8_text(src).ok()?;
    Some(member_path(owner, name, src).unwrap_or_else(|| name.to_owned()))
}

/// Nearest ancestor of `node` whose kind is one of `kinds`.
fn enclosing<'a>(node: Node<'a>, kinds: &[&str]) -> Option<Node<'a>> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if kinds.contains(&n.kind()) {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

/// The type of the `impl` block enclosing `def`, if any. Uses the `type` field so
/// `impl Trait for Type` yields `Type` (the trait is a separate node).
/// Every `mod` gated on `cfg(test)`, as spans.
///
/// A tags-query capture cannot do this reliably. The attribute is a *preceding
/// sibling* of the module, so the pattern needs an anchor — and the anchor then
/// requires it to be the immediately preceding one, which `#[cfg(test)]`
/// `#[allow(…)]` `mod tests` violates. Matching the attribute's text needs a
/// regex predicate, and this project's query engine treats an unsupported
/// predicate as *passing*, which would mark every `mod` in a repo as tests.
/// Walking the tree costs one pass and has neither failure mode.
fn cfg_test_scopes(root: Node, src: &[u8]) -> Vec<ir::Span> {
    let mut out = Vec::new();
    let mut cursor = root.walk();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
        if node.kind() != "mod_item" || !gated_on_cfg_test(node, src) {
            continue;
        }
        let (s, e) = (node.start_position(), node.end_position());
        out.push(ir::Span {
            start_line: s.row as u32 + 1,
            start_col: s.column as u32 + 1,
            end_line: e.row as u32 + 1,
            end_col: e.column as u32 + 1,
        });
    }
    out.sort_by_key(|s| (s.start_line, s.start_col));
    out
}

/// Is any attribute attached to this item a `cfg` naming the `test` feature?
/// Walks the whole run of attributes, so a `#[cfg(test)]` behind an `#[allow]`
/// still counts, and reads `cfg(all(test, …))` too.
fn gated_on_cfg_test(item: Node, src: &[u8]) -> bool {
    let mut prev = item.prev_named_sibling();
    while let Some(node) = prev {
        if node.kind() != "attribute_item" {
            return false;
        }
        if let Ok(text) = node.utf8_text(src) {
            let after_cfg = text.strip_prefix("#[").map(str::trim_start);
            if after_cfg.is_some_and(|t| t.starts_with("cfg")) && names_test(text) {
                return true;
            }
        }
        prev = node.prev_named_sibling();
    }
    false
}

/// `test` as a whole token, so `cfg(feature = "testing")` doesn't qualify.
fn names_test(attr: &str) -> bool {
    attr.split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|tok| tok == "test")
}

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

    /// What tags.scm actually yields: (capture kind, qualified name), sorted.
    fn captured(src: &str) -> Vec<(String, String)> {
        let adapter = Adapter::new();
        let lang = adapter.grammar();
        let query = tree_sitter::Query::new(&lang, adapter.tags_query()).expect("tags.scm");
        let tree = parse(src);
        let bytes = src.as_bytes();
        let names = query.capture_names();
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), bytes);
        let mut out = Vec::new();
        while let Some(m) = streaming_iterator::StreamingIterator::next(&mut matches) {
            let mut def = None;
            let mut name = None;
            for cap in m.captures {
                let cap_name = names[cap.index as usize];
                if cap_name == "name" {
                    name = cap.node.utf8_text(bytes).ok();
                } else if let Some(kind) = NodeKind::from_capture(cap_name) {
                    def = Some((kind, cap.node));
                }
            }
            if let (Some((kind, node)), Some(name)) = (def, name) {
                out.push((
                    format!("{kind:?}"),
                    adapter.qualified_name(kind, name, node, bytes),
                ));
            }
        }
        out.sort();
        out
    }

    /// `is_exported` for every node of `kind`, paired with its name.
    fn exports(src: &str, kind: &str) -> Vec<(String, bool)> {
        let tree = parse(src);
        let adapter = Adapter::new();
        let bytes = src.as_bytes();
        find(&tree, kind)
            .into_iter()
            .map(|n| {
                let name = n
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(bytes).ok())
                    .unwrap_or_default()
                    .to_owned();
                (name, adapter.is_exported(n, bytes))
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
        let flags = exports(
            "pub fn public() {}\nfn private() {}\npub(crate) fn crate_wide() {}\n",
            "function_item",
        );
        assert_eq!(
            flags,
            [
                ("public".to_owned(), true),
                ("private".to_owned(), false),
                ("crate_wide".to_owned(), true),
            ]
        );
    }

    /// The enum was a symbol but its variants were not, so a `match` on
    /// `Kind::Route` depended on nothing a reviewer could see, and adding or
    /// removing a variant looked like it touched no symbol at all.
    #[test]
    fn enum_variants_are_captured_as_fields() {
        let caps = captured("pub enum Kind { Route, Channel }\n");
        assert_eq!(
            caps,
            [
                ("Enum".to_owned(), "Kind".to_owned()),
                ("Field".to_owned(), "Kind::Channel".to_owned()),
                ("Field".to_owned(), "Kind::Route".to_owned()),
            ]
        );
    }

    /// Two enums in a file routinely share a variant name (`None`, `Other`).
    /// Unqualified they'd be one `SymbolId` and collapse into a single node.
    #[test]
    fn variants_of_different_enums_stay_distinct() {
        let caps = captured("enum A { Other }\nenum B { Other }\n");
        let fields: Vec<&String> = caps
            .iter()
            .filter(|(k, _)| k == "Field")
            .map(|(_, q)| q)
            .collect();
        assert_eq!(fields, ["A::Other", "B::Other"]);
    }

    /// Reading `node.span` depends on the field, not on the whole struct. Union
    /// and struct-variant bodies reuse `field_declaration`, so they come along.
    #[test]
    fn named_struct_union_and_variant_fields_are_captured() {
        let caps = captured(
            "struct S { a: u32 }\nunion U { b: u32 }\nenum E { V { c: u32 } }\nstruct T(u32);\n",
        );
        let fields: Vec<&String> = caps
            .iter()
            .filter(|(k, _)| k == "Field")
            .map(|(_, q)| q)
            .collect();
        // `struct T(u32)` has no field name to address, so it contributes nothing
        assert_eq!(fields, ["E::V", "E::V.c", "S.a", "U.b"]);
    }

    /// Identity is (path, qualified name). Spelling the field `Config::path` would
    /// give it the id of its own getter, and one would swallow the other.
    #[test]
    fn a_field_and_its_getter_are_separate_symbols() {
        let caps = captured(
            "struct Config { path: u32 }\nimpl Config { pub fn path(&self) -> u32 { self.path } }\n",
        );
        assert!(
            caps.contains(&("Field".to_owned(), "Config.path".to_owned())),
            "got {caps:?}"
        );
        assert!(
            caps.contains(&("Function".to_owned(), "Config::path".to_owned())),
            "got {caps:?}"
        );
    }

    /// A variant is exactly as reachable as its enum, and the grammar gives it no
    /// visibility of its own — read literally, every variant is private.
    #[test]
    fn variants_inherit_the_enums_visibility() {
        assert_eq!(
            exports("pub enum A { X }\nenum B { Y }\n", "enum_variant"),
            [("X".to_owned(), true), ("Y".to_owned(), false)]
        );
    }

    /// Fields do not inherit: `pub struct S { a: u32 }` really does keep `a`
    /// private, and marking it exported would offer callers a symbol they cannot
    /// name.
    #[test]
    fn struct_fields_keep_their_own_visibility() {
        assert_eq!(
            exports("pub struct S { pub a: u32, b: u32 }\n", "field_declaration"),
            [("a".to_owned(), true), ("b".to_owned(), false)]
        );
    }

    /// Locals are not addressable from anywhere else, so capturing them would add
    /// noise to every name lookup. Only module-level bindings are symbols.
    #[test]
    fn locals_and_parameters_are_not_captured() {
        let caps = captured(
            "const TOP: u32 = 1;\nfn f(arg: u32) {\n    let local = 2;\n    let g = |p: u32| p;\n    match arg { other => other };\n}\n",
        );
        assert_eq!(
            caps,
            [
                ("Function".to_owned(), "f".to_owned()),
                ("Variable".to_owned(), "TOP".to_owned()),
            ]
        );
    }
}
