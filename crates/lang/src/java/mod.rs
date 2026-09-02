//! Java adapter — Tier 0 defs plus Tier 1/2 imports and call edges.
//!
//! Defs are the class/interface/enum/record types and the methods, constructors
//! and fields they hold. Methods and fields are qualified by their enclosing
//! type (`Foo.getCount`) so same-named members on different classes stay distinct
//! and a field never collides with a same-named method. Imports capture the
//! dotted qualified name; refs capture bare and receiver-qualified method calls.

use crate::{LanguageAdapter, Workspace};
use std::path::{Path, PathBuf};
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
        "java"
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_java::LANGUAGE.into()
    }

    fn file_globs(&self) -> &'static [&'static str] {
        &["*.java"]
    }

    /// Maven/Gradle convention: production code lives under `src/main/`, tests
    /// under `src/test/`. Loose files fall back to the `*Test`/`*Tests` suffix.
    fn is_test_path(&self, rel: &str) -> bool {
        rel.contains("src/test/") || rel.ends_with("Test.java") || rel.ends_with("Tests.java")
    }

    fn tags_query(&self) -> &'static str {
        include_str!("queries/tags.scm")
    }

    fn imports_query(&self) -> Option<&'static str> {
        Some(include_str!("queries/imports.scm"))
    }

    fn refs_query(&self) -> Option<&'static str> {
        Some(include_str!("queries/refs.scm"))
    }

    /// Resolve a dotted FQN import to a source file inside the indexed tree.
    ///
    /// `com.google.gson.Gson` names the class `com/google/gson/Gson.java`, but the
    /// source root it sits under (`src/main/java`, `src/`, a bare module dir) isn't
    /// known up front. Rather than hunt for it, walk from the importing file's
    /// directory up through its ancestors and, at each, probe for the class file at
    /// the FQN's relative path. The first hit implicitly discovers the source root.
    /// A JDK or third-party class won't be in the tree and returns `None`, falling
    /// through to `external_dep_key`.
    fn resolve_import(&self, spec: &str, from: &Path, _ws: &Workspace) -> Option<PathBuf> {
        let rel = spec.replace('.', "/");
        let mut dir = from.parent();
        for _ in 0..8 {
            let ancestor = dir?;
            let candidate = ancestor.join(&rel).with_extension("java");
            if candidate.is_file() {
                return candidate.canonicalize().ok();
            }
            dir = ancestor.parent();
        }
        None
    }

    /// A dotted import that didn't resolve locally (JDK, a third-party jar) keys an
    /// external node by its full FQN, so `pkg.Class` calls through it still bind.
    fn external_dep_key(&self, spec: &str) -> Option<String> {
        (!spec.is_empty() && spec.contains('.')).then(|| spec.to_owned())
    }

    /// A definition is exported iff its `modifiers` list carries `public`.
    fn is_exported(&self, def: Node, _src: &[u8]) -> bool {
        let Some(mods) = modifiers(def) else {
            return false;
        };
        let mut c = mods.walk();
        let public = mods.children(&mut c).any(|m| m.kind() == "public");
        public
    }

    /// Methods and fields are qualified by the type they are declared in, so
    /// `Foo.count` and `Bar.count` are distinct symbols and a field `count`
    /// never hashes to the same `SymbolId` as a method `count`. Everything else
    /// (the types themselves) keeps its bare name.
    fn qualified_name(&self, kind: ir::NodeKind, name: &str, def: Node, src: &[u8]) -> String {
        match kind {
            ir::NodeKind::Method | ir::NodeKind::Field => match enclosing_type_name(def, src) {
                Some(owner) => format!("{owner}.{name}"),
                None => name.to_owned(),
            },
            _ => name.to_owned(),
        }
    }
}

/// The `modifiers` child of a declaration, if it has one. Not a named field in
/// tree-sitter-java, so it is found by kind among the direct children.
fn modifiers(def: Node) -> Option<Node> {
    let mut c = def.walk();
    let found = def.children(&mut c).find(|n| n.kind() == "modifiers");
    found
}

/// Name of the class/interface/enum/record a member is declared inside — walk up
/// to the first type declaration and read its `name`.
fn enclosing_type_name<'a>(def: Node, src: &'a [u8]) -> Option<&'a str> {
    let mut cur = def.parent();
    while let Some(n) = cur {
        if matches!(
            n.kind(),
            "class_declaration"
                | "interface_declaration"
                | "enum_declaration"
                | "record_declaration"
        ) {
            return n.child_by_field_name("name")?.utf8_text(src).ok();
        }
        cur = n.parent();
    }
    None
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

    /// Every def capture as `(kind, name, qualified name)`, mirroring what
    /// `parse::extract_defs` does with the same query.
    fn captures(src: &str) -> Vec<(String, String, String)> {
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
                if let Some(k) = cap_name.strip_prefix("def.") {
                    def = Some((k.to_owned(), cap.node));
                } else if cap_name == "name" {
                    name = cap.node.utf8_text(bytes).ok().map(str::to_owned);
                }
            }
            let (Some((k, node)), Some(n)) = (def, name) else {
                continue;
            };
            let Some(kind) = NodeKind::from_capture(&format!("def.{k}")) else {
                continue;
            };
            let qn = adapter.qualified_name(kind, &n, node, bytes);
            out.push((k, n, qn));
        }
        out.sort();
        out
    }

    /// The `(kind, name)` pairs a tags-only language lives or dies by.
    fn captured(src: &str) -> Vec<(String, String)> {
        captures(src).into_iter().map(|(k, n, _)| (k, n)).collect()
    }

    /// Captures keyed by qualified name — the string a `SymbolId` is hashed from.
    fn qualified(src: &str) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> =
            captures(src).into_iter().map(|(k, _, q)| (k, q)).collect();
        out.sort();
        out
    }

    #[test]
    fn queries_compile() {
        let adapter = Adapter::new();
        let lang = adapter.grammar();
        tree_sitter::Query::new(&lang, adapter.tags_query()).expect("tags.scm");
        tree_sitter::Query::new(&lang, adapter.imports_query().expect("imports.scm present"))
            .expect("imports.scm");
        tree_sitter::Query::new(&lang, adapter.refs_query().expect("refs.scm present"))
            .expect("refs.scm");
    }

    #[test]
    fn captures_class_interface_enum_method_field() {
        let caps = captured(
            "package p;\n\
             public class Foo {\n\
             \tprivate int count;\n\
             \tpublic int getCount() { return count; }\n\
             }\n\
             interface Doer { void doIt(); }\n\
             enum Color { RED, GREEN }\n\
             record Point(int x, int y) {}\n",
        );
        assert_eq!(
            caps,
            [
                ("class".to_owned(), "Foo".to_owned()),
                ("class".to_owned(), "Point".to_owned()),
                ("enum".to_owned(), "Color".to_owned()),
                ("field".to_owned(), "count".to_owned()),
                ("interface".to_owned(), "Doer".to_owned()),
                ("method".to_owned(), "doIt".to_owned()),
                ("method".to_owned(), "getCount".to_owned()),
            ]
        );
    }

    #[test]
    fn constructor_is_captured_as_a_method() {
        let caps = captured("package p;\nclass Foo {\n\tFoo(int c) {}\n}\n");
        assert!(caps.contains(&("method".to_owned(), "Foo".to_owned())));
    }

    /// `int a, b;` names two fields, each addressable as `obj.a` / `obj.b`.
    #[test]
    fn multi_declarator_field_yields_one_symbol_each() {
        let caps = captured("package p;\nclass Foo {\n\tint a, b;\n}\n");
        let fields: Vec<&(String, String)> = caps.iter().filter(|(k, _)| k == "field").collect();
        assert_eq!(
            fields,
            [
                &("field".to_owned(), "a".to_owned()),
                &("field".to_owned(), "b".to_owned()),
            ]
        );
    }

    #[test]
    fn methods_are_qualified_by_class() {
        let qns = qualified(
            "package p;\n\
             class Foo {\n\
             \tvoid run() {}\n\
             \tint count;\n\
             }\n\
             class Bar {\n\
             \tvoid run() {}\n\
             \tint count;\n\
             }\n",
        );
        let members: Vec<&(String, String)> = qns
            .iter()
            .filter(|(k, _)| k == "method" || k == "field")
            .collect();
        assert_eq!(
            members,
            [
                &("field".to_owned(), "Bar.count".to_owned()),
                &("field".to_owned(), "Foo.count".to_owned()),
                &("method".to_owned(), "Bar.run".to_owned()),
                &("method".to_owned(), "Foo.run".to_owned()),
            ]
        );
    }

    /// Unqualified, a field and a same-named method hash to one `SymbolId` and
    /// the loser is dropped; qualifying both by the class keeps them apart.
    #[test]
    fn a_field_does_not_collide_with_a_same_named_method() {
        let qns = qualified(
            "package p;\nclass Foo {\n\tint name;\n\tString name() { return \"\"; }\n}\n",
        );
        let members: Vec<&(String, String)> = qns
            .iter()
            .filter(|(k, _)| k == "method" || k == "field")
            .collect();
        assert_eq!(
            members,
            [
                &("field".to_owned(), "Foo.name".to_owned()),
                &("method".to_owned(), "Foo.name".to_owned()),
            ]
        );
    }

    #[test]
    fn public_drives_is_exported() {
        let src = "package p;\n\
             class Foo {\n\
             \tpublic int pub() { return 0; }\n\
             \tprivate int priv() { return 0; }\n\
             \tint pkg() { return 0; }\n\
             }\n";
        let tree = parse(src);
        let adapter = Adapter::new();
        let bytes = src.as_bytes();
        let flags: Vec<bool> = find(&tree, "method_declaration")
            .into_iter()
            .map(|m| adapter.is_exported(m, bytes))
            .collect();
        assert_eq!(flags, [true, false, false]);
    }

    /// A dotted FQN import resolves to the class file under the source root that
    /// the ancestor walk discovers, canonicalized.
    #[test]
    fn resolve_import_finds_class_under_source_root() {
        use std::sync::atomic::{AtomicU32, Ordering};

        static N: AtomicU32 = AtomicU32::new(0);
        let id = N.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("ripple-java-ri-{}-{id}", std::process::id()));
        let pkg = root.join("src/main/java/com/example");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("Foo.java"),
            "package com.example;\npublic class Foo {}\n",
        )
        .unwrap();
        std::fs::write(
            pkg.join("Bar.java"),
            "package com.example;\nimport com.example.Foo;\npublic class Bar {}\n",
        )
        .unwrap();

        let adapter = Adapter::new();
        let bar = pkg.join("Bar.java");
        let got = adapter.resolve_import("com.example.Foo", &bar, &Workspace::default());
        let want = pkg.join("Foo.java").canonicalize().ok();
        assert_eq!(got, want);

        // a class that isn't in the tree falls through to an external dep-key
        assert_eq!(
            adapter.resolve_import("com.google.gson.Gson", &bar, &Workspace::default()),
            None
        );
        assert_eq!(
            adapter.external_dep_key("com.google.gson.Gson"),
            Some("com.google.gson.Gson".to_owned())
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn is_test_path_follows_convention() {
        let adapter = Adapter::new();
        assert!(adapter.is_test_path("src/test/java/com/x/FooTest.java"));
        assert!(adapter.is_test_path("FooTests.java"));
        assert!(!adapter.is_test_path("src/main/java/com/x/Foo.java"));
    }
}
