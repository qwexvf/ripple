//! C# adapter — Tier 0 defs plus Tier 1/2 imports and member-call edges.
//!
//! Type members (methods, properties, fields) are qualified by their enclosing
//! type declaration, so `Widget.Name` and `Order.Name` stay distinct symbols and
//! a field never collides with a same-named top-level function. Exports follow
//! the `public` modifier — the C# notion of externally visible.

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
        "csharp"
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_c_sharp::LANGUAGE.into()
    }

    fn file_globs(&self) -> &'static [&'static str] {
        &["*.cs"]
    }

    fn is_test_path(&self, rel: &str) -> bool {
        rel.ends_with("Test.cs")
            || rel.ends_with("Tests.cs")
            || rel
                .split(['/', '\\'])
                .any(|seg| seg == "test" || seg == "tests")
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

    fn bindings_query(&self) -> Option<&'static str> {
        Some(include_str!("queries/bindings.scm"))
    }

    /// `Helper()` inside an instance method is `this.Helper()` (#120).
    fn bare_call_in_method_is_self_call(&self) -> bool {
        true
    }

    /// A definition is exported when its modifiers include `public`. Other
    /// accessibility levels (`protected`, `internal`, `private`) are not
    /// externally visible, so they don't count as exported.
    fn is_exported(&self, def: Node, src: &[u8]) -> bool {
        has_public_modifier(def, src)
    }

    /// Type members are qualified by the type they're declared in, so
    /// `Widget.GetCount` and `Order.GetCount` are distinct and a field `Name`
    /// doesn't hash to the same `SymbolId` as a top-level `Name`.
    fn qualified_name(&self, kind: ir::NodeKind, name: &str, def: Node, src: &[u8]) -> String {
        match kind {
            ir::NodeKind::Method | ir::NodeKind::Field => match owner_type(def, src) {
                Some(ty) => format!("{ty}.{name}"),
                None => name.to_owned(),
            },
            _ => name.to_owned(),
        }
    }

    /// A C# `using` imports a *namespace*, and a namespace spans many files —
    /// `using System.Text;` names no single file on disk. So there is nothing to
    /// resolve locally; the import is always bound as an external namespace node
    /// (see [`Self::external_dep_key`]). Returning `None` routes every `using`
    /// through that external-binding pass.
    fn resolve_import(
        &self,
        _spec: &str,
        _from: &std::path::Path,
        _ws: &crate::Workspace,
    ) -> Option<std::path::PathBuf> {
        None
    }

    /// The dep-key of a `using` is the namespace itself — `System.Text` keys a
    /// `System.Text` external node. This mints a namespace node plus an `Imports`
    /// edge for every non-empty `using`. Empty specifiers (which cannot happen
    /// from the query, but stay honest) normalize to `None`.
    fn external_dep_key(&self, spec: &str) -> Option<String> {
        (!spec.is_empty()).then(|| spec.to_owned())
    }
}

/// Whether any direct-child `modifier` of `def` is `public`.
fn has_public_modifier(def: Node, src: &[u8]) -> bool {
    let mut c = def.walk();
    let found = def
        .children(&mut c)
        .filter(|n| n.kind() == "modifier")
        .any(|n| n.utf8_text(src).is_ok_and(|t| t == "public"));
    found
}

/// Name of the type declaration a member sits inside — the nearest enclosing
/// class/struct/record/interface/enum, walking up from the member's def node.
fn owner_type<'a>(def: Node, src: &'a [u8]) -> Option<&'a str> {
    let mut cur = def.parent();
    while let Some(node) = cur {
        if is_type_decl(node.kind()) {
            let name = node.child_by_field_name("name")?;
            return name.utf8_text(src).ok();
        }
        cur = node.parent();
    }
    None
}

fn is_type_decl(kind: &str) -> bool {
    matches!(
        kind,
        "class_declaration"
            | "struct_declaration"
            | "record_declaration"
            | "interface_declaration"
            | "enum_declaration"
    )
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
        tree_sitter::Query::new(
            &lang,
            adapter.bindings_query().expect("bindings.scm present"),
        )
        .expect("bindings.scm");
    }

    /// Every `bindings.scm` match as `(name, type)`, mirroring what
    /// `parse::extract_bindings` does with the same query.
    fn bindings(src: &str) -> Vec<(String, String)> {
        let adapter = Adapter::new();
        let lang = adapter.grammar();
        let query = tree_sitter::Query::new(
            &lang,
            adapter.bindings_query().expect("bindings.scm present"),
        )
        .expect("bindings.scm");
        let tree = parse(src);
        let bytes = src.as_bytes();
        let names = query.capture_names();
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), bytes);
        let mut out = Vec::new();
        while let Some(m) = streaming_iterator::StreamingIterator::next(&mut matches) {
            let mut name = None;
            let mut ty = None;
            for cap in m.captures {
                let text = cap.node.utf8_text(bytes).unwrap_or("").to_owned();
                match names[cap.index as usize] {
                    "bind.name" => name = Some(text),
                    "bind.ctor" | "bind.type" => ty = Some(text),
                    _ => {}
                }
            }
            if let Some(name) = name {
                out.push((name, ty.unwrap_or_default()));
            }
        }
        out.sort();
        out
    }

    fn bound(name: &str, ty: &str) -> (String, String) {
        (name.to_owned(), ty.to_owned())
    }

    /// A field, a local, a parameter and `var x = new Foo()` — one
    /// `variable_declaration` node serves the field and the local, so both come
    /// out of the same pattern.
    #[test]
    fn bindings_capture_every_written_down_type() {
        let caps = bindings(
            "class App {\n private Foo field;\n void M(Bar param) {\n  Baz local = Mk();\n  var made = new Qux();\n }\n}\n",
        );
        assert_eq!(
            caps,
            [
                bound("field", "Foo"),
                bound("local", "Baz"),
                bound("made", "Qux"),
                bound("param", "Bar"),
            ]
        );
    }

    /// `var` is its own node kind here, so an inferred local with no `new` writes
    /// no type at all and must produce no binding.
    #[test]
    fn an_inferred_local_without_a_constructor_binds_nothing() {
        assert_eq!(bindings("class A { void M() { var x = Mk(); } }\n"), []);
    }

    /// A generic, qualified or predefined type is not a plain `identifier`, so it
    /// is left to the by-name fallback rather than guessed at.
    #[test]
    fn generic_qualified_and_predefined_types_are_not_bound() {
        assert_eq!(
            bindings(
                "class A {\n private List<Foo> xs;\n void M(int n) {\n  System.Text.Foo q = null;\n }\n}\n"
            ),
            []
        );
    }

    #[test]
    fn captured_covers_every_declaration_form() {
        let caps = captured(
            "namespace N {\n\
             public class Widget { public string Name { get; set; } public int GetCount() { return 0; } }\n\
             public interface IThing { void DoIt(); }\n\
             public struct Point { public int X; }\n\
             public enum Color { Red, Green }\n\
             }\n",
        );
        assert_eq!(
            caps,
            [
                ("class".to_owned(), "Point".to_owned()),
                ("class".to_owned(), "Widget".to_owned()),
                ("enum".to_owned(), "Color".to_owned()),
                ("field".to_owned(), "Name".to_owned()),
                ("field".to_owned(), "X".to_owned()),
                ("interface".to_owned(), "IThing".to_owned()),
                ("method".to_owned(), "DoIt".to_owned()),
                ("method".to_owned(), "GetCount".to_owned()),
            ]
        );
    }

    #[test]
    fn a_record_is_captured_once_as_a_class() {
        let caps = captured("namespace N { public record Person(string First); }\n");
        let named: Vec<&(String, String)> = caps.iter().filter(|(_, n)| n == "Person").collect();
        assert_eq!(named, [&("class".to_owned(), "Person".to_owned())]);
    }

    #[test]
    fn members_are_qualified_by_type() {
        let qns = qualified(
            "namespace N {\n\
             public class Widget { public string Name { get; set; } private int count; public int Get() { return 0; } }\n\
             public class Order { public string Name { get; set; } }\n\
             }\n",
        );
        assert_eq!(
            qns,
            [
                ("class".to_owned(), "Order".to_owned()),
                ("class".to_owned(), "Widget".to_owned()),
                ("field".to_owned(), "Order.Name".to_owned()),
                ("field".to_owned(), "Widget.Name".to_owned()),
                ("field".to_owned(), "Widget.count".to_owned()),
                ("method".to_owned(), "Widget.Get".to_owned()),
            ]
        );
    }

    /// Unqualified, a field and a same-named top-level type would risk colliding;
    /// qualifying the member keeps them apart.
    #[test]
    fn a_field_does_not_collide_with_a_same_named_member_of_another_type() {
        let qns = qualified(
            "namespace N { public class A { public int V; } public class B { public int V; } }\n",
        );
        let fields: Vec<&(String, String)> = qns.iter().filter(|(k, _)| k == "field").collect();
        assert_eq!(
            fields,
            [
                &("field".to_owned(), "A.V".to_owned()),
                &("field".to_owned(), "B.V".to_owned()),
            ]
        );
    }

    #[test]
    fn multiple_declarators_each_get_a_field() {
        let qns = qualified("namespace N { public class A { private int x, y; } }\n");
        let fields: Vec<&(String, String)> = qns.iter().filter(|(k, _)| k == "field").collect();
        assert_eq!(
            fields,
            [
                &("field".to_owned(), "A.x".to_owned()),
                &("field".to_owned(), "A.y".to_owned()),
            ]
        );
    }

    #[test]
    fn public_drives_is_exported() {
        let src = "namespace N {\n\
             public class Widget {\n\
               public int Pub() { return 0; }\n\
               private int Priv() { return 0; }\n\
               internal int Int() { return 0; }\n\
             }\n\
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

    /// A `using` binds to an external namespace node, not a file: `resolve_import`
    /// declines (namespaces don't map to paths) so the specifier falls through to
    /// `external_dep_key`, which keys the namespace itself.
    #[test]
    fn using_binds_to_an_external_namespace() {
        let adapter = Adapter::new();
        assert_eq!(
            adapter.external_dep_key("System.Text"),
            Some("System.Text".to_owned())
        );
        assert_eq!(adapter.external_dep_key(""), None);

        let ws = crate::Workspace::default();
        assert_eq!(
            adapter.resolve_import("System.Text", std::path::Path::new("/x/A.cs"), &ws),
            None
        );
    }

    #[test]
    fn is_test_path_matches_conventions() {
        let adapter = Adapter::new();
        assert!(adapter.is_test_path("src/WidgetTests.cs"));
        assert!(adapter.is_test_path("src/WidgetTest.cs"));
        assert!(adapter.is_test_path("test/Widget.cs"));
        assert!(adapter.is_test_path("app/tests/Widget.cs"));
        assert!(!adapter.is_test_path("src/Widget.cs"));
    }
}
