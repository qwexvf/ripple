//! Go adapter — Tier 0 only, on purpose.
//!
//! This is the breadth proof of docs/11 phase 4: a language added with `tags.scm`
//! and nothing else, whose call edges come from `gopls` instead of from a
//! hand-written refs query. So there is no `refs_query`, no `imports_query` and no
//! `bindings_query` here, and adding one would defeat the measurement rather than
//! improve it.

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
        "go"
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_go::LANGUAGE.into()
    }

    fn file_globs(&self) -> &'static [&'static str] {
        &["*.go"]
    }

    fn tags_query(&self) -> &'static str {
        include_str!("queries/tags.scm")
    }

    /// Go exports by case: an identifier is visible outside its package iff it
    /// starts with an uppercase letter.
    fn is_exported(&self, def: Node, src: &[u8]) -> bool {
        name_of(def, src).is_some_and(|n| n.chars().next().is_some_and(char::is_uppercase))
    }

    /// Methods are qualified by their receiver type, so `(*Terminal).Loop` and
    /// `(*Reader).Loop` stay distinct symbols. `bare_name` strips the qualifier
    /// again when reconciling against a server, which spells the same method
    /// `(*Terminal).Loop`.
    fn qualified_name(&self, kind: ir::NodeKind, name: &str, def: Node, src: &[u8]) -> String {
        if kind != ir::NodeKind::Method {
            return name.to_owned();
        }
        match receiver_type(def, src) {
            Some(ty) => format!("{ty}.{name}"),
            None => name.to_owned(),
        }
    }
}

/// The `name`/`field_identifier` a definition capture is anchored on. The tags
/// query captures the whole declaration, so the name has to be read back off it.
fn name_of<'a>(def: Node, src: &'a [u8]) -> Option<&'a str> {
    let named = def
        .child_by_field_name("name")
        .or_else(|| first_spec_name(def))?;
    named.utf8_text(src).ok()
}

/// `type`/`const`/`var` declarations hold their name one level down, on the spec.
fn first_spec_name(def: Node) -> Option<Node> {
    if let Some(name) = def.child_by_field_name("name") {
        return Some(name);
    }
    let mut c = def.walk();
    let found = def
        .children(&mut c)
        .find_map(|child| child.child_by_field_name("name"));
    found
}

/// Receiver type of a method, with the pointer and any type arguments stripped:
/// `func (t *Terminal[T]) Loop()` → `Terminal`. The pointer is not part of the
/// symbol's identity — `t.Loop()` and `(&t).Loop()` are the same method.
fn receiver_type<'a>(def: Node, src: &'a [u8]) -> Option<&'a str> {
    let receiver = def.child_by_field_name("receiver")?;
    let mut c = receiver.walk();
    let param = receiver.named_children(&mut c).next()?;
    let ty = param.child_by_field_name("type")?;
    Some(short_type(ty.utf8_text(src).ok()?))
}

/// `*Terminal[T]` → `Terminal`, `pkg.Thing` → `Thing`.
fn short_type(text: &str) -> &str {
    let no_ptr = text.trim_start_matches('*');
    let no_args = no_ptr.split('[').next().unwrap_or(no_ptr);
    no_args.rsplit('.').next().unwrap_or(no_args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ir::NodeKind;

    fn parse(src: &str) -> tree_sitter::Tree {
        let mut p = tree_sitter::Parser::new();
        p.set_language(&Adapter::new().grammar()).expect("grammar");
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

    fn methods(src: &str) -> Vec<String> {
        let tree = parse(src);
        let adapter = Adapter::new();
        let bytes = src.as_bytes();
        find(&tree, "method_declaration")
            .into_iter()
            .map(|m| {
                let name = m
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(bytes).ok())
                    .unwrap_or_default();
                adapter.qualified_name(NodeKind::Method, name, m, bytes)
            })
            .collect()
    }

    /// The captures a tags-only language lives or dies by: what the query matches
    /// is the entire set of places an LSP-reported call can be attributed to.
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
            let mut kind = None;
            let mut name = None;
            for cap in m.captures {
                let cap_name = names[cap.index as usize];
                if let Some(k) = cap_name.strip_prefix("def.") {
                    kind = Some(k.to_owned());
                } else if cap_name == "name" {
                    name = cap.node.utf8_text(bytes).ok().map(str::to_owned);
                }
            }
            if let (Some(k), Some(n)) = (kind, name) {
                out.push((k, n));
            }
        }
        out.sort();
        out
    }

    #[test]
    fn queries_compile() {
        let adapter = Adapter::new();
        tree_sitter::Query::new(&adapter.grammar(), adapter.tags_query()).expect("tags.scm");
    }

    #[test]
    fn tier_two_queries_are_absent_on_purpose() {
        let adapter = Adapter::new();
        assert!(adapter.refs_query().is_none(), "go calls come from gopls");
        assert!(adapter.imports_query().is_none());
        assert!(adapter.bindings_query().is_none());
    }

    #[test]
    fn methods_are_qualified_by_receiver_type() {
        let qns = methods(
            "package p\nfunc (t *Terminal) Loop() {}\nfunc (r Reader) Loop() {}\nfunc (t *Box[T]) Get() {}\n",
        );
        assert_eq!(qns, ["Terminal.Loop", "Reader.Loop", "Box.Get"]);
    }

    #[test]
    fn case_drives_is_exported() {
        let src = "package p\nfunc Public() {}\nfunc private() {}\n";
        let tree = parse(src);
        let adapter = Adapter::new();
        let bytes = src.as_bytes();
        let flags: Vec<bool> = find(&tree, "function_declaration")
            .into_iter()
            .map(|f| adapter.is_exported(f, bytes))
            .collect();
        assert_eq!(flags, [true, false]);
    }

    /// A struct must not be captured twice — once as a class and once by the
    /// catch-all `type` alternation.
    #[test]
    fn a_struct_is_captured_once_as_a_class() {
        let caps = captured("package p\ntype Item struct{ n int }\n");
        assert_eq!(caps, [("class".to_owned(), "Item".to_owned())]);
    }

    #[test]
    fn every_type_form_lands_in_some_kind() {
        let caps = captured(
            "package p\ntype Item struct{}\ntype Doer interface{}\ntype Offset int\ntype Fn func(int) error\ntype Alias = Item\n",
        );
        assert_eq!(
            caps,
            [
                ("class".to_owned(), "Item".to_owned()),
                ("interface".to_owned(), "Doer".to_owned()),
                ("type".to_owned(), "Alias".to_owned()),
                ("type".to_owned(), "Fn".to_owned()),
                ("type".to_owned(), "Offset".to_owned()),
            ]
        );
    }

    /// Package-level only: a local `var` is not a symbol another file can reach.
    #[test]
    fn locals_are_not_captured_as_variables() {
        let caps = captured("package p\nvar Top = 1\nfunc f() {\n\tinner := 2\n\tvar also = 3\n\t_ = inner + also\n}\n");
        let vars: Vec<&(String, String)> = caps.iter().filter(|(k, _)| k == "variable").collect();
        assert_eq!(vars, [&("variable".to_owned(), "Top".to_owned())]);
    }
}
