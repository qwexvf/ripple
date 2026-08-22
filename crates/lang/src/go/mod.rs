//! Go adapter — Tier 0 defs plus Tier 1/2 imports and call edges.
//!
//! Go started life as the breadth proof of docs/11 phase 4 (tags-only, call
//! edges from `gopls`). The reachability-engine consolidation (phase 3) needs
//! the engine to self-seed external reachability without a language server, so
//! Go now carries its own `imports.scm` and `refs.scm`: an import path is its own
//! dep-key, and a `pkg.Foo()` selector call binds to the external `pkg.Foo`
//! symbol. There is still no `bindings_query` — Go's type-based member
//! resolution is left to the selector/receiver machinery.

use crate::{resolve_import, LanguageAdapter, Workspace};
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
        "go"
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_go::LANGUAGE.into()
    }

    fn file_globs(&self) -> &'static [&'static str] {
        &["*.go"]
    }

    fn is_test_path(&self, rel: &str) -> bool {
        rel.ends_with("_test.go")
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

    /// An import under this module's own path names a *local* package directory,
    /// not a file: `github.com/org/app/internal/config` → `<root>/internal/config`.
    /// Returning the directory lets the linker resolve `config.Foo()` against the
    /// package's own defs instead of minting an external stub (#85). A specifier
    /// that isn't under the module prefix falls through to `external_dep_key`.
    fn resolve_import(&self, spec: &str, _from: &Path, ws: &Workspace) -> Option<PathBuf> {
        let (module, root) = ws.go_module.as_ref()?;
        let rest = if spec == module {
            ""
        } else {
            spec.strip_prefix(module)?.strip_prefix('/')?
        };
        let dir = if rest.is_empty() {
            root.clone()
        } else {
            root.join(rest)
        };
        dir.is_dir().then_some(dir)
    }

    /// The dep-key of a Go import path is the path itself. A standard-library
    /// path (no dot in its first segment — `fmt`, `net/http`) is not a
    /// third-party dependency and normalizes away. Intra-module packages are
    /// resolved locally by [`LanguageAdapter::resolve_import`] before this runs.
    fn external_dep_key(&self, spec: &str) -> Option<String> {
        resolve_import::go_dep_key(spec)
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
    ///
    /// Type members carry the same qualifier for a second reason: unqualified,
    /// a field `Name` and a package-level `func Name` hash to the same
    /// `SymbolId`, and whichever the query engine yields first would swallow
    /// the other.
    fn qualified_name(&self, kind: ir::NodeKind, name: &str, def: Node, src: &[u8]) -> String {
        let owner = match kind {
            // an interface's `method_elem` has no receiver — its owner is the
            // type declaration it sits in.
            ir::NodeKind::Method => receiver_type(def, src).or_else(|| owner_type(def, src)),
            ir::NodeKind::Field => owner_type(def, src),
            _ => return name.to_owned(),
        };
        match owner {
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

/// Name of the type declaration a member was written inside: a struct field or
/// an interface method sits a couple of levels under the `type_spec` that names
/// the type it belongs to.
fn owner_type<'a>(def: Node, src: &'a [u8]) -> Option<&'a str> {
    let mut cur = def.parent()?;
    while cur.kind() != "type_spec" {
        cur = cur.parent()?;
    }
    let name = cur.child_by_field_name("name")?;
    Some(short_type(name.utf8_text(src).ok()?))
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

    /// The captures a tags-only language lives or dies by: what the query matches
    /// is the entire set of places an LSP-reported call can be attributed to.
    fn captured(src: &str) -> Vec<(String, String)> {
        captures(src).into_iter().map(|(k, n, _)| (k, n)).collect()
    }

    /// Captures keyed by qualified name — the string `SymbolId` is hashed from,
    /// so this is where two symbols either stay apart or collide.
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
        // no bindings query — Go leans on selector/receiver resolution instead
        assert!(adapter.bindings_query().is_none());
    }

    #[test]
    fn dep_key_is_the_import_path_and_stdlib_normalizes_away() {
        let adapter = Adapter::new();
        assert_eq!(
            adapter.external_dep_key("github.com/gin-gonic/gin"),
            Some("github.com/gin-gonic/gin".to_owned())
        );
        assert_eq!(
            adapter.external_dep_key("gopkg.in/yaml.v2"),
            Some("gopkg.in/yaml.v2".to_owned())
        );
        // no dot in the first segment → standard library, not a dependency
        assert_eq!(adapter.external_dep_key("fmt"), None);
        assert_eq!(adapter.external_dep_key("net/http"), None);
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
        let named_item: Vec<&(String, String)> = caps.iter().filter(|(_, n)| n == "Item").collect();
        assert_eq!(named_item, [&("class".to_owned(), "Item".to_owned())]);
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

    /// `item.Name` is a reference gopls can report, so the field needs a symbol.
    #[test]
    fn struct_fields_are_captured_and_owned_by_their_struct() {
        let qns = qualified(
            "package p\ntype Item struct {\n\tName string `json:\"name\"`\n\tcount int\n\tX, Y int\n}\ntype Order struct{ Name string }\n",
        );
        let fields: Vec<&(String, String)> = qns.iter().filter(|(k, _)| k == "field").collect();
        assert_eq!(
            fields,
            [
                &("field".to_owned(), "Item.Name".to_owned()),
                &("field".to_owned(), "Item.X".to_owned()),
                &("field".to_owned(), "Item.Y".to_owned()),
                &("field".to_owned(), "Item.count".to_owned()),
                &("field".to_owned(), "Order.Name".to_owned()),
            ]
        );
    }

    /// Unqualified, a field and a package-level func of the same name hash to
    /// one `SymbolId` and the loser is dropped.
    #[test]
    fn a_field_does_not_collide_with_a_package_level_function() {
        let qns = qualified(
            "package p\nfunc Name() string { return \"\" }\ntype Item struct{ Name string }\n",
        );
        assert_eq!(
            qns,
            [
                ("class".to_owned(), "Item".to_owned()),
                ("field".to_owned(), "Item.Name".to_owned()),
                ("function".to_owned(), "Name".to_owned()),
            ]
        );
    }

    /// Embedded fields have no name to key a symbol on, and a field of an
    /// anonymous struct — nested or in a function body — is not addressable
    /// from anywhere the owning type could be named.
    #[test]
    fn nameless_and_anonymous_struct_fields_are_not_captured() {
        let caps = captured(
            "package p\nimport \"io\"\ntype Item struct {\n\tio.Reader\n\t*Embedded\n\tinner struct{ deep int }\n}\nfunc f() {\n\tanon := struct{ q int }{}\n\t_ = anon\n}\n",
        );
        let fields: Vec<&(String, String)> = caps.iter().filter(|(k, _)| k == "field").collect();
        assert_eq!(fields, [&("field".to_owned(), "inner".to_owned())]);
    }

    /// A call through an interface is reported against `Doer.Do`, so the
    /// method element needs a symbol even though it has no receiver.
    #[test]
    fn interface_methods_are_captured_and_owned_by_their_interface() {
        let qns = qualified(
            "package p\nimport \"io\"\ntype Doer interface {\n\tDo() error\n\tio.Closer\n}\nfunc (t *Terminal) Do() error { return nil }\n",
        );
        let methods: Vec<&(String, String)> = qns.iter().filter(|(k, _)| k == "method").collect();
        assert_eq!(
            methods,
            [
                &("method".to_owned(), "Doer.Do".to_owned()),
                &("method".to_owned(), "Terminal.Do".to_owned()),
            ]
        );
    }

    /// Every member of a `const ( … )` block, including the valueless `iota`
    /// continuations and multi-name specs.
    #[test]
    fn const_block_members_are_all_captured() {
        let caps = captured(
            "package p\ntype Color int\nconst (\n\tRed Color = iota\n\tGreen\n\tBlue\n)\nconst A, B = 1, 2\n",
        );
        let vars: Vec<&(String, String)> = caps.iter().filter(|(k, _)| k == "variable").collect();
        assert_eq!(
            vars,
            [
                &("variable".to_owned(), "A".to_owned()),
                &("variable".to_owned(), "B".to_owned()),
                &("variable".to_owned(), "Blue".to_owned()),
                &("variable".to_owned(), "Green".to_owned()),
                &("variable".to_owned(), "Red".to_owned()),
            ]
        );
    }

    /// `var ( … )` nests a `var_spec_list` under the declaration; the ungrouped
    /// pattern walks straight past it.
    #[test]
    fn grouped_package_vars_are_captured() {
        let caps = captured("package p\nvar Top = 1\nvar (\n\tG1 = 1\n\tG2, G3 int\n)\n");
        let vars: Vec<&(String, String)> = caps.iter().filter(|(k, _)| k == "variable").collect();
        assert_eq!(
            vars,
            [
                &("variable".to_owned(), "G1".to_owned()),
                &("variable".to_owned(), "G2".to_owned()),
                &("variable".to_owned(), "G3".to_owned()),
                &("variable".to_owned(), "Top".to_owned()),
            ]
        );
    }
}
