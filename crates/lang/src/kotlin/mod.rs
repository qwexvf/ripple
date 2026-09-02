//! Kotlin adapter — Tier 0 defs plus Tier 1/2 imports and call edges.
//!
//! Kotlin's grammar (`tree-sitter-kotlin-ng`) reuses `class_declaration` for
//! classes, interfaces and `enum class`, and has no separate method or field
//! node — a member is a `function_declaration`/`property_declaration` that
//! happens to sit in a `class_body`. So the tags query does the sorting by scope
//! (top level vs. member) and by keyword, and [`Adapter::qualified_name`] prefixes
//! the owning type onto members. Identifiers in this grammar are `identifier`, not
//! the `simple_identifier` older Kotlin grammars use.

use crate::LanguageAdapter;
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
        "kotlin"
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_kotlin_ng::LANGUAGE.into()
    }

    fn file_globs(&self) -> &'static [&'static str] {
        &["*.kt", "*.kts"]
    }

    /// Gradle/Maven convention: production code lives under `src/main/`, tests
    /// under `src/test/` (and `src/androidTest/` on Android).
    fn is_test_path(&self, rel: &str) -> bool {
        rel.contains("src/test/") || rel.contains("src/androidTest/")
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

    /// Kotlin is public by default: a declaration is exported unless it carries a
    /// `private`, `internal` or `protected` visibility modifier.
    fn is_exported(&self, def: Node, src: &[u8]) -> bool {
        !matches!(
            visibility(def, src).as_deref(),
            Some("private" | "internal" | "protected")
        )
    }

    /// Members are qualified by their owning type so `Terminal.loop` and
    /// `Reader.loop` stay distinct, and a member never collides with a top-level
    /// symbol of the same name. Top-level functions/properties (`Function`,
    /// `Variable`) keep their bare name — the tags query already only captures
    /// those two kinds at file scope.
    fn qualified_name(&self, kind: ir::NodeKind, name: &str, def: Node, src: &[u8]) -> String {
        match kind {
            ir::NodeKind::Method | ir::NodeKind::Field => match owner_name(def, src) {
                Some(owner) => format!("{owner}.{name}"),
                None => name.to_owned(),
            },
            _ => name.to_owned(),
        }
    }

    /// Kotlin fuses the package and the type in one dotted import path
    /// (`import com.example.Foo`). Kotlin does not *enforce* package == directory,
    /// but the convention holds often enough to be worth probing: map the dotted
    /// path to `com/example/Foo` and look for `<ancestor>/com/example/Foo.kt`,
    /// walking up a bounded number of ancestors from the importing file (source
    /// roots like `src/main/kotlin/` sit above the package dirs). First hit wins;
    /// anything else falls through to [`external_dep_key`]. A file whose name
    /// differs from the class it declares won't be found this way — that's fine,
    /// we resolve what we can.
    fn resolve_import(&self, spec: &str, from: &Path, _ws: &crate::Workspace) -> Option<PathBuf> {
        if spec.is_empty() {
            return None;
        }
        let rel = spec.replace('.', "/");
        let mut dir = from.parent();
        for _ in 0..8 {
            let Some(base) = dir else { break };
            for ext in ["kt", "kts"] {
                let cand = base.join(format!("{rel}.{ext}"));
                if cand.is_file() {
                    return cand.canonicalize().ok();
                }
            }
            dir = base.parent();
        }
        None
    }

    /// A Kotlin import that didn't resolve to an indexed file is a third-party or
    /// stdlib symbol (`android.util.Log`, `org.junit.Test`): its dep-key is the
    /// full dotted path, so a call through it still has a real external target.
    fn external_dep_key(&self, spec: &str) -> Option<String> {
        (!spec.is_empty()).then(|| spec.to_owned())
    }
}

/// The `visibility_modifier` keyword on a declaration (`private`/`internal`/…),
/// read off its `modifiers` child. `None` when the declaration has none.
fn visibility(def: Node, src: &[u8]) -> Option<String> {
    let mut c = def.walk();
    let mods = def.children(&mut c).find(|ch| ch.kind() == "modifiers")?;
    let mut mc = mods.walk();
    let found = mods
        .children(&mut mc)
        .find(|m| m.kind() == "visibility_modifier")
        .and_then(|m| m.utf8_text(src).ok())
        .map(str::to_owned);
    found
}

/// Name of the nearest enclosing `class_declaration`/`object_declaration`. A
/// member inside a companion object resolves to the outer class (the companion
/// itself is nameless), which matches how the member is actually addressed.
fn owner_name<'a>(def: Node, src: &'a [u8]) -> Option<&'a str> {
    let mut cur = def.parent();
    while let Some(n) = cur {
        if matches!(n.kind(), "class_declaration" | "object_declaration") {
            if let Some(name) = n.child_by_field_name("name") {
                return name.utf8_text(src).ok();
            }
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

    /// `(kind, name)` for every def, dropping the qualified form.
    fn captured(src: &str) -> Vec<(String, String)> {
        captures(src).into_iter().map(|(k, n, _)| (k, n)).collect()
    }

    /// `(kind, qualified name)` for every def — the string a `SymbolId` is hashed
    /// from, so where two symbols either stay apart or collide.
    fn qualified(src: &str) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> =
            captures(src).into_iter().map(|(k, _, q)| (k, q)).collect();
        out.sort();
        out
    }

    #[test]
    fn queries_compile() {
        let adapter = Adapter::new();
        tree_sitter::Query::new(&adapter.grammar(), adapter.tags_query()).expect("tags.scm");
    }

    #[test]
    fn imports_and_refs_queries_compile() {
        let adapter = Adapter::new();
        let lang = adapter.grammar();
        tree_sitter::Query::new(&lang, adapter.imports_query().expect("imports.scm present"))
            .expect("imports.scm");
        tree_sitter::Query::new(&lang, adapter.refs_query().expect("refs.scm present"))
            .expect("refs.scm");
    }

    /// The core vocabulary: a class, an object, a top-level function and a
    /// top-level property each land in exactly one kind.
    #[test]
    fn captured_covers_class_object_function_property() {
        let caps = captured("class Widget\nobject Registry\nfun launch() {}\nval version = 1\n");
        assert_eq!(
            caps,
            [
                ("class".to_owned(), "Registry".to_owned()),
                ("class".to_owned(), "Widget".to_owned()),
                ("function".to_owned(), "launch".to_owned()),
                ("variable".to_owned(), "version".to_owned()),
            ]
        );
    }

    /// An `enum class` and an `interface` reuse `class_declaration`; each must be
    /// captured once, as its own kind — never also as a plain class.
    #[test]
    fn interface_and_enum_are_not_also_captured_as_class() {
        let caps = captured("interface Doer\nenum class Color { RED, GREEN }\n");
        let doer: Vec<&(String, String)> = caps.iter().filter(|(_, n)| n == "Doer").collect();
        let color: Vec<&(String, String)> = caps.iter().filter(|(_, n)| n == "Color").collect();
        assert_eq!(doer, [&("interface".to_owned(), "Doer".to_owned())]);
        assert_eq!(color, [&("enum".to_owned(), "Color".to_owned())]);
    }

    /// A class with a primary constructor, a body, generics or a supertype is
    /// still a class captured once — the enum end-anchor must not exclude these.
    #[test]
    fn every_class_form_is_captured_once() {
        let caps = captured(
            "class A(val x: Int)\ndata class B(val y: Int)\nsealed class C\nclass D : C()\nclass E<T> {\n\tfun m() {}\n}\n",
        );
        let classes: Vec<String> = caps
            .iter()
            .filter(|(k, _)| k == "class")
            .map(|(_, n)| n.clone())
            .collect();
        assert_eq!(classes, ["A", "B", "C", "D", "E"]);
    }

    #[test]
    fn members_are_qualified_by_owner() {
        let qns = qualified(
            "class Terminal {\n\tfun loop() {}\n\tval size = 0\n}\nclass Reader {\n\tfun loop() {}\n}\nfun loop() {}\n",
        );
        assert_eq!(
            qns,
            [
                ("class".to_owned(), "Reader".to_owned()),
                ("class".to_owned(), "Terminal".to_owned()),
                ("field".to_owned(), "Terminal.size".to_owned()),
                ("function".to_owned(), "loop".to_owned()),
                ("method".to_owned(), "Reader.loop".to_owned()),
                ("method".to_owned(), "Terminal.loop".to_owned()),
            ]
        );
    }

    /// Locals are not symbols another file can reach: a `val`/`fun` inside a
    /// function body must not be captured.
    #[test]
    fn locals_are_not_captured() {
        let caps =
            captured("fun outer() {\n\tval local = 1\n\tfun inner() {}\n\tprintln(local)\n}\n");
        assert_eq!(caps, [("function".to_owned(), "outer".to_owned())]);
    }

    #[test]
    fn private_is_not_exported() {
        let src = "fun open() {}\nprivate fun closed() {}\ninternal fun pkg() {}\nprotected fun sub() {}\n";
        let tree = parse(src);
        let adapter = Adapter::new();
        let bytes = src.as_bytes();
        let flags: Vec<(String, bool)> = find(&tree, "function_declaration")
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
                ("open".to_owned(), true),
                ("closed".to_owned(), false),
                ("pkg".to_owned(), false),
                ("sub".to_owned(), false),
            ]
        );
    }

    #[test]
    fn is_test_path_follows_gradle_layout() {
        let adapter = Adapter::new();
        assert!(adapter.is_test_path("app/src/test/kotlin/FooTest.kt"));
        assert!(adapter.is_test_path("app/src/androidTest/kotlin/BarTest.kt"));
        assert!(!adapter.is_test_path("app/src/main/kotlin/Foo.kt"));
    }

    /// The import query's captures, keyed by capture name — mirrors what
    /// `parse::extract_imports` reads to build an `ImportRec`. A named import
    /// needs `import.name`; without it the record builder emits nothing.
    fn imports(src: &str) -> Vec<(String, String)> {
        let adapter = Adapter::new();
        let lang = adapter.grammar();
        let query =
            tree_sitter::Query::new(&lang, adapter.imports_query().unwrap()).expect("imports.scm");
        let tree = parse(src);
        let bytes = src.as_bytes();
        let names = query.capture_names();
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), bytes);
        let mut out = Vec::new();
        while let Some(m) = streaming_iterator::StreamingIterator::next(&mut matches) {
            for cap in m.captures {
                let cap_name = names[cap.index as usize].to_owned();
                let text = cap.node.utf8_text(bytes).unwrap_or("").to_owned();
                out.push((cap_name, text));
            }
        }
        out
    }

    #[test]
    fn import_captures_source_name_and_alias() {
        let caps = imports(
            "package p\nimport com.example.Foo\nimport com.example.Bar as Baz\nimport com.example.*\n",
        );
        // full dotted path is the specifier; last segment is the named symbol
        assert!(caps.contains(&("import.source".to_owned(), "com.example.Foo".to_owned())));
        assert!(caps.contains(&("import.name".to_owned(), "Foo".to_owned())));
        // `as` alias is captured separately, becomes the local binding
        assert!(caps.contains(&("import.source".to_owned(), "com.example.Bar".to_owned())));
        assert!(caps.contains(&("import.name".to_owned(), "Bar".to_owned())));
        assert!(caps.contains(&("import.alias".to_owned(), "Baz".to_owned())));
        // star import has no trailing `*` node; its last segment is the package tail
        assert!(caps.contains(&("import.source".to_owned(), "com.example".to_owned())));
    }

    /// A throwaway directory tree; files are created by relative path.
    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(files: &[&str]) -> Fixture {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let id = N.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!("ripple-kt-{}-{id}", std::process::id()));
            for f in files {
                let p = root.join(f);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(&p, "").unwrap();
            }
            Fixture { root }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn resolve_import_maps_dotted_path_to_file_up_the_source_root() {
        let fx = Fixture::new(&[
            "app/src/main/kotlin/com/example/Foo.kt",
            "app/src/main/kotlin/com/example/Client.kt",
        ]);
        let adapter = Adapter::new();
        let from = fx.root.join("app/src/main/kotlin/com/example/Client.kt");
        let got = adapter.resolve_import("com.example.Foo", &from, &crate::Workspace::default());
        let want = fx
            .root
            .join("app/src/main/kotlin/com/example/Foo.kt")
            .canonicalize()
            .ok();
        assert_eq!(got, want);
    }

    #[test]
    fn resolve_import_returns_none_for_unknown_and_external() {
        let fx = Fixture::new(&["src/main/kotlin/com/example/Client.kt"]);
        let adapter = Adapter::new();
        let from = fx.root.join("src/main/kotlin/com/example/Client.kt");
        let ws = crate::Workspace::default();
        assert_eq!(adapter.resolve_import("android.util.Log", &from, &ws), None);
        // unresolved specifiers mint an external dep node instead
        assert_eq!(
            adapter.external_dep_key("android.util.Log"),
            Some("android.util.Log".to_owned())
        );
        assert_eq!(adapter.external_dep_key(""), None);
    }
}
