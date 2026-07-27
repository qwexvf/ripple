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

    /// `(kind, name, exported)` for every definition the tags query captures.
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
                if let Some(k) = cap_name.strip_prefix("def.") {
                    kind = Some(k.to_owned());
                    def = Some(cap.node);
                } else if cap_name == "name" {
                    name = cap.node.utf8_text(bytes).ok().map(str::to_owned);
                }
            }
            if let (Some(k), Some(n), Some(d)) = (kind, name, def) {
                out.push((k, n, adapter.is_exported(d, bytes)));
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
