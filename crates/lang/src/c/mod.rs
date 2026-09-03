//! C adapter — Tier 0 defs plus Tier 1/2 include and call captures.
//!
//! C has no classes, methods, or visibility keywords, so the vocabulary is
//! mapped structurally: a struct/union becomes a "class", a typedef a "type",
//! and file-scope declarations "variables". Linkage stands in for visibility —
//! a top-level symbol is externally visible unless it is declared `static`.

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
        "c"
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_c::LANGUAGE.into()
    }

    /// `.c` only. A `.h` is ambiguous — the dominant C++ convention names headers
    /// `.h` too — and tree-sitter-c cannot parse a `namespace`/`template`, so
    /// claiming it here extracted *nothing* from a C++ header (#119). The C++
    /// grammar is very nearly a superset of C, so it reads a C header correctly;
    /// `.h` belongs to that adapter instead.
    fn file_globs(&self) -> &'static [&'static str] {
        &["*.c"]
    }

    /// Keep it simple: a file under a `test`/`tests` path segment is a test.
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

    /// A quoted `#include "foo.h"` names a local header. C has no `./` convention —
    /// the path is relative to the including file's directory or to a project
    /// include root above it — so probe the file's own directory first, then walk
    /// up a bounded number of ancestors, trying both `<ancestor>/foo.h` and the
    /// very common `<ancestor>/include/foo.h` layout. A system `<stdio.h>` include
    /// (specifier starts with `<`) is never local: return `None` so it binds as an
    /// external dependency via `external_dep_key`.
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
    /// the system-include angle brackets stripped (`<stdio.h>` → `stdio.h`). This
    /// is what mints an external node + `Imports` edge for a standard-library or
    /// third-party header.
    fn external_dep_key(&self, spec: &str) -> Option<String> {
        let s = spec.trim_start_matches('<').trim_end_matches('>');
        (!s.is_empty()).then(|| s.to_owned())
    }

    /// C uses `static` for file-local (internal) linkage. A top-level function or
    /// variable is externally visible unless it carries a `static`
    /// storage-class specifier, so the default is exported.
    fn is_exported(&self, def: Node, src: &[u8]) -> bool {
        !has_static_storage(def, src)
    }

    /// Struct and union fields are qualified by their owning specifier's name —
    /// `Point.x` and `Rect.x` stay distinct and neither collides with a
    /// file-scope `x`. Everything else keeps its bare name.
    fn qualified_name(&self, kind: ir::NodeKind, name: &str, def: Node, src: &[u8]) -> String {
        if kind != ir::NodeKind::Field {
            return name.to_owned();
        }
        match owner_type(def, src) {
            Some(owner) => format!("{owner}.{name}"),
            None => name.to_owned(),
        }
    }
}

/// Does this top-level definition carry a `static` storage-class specifier? The
/// specifier is a direct child of the `function_definition`/`declaration`.
fn has_static_storage(def: Node, src: &[u8]) -> bool {
    let mut c = def.walk();
    for child in def.children(&mut c) {
        if child.kind() == "storage_class_specifier"
            && child.utf8_text(src).is_ok_and(|t| t == "static")
        {
            return true;
        }
    }
    false
}

/// Name of the struct/union a field was written inside — a `field_declaration`
/// sits a couple of levels under the `struct_specifier`/`union_specifier` that
/// names the owning type.
fn owner_type<'a>(def: Node, src: &'a [u8]) -> Option<&'a str> {
    let mut cur = def.parent()?;
    while cur.kind() != "struct_specifier" && cur.kind() != "union_specifier" {
        cur = cur.parent()?;
    }
    let name = cur.child_by_field_name("name")?;
    name.utf8_text(src).ok()
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
    }

    #[test]
    fn captured_covers_every_def_form() {
        let caps = captured(
            "int add(int a, int b) { return a + b; }\n\
             struct Point { int x; int y; };\n\
             enum Color { RED, GREEN };\n\
             typedef int MyInt;\n",
        );
        assert_eq!(
            caps,
            [
                ("class".to_owned(), "Point".to_owned()),
                ("enum".to_owned(), "Color".to_owned()),
                ("field".to_owned(), "x".to_owned()),
                ("field".to_owned(), "y".to_owned()),
                ("function".to_owned(), "add".to_owned()),
                ("type".to_owned(), "MyInt".to_owned()),
            ]
        );
    }

    #[test]
    fn fields_are_qualified_by_owner() {
        let qns = qualified(
            "struct Point { int x; int y; };\n\
             struct Rect { int x; int w; };\n",
        );
        let fields: Vec<&(String, String)> = qns.iter().filter(|(k, _)| k == "field").collect();
        assert_eq!(
            fields,
            [
                &("field".to_owned(), "Point.x".to_owned()),
                &("field".to_owned(), "Point.y".to_owned()),
                &("field".to_owned(), "Rect.w".to_owned()),
                &("field".to_owned(), "Rect.x".to_owned()),
            ]
        );
    }

    #[test]
    fn static_is_not_exported() {
        let src = "int visible(void) { return 0; }\n\
                   static int hidden(void) { return 0; }\n\
                   int shared = 1;\n\
                   static int local = 2;\n";
        let tree = parse(src);
        let adapter = Adapter::new();
        let bytes = src.as_bytes();

        let fns: Vec<bool> = find(&tree, "function_definition")
            .into_iter()
            .map(|f| adapter.is_exported(f, bytes))
            .collect();
        assert_eq!(fns, [true, false]);

        let vars: Vec<bool> = find(&tree, "declaration")
            .into_iter()
            .map(|d| adapter.is_exported(d, bytes))
            .collect();
        assert_eq!(vars, [true, false]);
    }

    #[test]
    fn locals_are_not_captured() {
        let caps = captured(
            "int top = 1;\n\
             int f(void) {\n\
                 int inner = 2;\n\
                 int also;\n\
                 return inner + also;\n\
             }\n",
        );
        let vars: Vec<&(String, String)> = caps.iter().filter(|(k, _)| k == "variable").collect();
        assert_eq!(vars, [&("variable".to_owned(), "top".to_owned())]);
    }

    /// An anonymous struct behind a typedef is named by the typedef. Its fields
    /// have no single-segment owner to qualify them by, so — like Go's anonymous
    /// struct fields — they are deliberately left uncaptured.
    #[test]
    fn anonymous_typedef_struct_is_named_by_the_typedef() {
        let caps = captured("typedef struct { int v; } Node;\n");
        assert_eq!(caps, [("type".to_owned(), "Node".to_owned())]);
    }

    /// `typedef struct Tag {...} Tag;` — the tag and the typedef share a name.
    /// Capture it once (as the class), never also as a `type`: two nodes with the
    /// same name hash to one `SymbolId` and clobber each other non-deterministically.
    #[test]
    fn typedef_of_a_named_struct_is_not_also_captured_as_a_type() {
        let caps = captured("typedef struct Tag { int v; } Tag;\n");
        assert_eq!(
            caps,
            [
                ("class".to_owned(), "Tag".to_owned()),
                ("field".to_owned(), "v".to_owned()),
            ]
        );
    }

    #[test]
    fn a_union_is_captured_as_a_class_with_owned_fields() {
        let qns = qualified("union U { int a; float b; };\n");
        assert_eq!(
            qns,
            [
                ("class".to_owned(), "U".to_owned()),
                ("field".to_owned(), "U.a".to_owned()),
                ("field".to_owned(), "U.b".to_owned()),
            ]
        );
    }

    /// A quoted include resolves to the header in the including file's directory;
    /// a system `<...>` include does not, and binds as an external dep instead.
    #[test]
    fn quoted_include_resolves_locally_and_system_include_is_external() {
        let dir =
            std::env::temp_dir().join(format!("ripple-c-ri-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/jv.c"), "").unwrap();
        std::fs::write(dir.join("src/jv.h"), "").unwrap();
        let from = dir.join("src/jv.c");
        let adapter = Adapter::new();
        let ws = Workspace::default();

        assert_eq!(
            adapter.resolve_import("jv.h", &from, &ws),
            dir.join("src/jv.h").canonicalize().ok()
        );
        assert_eq!(adapter.resolve_import("<stdio.h>", &from, &ws), None);
        assert_eq!(
            adapter.external_dep_key("<stdio.h>"),
            Some("stdio.h".to_owned())
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A header under a project `include/` root resolves from a source file that
    /// sits in a sibling `src/` — the common split-layout that a bare relative
    /// probe misses.
    #[test]
    fn include_root_layout_resolves() {
        let dir =
            std::env::temp_dir().join(format!("ripple-c-inc-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("include/lib")).unwrap();
        std::fs::write(dir.join("src/main.c"), "").unwrap();
        std::fs::write(dir.join("include/lib/api.h"), "").unwrap();
        let adapter = Adapter::new();

        assert_eq!(
            adapter.resolve_import("lib/api.h", &dir.join("src/main.c"), &Workspace::default()),
            dir.join("include/lib/api.h").canonicalize().ok()
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
