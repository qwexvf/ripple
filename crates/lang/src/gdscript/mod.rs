//! GDScript adapter (Godot). Definitions and call sites only: `preload`/`load`
//! paths are not resolved, so a call into another script links through its
//! `class_name` (`Inventory.add()`) or by name.

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
        "gdscript"
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_gdscript::LANGUAGE.into()
    }

    fn file_globs(&self) -> &'static [&'static str] {
        &["*.gd"]
    }

    /// GUT and gdUnit both default to `test/` directories and `test_*.gd` files.
    fn is_test_path(&self, rel: &str) -> bool {
        let file = rel.rsplit('/').next().unwrap_or(rel);
        file.starts_with("test_") || rel.split('/').any(|seg| seg == "test" || seg == "tests")
    }

    fn tags_query(&self) -> &'static str {
        include_str!("queries/tags.scm")
    }

    fn refs_query(&self) -> Option<&'static str> {
        Some(include_str!("queries/refs.scm"))
    }

    /// GDScript has no visibility: a leading `_` is a convention, and Godot calls
    /// `_ready` and friends from outside.
    fn is_exported(&self, _def: Node, _src: &[u8]) -> bool {
        true
    }

    /// A function belongs to its inner `class`, else to the script, which is named
    /// by its `class_name` or, without one, by nothing: such a script is reached
    /// through `preload`, which is not resolved.
    fn qualified_name(&self, kind: ir::NodeKind, name: &str, def: Node, src: &[u8]) -> String {
        if kind != ir::NodeKind::Method {
            return name.to_owned();
        }
        match inner_class(def, src).or_else(|| script_class_name(def, src)) {
            Some(owner) => format!("{owner}.{name}"),
            None => name.to_owned(),
        }
    }
}

/// The `class Foo:` block enclosing `def`, if any.
fn inner_class<'a>(def: Node, src: &'a [u8]) -> Option<&'a str> {
    let mut cur = def.parent();
    while let Some(n) = cur {
        if n.kind() == "class_definition" {
            return n.child_by_field_name("name")?.utf8_text(src).ok();
        }
        cur = n.parent();
    }
    None
}

/// The script's `class_name`, read from the top level of the file.
fn script_class_name<'a>(def: Node, src: &'a [u8]) -> Option<&'a str> {
    let mut root = def;
    while let Some(p) = root.parent() {
        root = p;
    }
    let mut cursor = root.walk();
    let found = root
        .named_children(&mut cursor)
        .find(|n| n.kind() == "class_name_statement")?;
    found.child_by_field_name("name")?.utf8_text(src).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn captured(src: &str) -> Vec<(String, String)> {
        let adapter = Adapter::new();
        let lang = adapter.grammar();
        let query = tree_sitter::Query::new(&lang, adapter.tags_query()).expect("tags.scm");
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang).expect("grammar");
        let tree = parser.parse(src, None).expect("parse");
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
                } else if let Some(kind) = ir::NodeKind::from_capture(cap_name) {
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

    #[test]
    fn queries_compile() {
        let adapter = Adapter::new();
        let lang = adapter.grammar();
        tree_sitter::Query::new(&lang, adapter.tags_query()).expect("tags.scm");
        tree_sitter::Query::new(&lang, adapter.refs_query().expect("refs")).expect("refs.scm");
    }

    #[test]
    fn functions_belong_to_the_script_class() {
        let src = "class_name DigSite\nextends Node3D\n\nvar depth := 0\n\nfunc _ready():\n\tpass\n\nfunc _build_crew_figure() -> Node3D:\n\treturn null\n\nclass Crew:\n\tfunc wave():\n\t\tpass\n";
        let got = captured(src);
        for want in [
            ("Method", "DigSite._ready"),
            ("Method", "DigSite._build_crew_figure"),
            ("Method", "Crew.wave"),
            ("Class", "DigSite"),
            ("Class", "Crew"),
            ("Variable", "depth"),
        ] {
            assert!(
                got.contains(&(want.0.to_owned(), want.1.to_owned())),
                "{want:?} missing from {got:?}"
            );
        }
    }
}
