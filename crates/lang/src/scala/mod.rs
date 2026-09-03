//! Scala adapter — Tier 0 defs plus Tier 1/2 imports and call edges.
//!
//! Scala shares one node kind, `function_definition`, between free functions and
//! methods; the tags query splits them by whether the def sits in a
//! `template_body`/`enum_body` (a method) or the `compilation_unit` (a free
//! function). `qualified_name` then prefixes members with their owning
//! class/trait/object so same-named methods stay distinct and members don't
//! collide with a top-level def — identity is `(module, qualified_name)`, and the
//! node kind is not part of the hash.
//!
//! Imports resolve through the `import_declaration` text: the query captures the
//! whole declaration (`import a.b.C`) as the specifier because no single node
//! holds the dotted path. `resolve_import` strips the `import` keyword and maps a
//! plain `a.b.C` to `a/b/C.scala`; selector/wildcard forms carry braces or `_`
//! and are left to `external_dep_key`, which mints an external node keyed by the
//! package prefix.

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
        "scala"
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_scala::LANGUAGE.into()
    }

    fn file_globs(&self) -> &'static [&'static str] {
        &["*.scala", "*.sc"]
    }

    fn is_test_path(&self, rel: &str) -> bool {
        rel.contains("src/test/")
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

    /// Map a plain `import a.b.C` to the file `a/b/C.scala`. The specifier is the
    /// whole declaration text, so the `import` keyword is stripped first. Only
    /// plain dotted paths resolve here — selector (`{…}`) and wildcard (`_`/`*`)
    /// forms carry punctuation and are left to [`external_dep_key`].
    ///
    /// Scala does not enforce package==directory and Mill does not even put the
    /// package in the path — os-lib's `package os` lives at `os/src/`, so no ancestor
    /// of an importing file ever spells `.../os/Path.scala`. So the roots come from
    /// the build ([`crate::java::source_roots::resolve`]: sbt's `src/main/scala`,
    /// Mill's `<module>/src`, `<module>/src-jvm`, `<module>/test/src`), and a hit
    /// under a package-less Mill root is confirmed against the candidate file's own
    /// `package` declaration before it is believed. A miss falls through to an
    /// external node — no attempt is made to be exhaustive.
    ///
    /// [`external_dep_key`]: LanguageAdapter::external_dep_key
    fn resolve_import(&self, spec: &str, from: &Path, _ws: &Workspace) -> Option<PathBuf> {
        let path = strip_import_kw(spec);
        let segments: Vec<&str> = path.split('.').collect();
        // reject anything that isn't a plain dotted path of bare identifiers
        if segments.is_empty()
            || segments
                .iter()
                .any(|s| s.is_empty() || s.contains(['{', '}', ' ', '*', '_']) || s.contains('/'))
        {
            return None;
        }
        crate::java::source_roots::resolve(path, from, &["scala", "sc"])
    }

    /// The dep-key of a Scala import is its normalized dotted path: the `import`
    /// keyword stripped, and any trailing selector list or wildcard dropped so the
    /// key is the package prefix (`import a.b.{Map, Set}` → `a.b`). Third-party
    /// and stdlib imports (`scala.*`, `java.*`) that don't resolve to a local file
    /// land here and mint an external node.
    fn external_dep_key(&self, spec: &str) -> Option<String> {
        let path = strip_import_kw(spec);
        // drop a trailing selector list, then any trailing wildcard/dot debris
        let head = path.split('{').next().unwrap_or(path);
        let key = head.trim().trim_end_matches(['*', '_', '.', ' ']);
        (!key.is_empty()).then(|| key.to_owned())
    }

    /// Scala is public by default: a def is unexported only when its `modifiers`
    /// carry a `private`/`protected` `access_modifier`. Everything else — no
    /// modifiers, or modifiers like `final`/`override` — stays public.
    fn is_exported(&self, def: Node, src: &[u8]) -> bool {
        !has_access_modifier(def, src)
    }

    /// Members are qualified by the enclosing `class`/`trait`/`object`/`enum` so
    /// `Terminal.loop` and `Reader.loop` stay distinct, and a member `count`
    /// doesn't hash to the same symbol as a top-level `count`. Free functions and
    /// module-level values have no owner and keep their bare name.
    fn qualified_name(&self, kind: ir::NodeKind, name: &str, def: Node, src: &[u8]) -> String {
        match kind {
            ir::NodeKind::Method | ir::NodeKind::Variable => match enclosing_owner(def, src) {
                Some(owner) => format!("{owner}.{name}"),
                None => name.to_owned(),
            },
            _ => name.to_owned(),
        }
    }
}

/// Strip the leading `import` keyword off a captured `import_declaration`'s text,
/// leaving the dotted path (`import a.b.C` → `a.b.C`). The specifier is the whole
/// declaration because no grammar node spans just the path.
fn strip_import_kw(spec: &str) -> &str {
    let s = spec.trim();
    s.strip_prefix("import").map_or(s, str::trim)
}

/// Name of the nearest enclosing `class`/`trait`/`object`/`enum` definition, or
/// `None` when the def is at module scope. A member's parent chain is
/// `def → template_body/enum_body → *_definition`.
fn enclosing_owner<'a>(def: Node, src: &'a [u8]) -> Option<&'a str> {
    let mut cur = def.parent();
    while let Some(node) = cur {
        if matches!(
            node.kind(),
            "class_definition" | "trait_definition" | "object_definition" | "enum_definition"
        ) {
            let name = node.child_by_field_name("name")?;
            return name.utf8_text(src).ok();
        }
        cur = node.parent();
    }
    None
}

/// Does this def carry a `private`/`protected` access modifier? The `modifiers`
/// node is a direct child of the definition and holds an `access_modifier` when
/// visibility is restricted.
fn has_access_modifier(def: Node, _src: &[u8]) -> bool {
    let mut c = def.walk();
    let modifiers: Vec<Node> = def
        .children(&mut c)
        .filter(|child| child.kind() == "modifiers")
        .collect();
    modifiers.into_iter().any(|m| {
        let mut cc = m.walk();
        let restricted = m.children(&mut cc).any(|n| n.kind() == "access_modifier");
        restricted
    })
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

    /// Captures as `(kind, name)`.
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

    #[test]
    fn captured_covers_the_core_forms() {
        let caps = captured(
            "package p\nclass Terminal { def loop(): Unit = () }\ntrait Reader { def read(): String }\nobject Registry { val instances = 0 }\nenum Color { case Red }\ncase class Point(x: Int, y: Int)\ntype Offset = Int\nval topLevel = 1\ndef freeFn(): Unit = ()\n",
        );
        assert_eq!(
            caps,
            [
                ("class".to_owned(), "Point".to_owned()),
                ("class".to_owned(), "Registry".to_owned()),
                ("class".to_owned(), "Terminal".to_owned()),
                ("enum".to_owned(), "Color".to_owned()),
                ("function".to_owned(), "freeFn".to_owned()),
                ("interface".to_owned(), "Reader".to_owned()),
                ("method".to_owned(), "loop".to_owned()),
                ("method".to_owned(), "read".to_owned()),
                ("type".to_owned(), "Offset".to_owned()),
                ("variable".to_owned(), "instances".to_owned()),
                ("variable".to_owned(), "topLevel".to_owned()),
            ]
        );
    }

    /// A method on `Terminal` and a method on `Reader` with the same name stay
    /// distinct, and a member value does not collide with a top-level function of
    /// the same name.
    #[test]
    fn members_are_qualified_by_owner() {
        let qns = qualified(
            "package p\nclass Terminal { def loop(): Unit = ()\n  val state = 0 }\ntrait Reader { def loop(): Unit }\nobject Box { val state = 1 }\ndef loop(): Unit = ()\nval state = 2\n",
        );
        assert_eq!(
            qns,
            [
                ("class".to_owned(), "Box".to_owned()),
                ("class".to_owned(), "Terminal".to_owned()),
                ("function".to_owned(), "loop".to_owned()),
                ("interface".to_owned(), "Reader".to_owned()),
                ("method".to_owned(), "Reader.loop".to_owned()),
                ("method".to_owned(), "Terminal.loop".to_owned()),
                ("variable".to_owned(), "Box.state".to_owned()),
                ("variable".to_owned(), "Terminal.state".to_owned()),
                ("variable".to_owned(), "state".to_owned()),
            ]
        );
    }

    #[test]
    fn private_is_not_exported() {
        let src = "package p\nclass C {\n  def open(): Int = 1\n  private def secret(): Int = 2\n  protected def guarded(): Int = 3\n}\n";
        let tree = parse(src);
        let adapter = Adapter::new();
        let bytes = src.as_bytes();
        let flags: Vec<(String, bool)> = find(&tree, "function_definition")
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
                ("secret".to_owned(), false),
                ("guarded".to_owned(), false),
            ]
        );
    }

    /// Locals inside a method body are not addressable from elsewhere, so a `val`
    /// under a `block` must not be captured.
    #[test]
    fn locals_are_not_captured_as_variables() {
        let caps = captured(
            "package p\nval Top = 1\nobject O {\n  def f(): Int = {\n    val inner = 2\n    inner\n  }\n}\n",
        );
        let vars: Vec<&(String, String)> = caps.iter().filter(|(k, _)| k == "variable").collect();
        assert_eq!(vars, [&("variable".to_owned(), "Top".to_owned())]);
    }

    #[test]
    fn external_dep_key_strips_keyword_and_selectors() {
        let adapter = Adapter::new();
        assert_eq!(
            adapter.external_dep_key("import scala.collection.mutable"),
            Some("scala.collection.mutable".to_owned())
        );
        assert_eq!(
            adapter.external_dep_key("import a.b.{Map, Set}"),
            Some("a.b".to_owned())
        );
        assert_eq!(
            adapter.external_dep_key("import a.b._"),
            Some("a.b".to_owned())
        );
        assert_eq!(
            adapter.external_dep_key("import a.b.C.*"),
            Some("a.b.C".to_owned())
        );
        assert_eq!(adapter.external_dep_key("import"), None);
    }

    /// `import a.b.C` resolves to `a/b/C.scala` walking up from the importing file.
    #[test]
    fn resolve_import_maps_dotted_path_to_file() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let id = N.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("ripple-scala-ri-{}-{id}", std::process::id()));
        let target = root.join("a/b/C.scala");
        let importer = root.join("x/Main.scala");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::create_dir_all(importer.parent().unwrap()).unwrap();
        std::fs::write(&target, "package a.b\nclass C\n").unwrap();
        std::fs::write(&importer, "import a.b.C\n").unwrap();

        let adapter = Adapter::new();
        let ws = Workspace::default();
        let got = adapter.resolve_import("import a.b.C", &importer, &ws);
        assert_eq!(got, target.canonicalize().ok());

        // selector and wildcard forms do not resolve to a file
        assert_eq!(
            adapter.resolve_import("import a.b.{C}", &importer, &ws),
            None
        );
        assert_eq!(adapter.resolve_import("import a.b._", &importer, &ws), None);
        // a third-party path with no matching file falls through
        assert_eq!(
            adapter.resolve_import("import scala.collection.mutable", &importer, &ws),
            None
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A throwaway tree of `(relative path, contents)`, cleaned up on drop. The
    /// contents matter here: a Mill root does not encode the package in the path, so
    /// resolution is confirmed against the candidate file's `package` declaration.
    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(files: &[(&str, &str)]) -> Fixture {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let id = N.fetch_add(1, Ordering::Relaxed);
            let root =
                std::env::temp_dir().join(format!("ripple-scala-fx-{}-{id}", std::process::id()));
            for (rel, body) in files {
                let p = root.join(rel);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(&p, body).unwrap();
            }
            Fixture { root }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// The os-lib case. Mill puts module `os`'s sources at `os/src/`, so `package os`
    /// is nowhere in the path and `import os.Path` names `os/src/Path.scala`. The
    /// leading FQN segments are stripped to find it and the hit is confirmed against
    /// the file's own `package` line — which is what keeps `scala.util.Try` from
    /// binding to the unrelated `Try.scala` sitting in the same root.
    #[test]
    fn resolve_import_finds_mill_module_source_root() {
        let fx = Fixture::new(&[
            ("build.sc", "import mill._\nobject os extends ScalaModule\n"),
            ("os/src/Path.scala", "package os\n\nclass Path\n"),
            ("os/src/Try.scala", "package os\n\nclass Try\n"),
            ("os/src/FileOps.scala", "package os\n\nimport os.Path\n"),
        ]);
        let adapter = Adapter::new();
        let ws = Workspace::default();
        let from = fx.root.join("os/src/FileOps.scala");
        assert_eq!(
            adapter.resolve_import("import os.Path", &from, &ws),
            fx.root.join("os/src/Path.scala").canonicalize().ok()
        );
        // `os/src/Try.scala` exists but declares `package os`, not `scala.util`
        assert_eq!(
            adapter.resolve_import("import scala.util.Try", &from, &ws),
            None
        );
        assert_eq!(
            adapter.resolve_import("import java.io.File", &from, &ws),
            None
        );
    }

    /// Mill's test sources hang off `<module>/test/src`, and os-lib's live in
    /// `package test.os` — two leading segments to strip against a two-deep root.
    #[test]
    fn resolve_import_finds_mill_test_source_root() {
        let fx = Fixture::new(&[
            ("build.mill", "package build\n"),
            ("os/src/Path.scala", "package os\n\nclass Path\n"),
            (
                "os/test/src/TestUtil.scala",
                "package test.os\n\nobject TestUtil { def prep = () }\n",
            ),
            (
                "os/test/src/PathTests.scala",
                "package test.os\n\nimport test.os.TestUtil.prep\n",
            ),
        ]);
        let adapter = Adapter::new();
        let from = fx.root.join("os/test/src/PathTests.scala");
        assert_eq!(
            adapter.resolve_import("import test.os.TestUtil.prep", &from, &Workspace::default()),
            fx.root
                .join("os/test/src/TestUtil.scala")
                .canonicalize()
                .ok()
        );
    }

    /// sbt's `src/main/scala` convention, discovered from `build.sbt`, across two
    /// subprojects so no ancestor of the importer covers the target.
    #[test]
    fn resolve_import_uses_sbt_source_roots_across_subprojects() {
        let fx = Fixture::new(&[
            (
                "build.sbt",
                "lazy val core = project\nlazy val app = project\n",
            ),
            (
                "core/src/main/scala/com/example/Util.scala",
                "package com.example\n\nobject Util\n",
            ),
            (
                "app/src/main/scala/com/other/App.scala",
                "package com.other\n\nimport com.example.Util\n",
            ),
        ]);
        let adapter = Adapter::new();
        let from = fx.root.join("app/src/main/scala/com/other/App.scala");
        assert_eq!(
            adapter.resolve_import("import com.example.Util", &from, &Workspace::default()),
            fx.root
                .join("core/src/main/scala/com/example/Util.scala")
                .canonicalize()
                .ok()
        );
    }

    #[test]
    fn is_test_path_matches_scala_test_layout() {
        let adapter = Adapter::new();
        assert!(adapter.is_test_path("src/test/scala/com/example/FooSpec.scala"));
        assert!(!adapter.is_test_path("src/main/scala/com/example/Foo.scala"));
    }
}
