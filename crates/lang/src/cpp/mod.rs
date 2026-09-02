//! C++ adapter — Tier 0 defs plus Tier 1 includes and Tier 2 call edges.
//!
//! tree-sitter-cpp is a superset of the C grammar, so a free function and a
//! member function share the `function_definition` node and differ only in the
//! inner declarator: a bare `identifier` is a free function, a `field_identifier`
//! is an inline member, and a `qualified_identifier` (`Foo::bar`) is an
//! out-of-line member. `qualified_name` reads the owning class off that shape so
//! `Foo::bar` and `Bar::bar` stay distinct symbols. There is no member-type
//! resolution (`bindings_query`); member calls are left to receiver machinery.

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
        "cpp"
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_cpp::LANGUAGE.into()
    }

    fn file_globs(&self) -> &'static [&'static str] {
        &["*.cpp", "*.cc", "*.cxx", "*.hpp", "*.hh", "*.hxx"]
    }

    fn is_test_path(&self, rel: &str) -> bool {
        rel.split('/').any(|seg| seg == "test" || seg == "tests")
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

    /// A quoted `#include "foo.h"` names a local header, relative to the
    /// including file's directory or a project include root above it — probe the
    /// file's own directory first, then walk up a bounded number of ancestors,
    /// trying both `<ancestor>/foo.h` and the very common `<ancestor>/include/foo.h`
    /// layout. A system `<vector>` include (specifier starts with `<`) is never
    /// local: return `None` so it binds as an external dependency.
    fn resolve_import(&self, spec: &str, from: &Path, _ws: &Workspace) -> Option<PathBuf> {
        if spec.starts_with('<') {
            return None;
        }
        let mut dir = from.parent();
        for _ in 0..8 {
            let base = dir?;
            for cand in [base.join(spec), base.join("include").join(spec)] {
                if cand.is_file() {
                    return cand.canonicalize().ok();
                }
            }
            dir = base.parent();
        }
        None
    }

    /// The dep-key of a header that didn't resolve locally: the header path with
    /// the system-include angle brackets stripped (`<vector>` → `vector`), so a
    /// standard-library or third-party header mints an external node + `Imports`
    /// edge.
    fn external_dep_key(&self, spec: &str) -> Option<String> {
        let s = spec.trim_start_matches('<').trim_end_matches('>');
        (!s.is_empty()).then(|| s.to_owned())
    }

    /// C++ has no file-scope access control, so we keep this pragmatic: a
    /// namespace-scope function or type is externally visible unless declared
    /// `static` (internal linkage). Class members are treated the same way —
    /// access-specifier tracking (`private:`/`public:`) is too fiddly for a tags
    /// query, so a member defaults to visible and only `static` flips it off.
    fn is_exported(&self, def: Node, src: &[u8]) -> bool {
        !has_static(def, src)
    }

    /// Qualify a member by its owning type so `Foo::bar` and `Bar::bar` stay
    /// distinct, and so a field `count` and a free function `count` don't hash to
    /// the same `SymbolId`. An out-of-line definition names its owner inline
    /// (`void Foo::bar(){}`) via a `qualified_identifier`; an inline member takes
    /// the enclosing `class_specifier`/`struct_specifier`.
    fn qualified_name(&self, kind: ir::NodeKind, name: &str, def: Node, src: &[u8]) -> String {
        let owner = match kind {
            ir::NodeKind::Method => scope_owner(def, src).or_else(|| enclosing_type(def, src)),
            ir::NodeKind::Field => enclosing_type(def, src),
            _ => return name.to_owned(),
        };
        match owner {
            Some(ty) => format!("{ty}.{name}"),
            None => name.to_owned(),
        }
    }
}

/// Does `def` carry a `static` storage-class specifier as a direct child?
fn has_static(def: Node, src: &[u8]) -> bool {
    let mut c = def.walk();
    let is_static = def.children(&mut c).any(|child| {
        child.kind() == "storage_class_specifier"
            && child.utf8_text(src).map(str::trim) == Ok("static")
    });
    is_static
}

/// Owner named inline by an out-of-line member definition: the `scope` of the
/// `qualified_identifier` in the declarator (`void Foo::bar(){}` → `Foo`,
/// `A::B::bar` → `B`). `None` when the def has no `::`-qualified declarator.
fn scope_owner<'a>(def: Node, src: &'a [u8]) -> Option<&'a str> {
    let qid = find_descendant(def, "qualified_identifier")?;
    let scope = qid.child_by_field_name("scope")?;
    Some(last_segment(scope.utf8_text(src).ok()?))
}

/// Name of the `class_specifier`/`struct_specifier` a member sits inside.
fn enclosing_type<'a>(def: Node, src: &'a [u8]) -> Option<&'a str> {
    let mut cur = def.parent()?;
    loop {
        if matches!(cur.kind(), "class_specifier" | "struct_specifier") {
            let name = cur.child_by_field_name("name")?;
            return name.utf8_text(src).ok();
        }
        cur = cur.parent()?;
    }
}

/// First descendant of `node` (pre-order, excluding `node` itself) with `kind`.
fn find_descendant<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut c = node.walk();
    for child in node.children(&mut c) {
        if child.kind() == kind {
            return Some(child);
        }
        if let Some(found) = find_descendant(child, kind) {
            return Some(found);
        }
    }
    None
}

/// Last `::` segment of a scope name: `A::B` → `B`, `Foo` → `Foo`.
fn last_segment(text: &str) -> &str {
    text.rsplit("::").next().unwrap_or(text)
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

    /// Every def capture as `(kind, name, qualified name)`, mirroring
    /// `parse::extract_defs`.
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

    fn captured(src: &str) -> Vec<(String, String)> {
        captures(src).into_iter().map(|(k, n, _)| (k, n)).collect()
    }

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
        tree_sitter::Query::new(&lang, adapter.imports_query().expect("imports")).expect("imports");
        tree_sitter::Query::new(&lang, adapter.refs_query().expect("refs")).expect("refs");
        assert!(adapter.bindings_query().is_none());
    }

    /// The core shapes: class, struct, enum, free fn, member fn, field, typedef,
    /// using-alias.
    #[test]
    fn captured_covers_every_def_form() {
        let caps = captured(
            "class Foo {\n int count;\n public:\n void bar();\n int baz() { return count; }\n};\nstruct Point { int x; };\nenum Color { Red };\nint add(int a, int b) { return a + b; }\ntypedef int myint;\nusing Alias = int;\n",
        );
        assert_eq!(
            caps,
            [
                ("class".to_owned(), "Foo".to_owned()),
                ("class".to_owned(), "Point".to_owned()),
                ("enum".to_owned(), "Color".to_owned()),
                ("field".to_owned(), "count".to_owned()),
                ("field".to_owned(), "x".to_owned()),
                ("function".to_owned(), "add".to_owned()),
                ("method".to_owned(), "bar".to_owned()),
                ("method".to_owned(), "baz".to_owned()),
                ("type".to_owned(), "Alias".to_owned()),
                ("type".to_owned(), "myint".to_owned()),
            ]
        );
    }

    /// A struct/class must be captured once, not also by some type alternation.
    #[test]
    fn a_struct_is_captured_once_as_a_class() {
        let caps = captured("struct Item { int n; };\n");
        let named: Vec<&(String, String)> = caps.iter().filter(|(_, n)| n == "Item").collect();
        assert_eq!(named, [&("class".to_owned(), "Item".to_owned())]);
    }

    #[test]
    fn members_are_qualified_by_owner() {
        let qns = qualified(
            "class Foo {\n int count;\n public:\n int baz() { return count; }\n};\nstruct Bar { int count; int baz() { return count; } };\n",
        );
        let members: Vec<&(String, String)> = qns
            .iter()
            .filter(|(k, _)| k == "field" || k == "method")
            .collect();
        assert_eq!(
            members,
            [
                &("field".to_owned(), "Bar.count".to_owned()),
                &("field".to_owned(), "Foo.count".to_owned()),
                &("method".to_owned(), "Bar.baz".to_owned()),
                &("method".to_owned(), "Foo.baz".to_owned()),
            ]
        );
    }

    /// `void Foo::bar(){}` defined outside the class body still qualifies to
    /// `Foo.bar` off the `::` scope in its declarator.
    #[test]
    fn out_of_line_member_qualified() {
        let qns = qualified("void Foo::bar() { }\nint Foo::baz() { return 0; }\n");
        let methods: Vec<&(String, String)> = qns.iter().filter(|(k, _)| k == "method").collect();
        assert_eq!(
            methods,
            [
                &("method".to_owned(), "Foo.bar".to_owned()),
                &("method".to_owned(), "Foo.baz".to_owned()),
            ]
        );
    }

    /// A member `count` and a free function `count` must not collide.
    #[test]
    fn a_field_does_not_collide_with_a_free_function() {
        let qns = qualified("int count() { return 0; }\nstruct Item { int count; };\n");
        assert_eq!(
            qns,
            [
                ("class".to_owned(), "Item".to_owned()),
                ("field".to_owned(), "Item.count".to_owned()),
                ("function".to_owned(), "count".to_owned()),
            ]
        );
    }

    /// Only file-scope variables — a local inside a function body is not a symbol
    /// anything else can reach.
    #[test]
    fn locals_are_not_captured_as_variables() {
        let caps = captured(
            "int Top = 1;\nstatic int S = 2;\nvoid f() {\n int inner = 3;\n (void)inner;\n}\n",
        );
        let vars: Vec<&(String, String)> = caps.iter().filter(|(k, _)| k == "variable").collect();
        assert_eq!(
            vars,
            [
                &("variable".to_owned(), "S".to_owned()),
                &("variable".to_owned(), "Top".to_owned()),
            ]
        );
    }

    /// `static` gives internal linkage → not externally visible; a plain
    /// namespace-scope function or var is.
    #[test]
    fn static_is_not_exported() {
        let src = "int pub_fn() { return 0; }\nstatic int priv_fn() { return 0; }\n";
        let tree = parse(src);
        let adapter = Adapter::new();
        let bytes = src.as_bytes();
        let flags: Vec<bool> = find(&tree, "function_definition")
            .into_iter()
            .map(|f| adapter.is_exported(f, bytes))
            .collect();
        assert_eq!(flags, [true, false]);
    }

    #[test]
    fn enum_class_is_captured() {
        let caps = captured("enum class Mode { On, Off };\n");
        assert_eq!(caps, [("enum".to_owned(), "Mode".to_owned())]);
    }

    #[test]
    fn is_test_path_matches_test_dirs() {
        let adapter = Adapter::new();
        assert!(adapter.is_test_path("src/test/foo.cpp"));
        assert!(adapter.is_test_path("tests/bar.cc"));
        assert!(!adapter.is_test_path("src/foo.cpp"));
    }

    /// A quoted include resolves to a header next to the including file; a system
    /// `<...>` include does not, and binds as an external dep instead.
    #[test]
    fn quoted_include_resolves_locally_and_system_include_is_external() {
        let dir =
            std::env::temp_dir().join(format!("ripple-cpp-ri-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/app.cc"), "").unwrap();
        std::fs::write(dir.join("src/app.h"), "").unwrap();
        let from = dir.join("src/app.cc");
        let adapter = Adapter::new();
        let ws = Workspace::default();

        assert_eq!(
            adapter.resolve_import("app.h", &from, &ws),
            dir.join("src/app.h").canonicalize().ok()
        );
        assert_eq!(adapter.resolve_import("<vector>", &from, &ws), None);
        assert_eq!(
            adapter.external_dep_key("<vector>"),
            Some("vector".to_owned())
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
