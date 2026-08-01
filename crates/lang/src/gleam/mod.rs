//! Gleam adapter — Tier 0, with call edges from `gleam lsp`.
//!
//! Same shape as the Go adapter and for the same reason (docs/11 phase 4): the
//! language server is a better call resolver than a hand-written refs query would
//! be, so there is no `refs_query` here. What makes Gleam worth adding first is
//! measurement rather than preference — it is the backend language of the repos
//! where cross-service resolution was emitting zero edges, because the producer
//! side of every GraphQL join was unparsed. See issue #40.

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
        "gleam"
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_gleam::LANGUAGE.into()
    }

    fn file_globs(&self) -> &'static [&'static str] {
        &["*.gleam"]
    }

    fn is_test_path(&self, rel: &str) -> bool {
        rel.ends_with("_test.gleam") || rel.starts_with("test/") || rel.contains("/test/")
    }

    fn tags_query(&self) -> &'static str {
        include_str!("queries/tags.scm")
    }

    /// `pub` makes a definition visible outside its module. A data constructor has
    /// no modifier of its own — it is exported with the type that declares it, so
    /// the `pub` is found by walking up.
    fn is_exported(&self, def: Node, src: &[u8]) -> bool {
        let mut node = Some(def);
        while let Some(n) = node {
            if has_pub(n, src) {
                return true;
            }
            match n.kind() {
                // stop at the declaration boundary: a `pub` further up would belong
                // to something else entirely
                "source_file" => return false,
                _ => node = n.parent(),
            }
        }
        false
    }

    /// A record field is named by the type that declares it, not on its own:
    /// identity is (path, qualified name), so `Person(name: String)` and
    /// `Company(name: String)` in one module would otherwise collapse into a
    /// single `name` symbol.
    ///
    /// The owner is the *type*, not the constructor, because that is what the
    /// access is written against — `person.name` is legal exactly when every
    /// constructor of the type declares the label, and the two declarations are
    /// then two definition sites of one symbol.
    fn qualified_name(&self, kind: ir::NodeKind, name: &str, def: Node, src: &[u8]) -> String {
        if kind != ir::NodeKind::Field {
            return name.to_owned();
        }
        match enclosing_type_name(def, src) {
            Some(ty) => format!("{ty}.{name}"),
            None => name.to_owned(),
        }
    }
}

/// Name of the `type` declaration a constructor argument sits in.
fn enclosing_type_name(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.kind() == "type_definition" {
            let mut c = n.walk();
            let type_name = n
                .named_children(&mut c)
                .find(|ch| ch.kind() == "type_name")?;
            let name = type_name.child_by_field_name("name")?;
            return name.utf8_text(src).ok().map(str::to_owned);
        }
        cur = n.parent();
    }
    None
}

fn has_pub(node: Node, src: &[u8]) -> bool {
    let mut c = node.walk();
    let found = node.children(&mut c).any(|child| {
        child.kind() == "visibility_modifier" && child.utf8_text(src).is_ok_and(|v| v == "pub")
    });
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> tree_sitter::Tree {
        let mut p = tree_sitter::Parser::new();
        p.set_language(&Adapter::new().grammar()).expect("grammar");
        p.parse(src, None).expect("parse")
    }

    /// `(kind, qualified name, exported)` for every definition the tags query
    /// captures. Qualification is part of what a capture yields — a field that
    /// forgot it would share a `SymbolId` with the next type's same-named field.
    fn captured(src: &str) -> Vec<(String, String, bool)> {
        let adapter = Adapter::new();
        let lang = adapter.grammar();
        let query = tree_sitter::Query::new(&lang, adapter.tags_query()).expect("tags.scm");
        let names = query.capture_names();
        let tree = parse(src);
        let bytes = src.as_bytes();
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), bytes);
        let mut out = Vec::new();
        while let Some(m) = streaming_iterator::StreamingIterator::next(&mut matches) {
            let mut kind = None;
            let mut name = None;
            let mut def = None;
            for cap in m.captures {
                let cap_name = names[cap.index as usize];
                if let Some(k) = ir::NodeKind::from_capture(cap_name) {
                    kind = Some(k);
                    def = Some(cap.node);
                } else if cap_name == "name" {
                    name = cap.node.utf8_text(bytes).ok().map(str::to_owned);
                }
            }
            if let (Some(k), Some(n), Some(d)) = (kind, name, def) {
                out.push((
                    format!("{k:?}").to_lowercase(),
                    adapter.qualified_name(k, &n, d, bytes),
                    adapter.is_exported(d, bytes),
                ));
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
        assert!(
            adapter.refs_query().is_none(),
            "gleam calls come from gleam lsp"
        );
        assert!(adapter.imports_query().is_none());
        assert!(adapter.bindings_query().is_none());
    }

    #[test]
    fn functions_types_constructors_and_constants_are_captured() {
        let caps = captured(
            "import gleam/io\n\
             pub const limit = 10\n\
             const secret = 3\n\
             pub type Shape {\n  Circle(Float)\n  Square(Float)\n}\n\
             pub type Meters = Int\n\
             pub fn area(s: Shape) -> Float { 0.0 }\n\
             fn helper() { Nil }\n",
        );
        assert_eq!(
            caps,
            [
                ("function".to_owned(), "Circle".to_owned(), true),
                ("function".to_owned(), "Square".to_owned(), true),
                ("function".to_owned(), "area".to_owned(), true),
                ("function".to_owned(), "helper".to_owned(), false),
                ("type".to_owned(), "Meters".to_owned(), true),
                ("type".to_owned(), "Shape".to_owned(), true),
                ("variable".to_owned(), "limit".to_owned(), true),
                ("variable".to_owned(), "secret".to_owned(), false),
            ]
        );
    }

    /// A labelled constructor argument is what makes `person.name` legal, so the
    /// label is a symbol; before it was captured, every record field in a Gleam
    /// repo was invisible. Two types declaring `name` must stay two symbols, and
    /// a positional argument declares no label to capture.
    #[test]
    fn labelled_constructor_arguments_are_fields_qualified_by_their_type() {
        let caps = captured(
            "pub type Person {\n  Person(name: String, age: Int)\n}\n\
             pub type Company {\n  Company(name: String)\n}\n\
             pub type Shape {\n  Circle(Float)\n}\n",
        );
        assert_eq!(
            caps,
            [
                ("field".to_owned(), "Company.name".to_owned(), true),
                ("field".to_owned(), "Person.age".to_owned(), true),
                ("field".to_owned(), "Person.name".to_owned(), true),
                ("function".to_owned(), "Circle".to_owned(), true),
                ("function".to_owned(), "Company".to_owned(), true),
                ("function".to_owned(), "Person".to_owned(), true),
                ("type".to_owned(), "Company".to_owned(), true),
                ("type".to_owned(), "Person".to_owned(), true),
                ("type".to_owned(), "Shape".to_owned(), true),
            ]
        );
    }

    /// A constructor carries no `pub` of its own — it is exported with its type,
    /// so a private type's constructors must not read as exported.
    #[test]
    fn constructors_inherit_their_types_visibility() {
        let caps = captured("type Hidden {\n  Inner(Int)\n}\n");
        assert_eq!(
            caps,
            [
                ("function".to_owned(), "Inner".to_owned(), false),
                ("type".to_owned(), "Hidden".to_owned(), false),
            ]
        );
    }
}
