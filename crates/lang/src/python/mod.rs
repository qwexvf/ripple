//! Python adapter (Tier 0–2).
//!
//! Module resolution is the honest cost of the language: a dotted name is a path
//! under some root on `sys.path`, and a leading dot counts directories upward.
//! Neither is knowable in general — an installed package looks exactly like a
//! local one — so this resolves what the repository itself contains and leaves the
//! rest unlinked. Under-link, never wrong-link.

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

const PY: &[&str] = &["*.py", "*.pyi"];

impl LanguageAdapter for Adapter {
    fn id(&self) -> &'static str {
        "python"
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_python::LANGUAGE.into()
    }

    fn file_globs(&self) -> &'static [&'static str] {
        PY
    }

    /// pytest's discovery rules, which are what almost every Python project uses.
    fn is_test_path(&self, rel: &str) -> bool {
        let file = rel.rsplit('/').next().unwrap_or(rel);
        file.starts_with("test_")
            || file.ends_with("_test.py")
            || rel.starts_with("tests/")
            || rel.contains("/tests/")
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

    /// Everything at module level is importable; there is no export list.
    ///
    /// A class member is not importable: `from m import send` cannot name
    /// `Client.send`. Letting members into the export table is a wrong-link, not a
    /// missing one — the table is keyed by bare name, so a class attribute
    /// `Config.DEBUG` would shadow a module-level `DEBUG` in the same file.
    fn is_exported(&self, def: Node, _src: &[u8]) -> bool {
        let mut cur = def.parent();
        while let Some(n) = cur {
            match n.kind() {
                "module" => return true,
                // a nested def is reachable only through its owner
                "function_definition" | "block" if is_inside_function(n) => return false,
                "block" if is_class_body(n) => return false,
                _ => {}
            }
            cur = n.parent();
        }
        true
    }

    /// A member is qualified by its class, so `Client.send` and `Server.send` are
    /// different symbols, and `Color.RED` is not `Status.RED`.
    ///
    /// `Function` is in the list because a method also matches the general function
    /// pattern in `tags.scm`; the two captures have to agree on a name, or the same
    /// method arrives as two symbols instead of one.
    fn qualified_name(&self, kind: ir::NodeKind, name: &str, def: Node, src: &[u8]) -> String {
        use ir::NodeKind::{Field, Function, Method};
        if !matches!(kind, Function | Method | Field) {
            return name.to_owned();
        }
        match enclosing_class(def, src) {
            Some(class) => format!("{class}.{name}"),
            None => name.to_owned(),
        }
    }

    /// `from pkg.mod import x` → `pkg/mod.py`; `from .mod import x` → a sibling;
    /// `from ..pkg import x` → one directory up. A specifier that resolves to no
    /// file in this repository is an installed dependency, and is left alone.
    fn resolve_import(&self, spec: &str, from_file: &Path, _ws: &Workspace) -> Option<PathBuf> {
        let dir = from_file.parent()?;
        let dots = spec.chars().take_while(|c| *c == '.').count();
        let rest = spec.trim_start_matches('.');

        let base = if dots == 0 {
            // absolute: try it against every ancestor, since the import root is
            // whichever directory happens to be on sys.path
            return ancestors(dir)
                .find_map(|root| probe_module(&root.join(rest.replace('.', "/"))));
        } else {
            // `.` is this directory, `..` one above, and so on
            let mut base = dir.to_path_buf();
            for _ in 1..dots {
                base = base.parent()?.to_path_buf();
            }
            base
        };
        if rest.is_empty() {
            return probe_module(&base); // `from . import x` — the package itself
        }
        probe_module(&base.join(rest.replace('.', "/")))
    }
}

/// A module path is either `name.py` or `name/__init__.py`.
fn probe_module(base: &Path) -> Option<PathBuf> {
    resolve_import::probe(base, PY).or_else(|| resolve_import::probe(&base.join("__init__"), PY))
}

fn ancestors(dir: &Path) -> impl Iterator<Item = &Path> {
    std::iter::successors(Some(dir), |d| d.parent())
}

fn is_inside_function(node: Node) -> bool {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.kind() == "function_definition" {
            return true;
        }
        cur = n.parent();
    }
    false
}

fn is_class_body(node: Node) -> bool {
    node.parent()
        .is_some_and(|p| p.kind() == "class_definition")
}

fn enclosing_class<'a>(def: Node, src: &'a [u8]) -> Option<&'a str> {
    let mut node = def;
    while let Some(parent) = node.parent() {
        if parent.kind() == "class_definition" {
            return parent
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(src).ok());
        }
        node = parent;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use ir::NodeKind;
    use streaming_iterator::StreamingIterator;

    fn parse(src: &str) -> tree_sitter::Tree {
        let mut p = tree_sitter::Parser::new();
        p.set_language(&Adapter::new().grammar())
            .expect("python grammar");
        p.parse(src, None).expect("parse")
    }

    /// Every `@def.*` capture with the node it is anchored on, in the order the
    /// query engine reports them. The order is part of the contract: a symbol
    /// matched by two patterns keeps the kind of the first match.
    fn captured<'t>(tree: &'t tree_sitter::Tree, src: &str) -> Vec<(String, String, Node<'t>)> {
        let adapter = Adapter::new();
        let lang = adapter.grammar();
        let query = tree_sitter::Query::new(&lang, adapter.tags_query()).expect("tags.scm");
        let bytes = src.as_bytes();
        let names = query.capture_names();
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), bytes);
        let mut out = Vec::new();
        while let Some(m) = matches.next() {
            let mut def = None;
            let mut name = None;
            for cap in m.captures {
                let cap_name = names[cap.index as usize];
                if let Some(kind) = cap_name.strip_prefix("def.") {
                    def = Some((kind.to_owned(), cap.node));
                } else if cap_name == "name" {
                    name = cap.node.utf8_text(bytes).ok().map(str::to_owned);
                }
            }
            if let (Some((kind, node)), Some(name)) = (def, name) {
                out.push((kind, name, node));
            }
        }
        out
    }

    /// (kind, qualified name) as the graph would hold it: one entry per symbol,
    /// collapsed the way `resolve::index_defs` collapses a repeated `SymbolId` —
    /// first kind wins.
    fn symbols(src: &str) -> Vec<(String, String)> {
        let tree = parse(src);
        let adapter = Adapter::new();
        let bytes = src.as_bytes();
        let mut out: Vec<(String, String)> = Vec::new();
        for (kind, name, node) in captured(&tree, src) {
            let node_kind =
                NodeKind::from_capture(&format!("def.{kind}")).expect("a legal capture name");
            let qualified = adapter.qualified_name(node_kind, &name, node, bytes);
            if !out.iter().any(|(_, q)| *q == qualified) {
                out.push((kind, qualified));
            }
        }
        out
    }

    fn pair(kind: &str, name: &str) -> (String, String) {
        (kind.to_owned(), name.to_owned())
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

    /// A method matches the method pattern *and* the general function one. The
    /// method pattern is listed first so the surviving kind is `Method`; without
    /// it `class_of` sees a Function, and member-call resolution has no class to
    /// look a `send()` up in.
    #[test]
    fn a_method_beats_the_general_function_pattern() {
        let syms = symbols("class Client:\n    def send(self, body):\n        return body\n");
        assert_eq!(
            syms,
            [pair("class", "Client"), pair("method", "Client.send")]
        );
    }

    /// Two classes with a same-named method must not collapse onto one symbol.
    #[test]
    fn methods_are_qualified_by_their_class() {
        let syms = symbols(
            "class Client:\n    def send(self):\n        pass\n\nclass Server:\n    def send(self):\n        pass\n\ndef send():\n    pass\n",
        );
        assert_eq!(
            syms,
            [
                pair("class", "Client"),
                pair("method", "Client.send"),
                pair("class", "Server"),
                pair("method", "Server.send"),
                pair("function", "send"),
            ]
        );
    }

    /// `@property` and friends wrap the definition in a `decorated_definition`, so
    /// a method pattern anchored only on a direct child of the class body loses it
    /// and it falls through to the plain-function capture.
    #[test]
    fn a_decorated_method_is_still_a_method() {
        let syms = symbols(
            "class Client:\n    @property\n    def host(self):\n        return self._host\n\n    @staticmethod\n    def build():\n        pass\n",
        );
        assert_eq!(
            syms,
            [
                pair("class", "Client"),
                pair("method", "Client.host"),
                pair("method", "Client.build"),
            ]
        );
    }

    /// The grammar wraps a decorated module-level definition too. Nothing anchors
    /// the free-function or class patterns to their parent, so both survive it —
    /// this pins that, because a decorated `def` is most of a Flask/FastAPI app.
    #[test]
    fn decorated_module_level_definitions_are_not_dropped() {
        let syms = symbols(
            "@app.route(\"/\")\ndef index():\n    pass\n\n@dataclass\nclass Config:\n    pass\n",
        );
        assert_eq!(syms, [pair("function", "index"), pair("class", "Config")]);
    }

    /// Enum members and class constants are addressable symbols (`Color.RED`), and
    /// ripple had none of them: the query captured no class-level assignment at all.
    #[test]
    fn class_attributes_are_captured_as_fields() {
        let syms = symbols("class Color(Enum):\n    RED = 1\n    GREEN = 2\n");
        assert_eq!(
            syms,
            [
                pair("class", "Color"),
                pair("field", "Color.RED"),
                pair("field", "Color.GREEN"),
            ]
        );
    }

    /// Annotated forms are the same `assignment` node in this grammar, so the one
    /// pattern has to cover a dataclass field with and without a default.
    #[test]
    fn annotated_class_attributes_are_captured() {
        let syms = symbols("class Config:\n    host: str\n    port: int = 8080\n");
        assert_eq!(
            syms,
            [
                pair("class", "Config"),
                pair("field", "Config.host"),
                pair("field", "Config.port"),
            ]
        );
    }

    /// A field on one class must not collide with a same-named field on another.
    #[test]
    fn fields_of_different_classes_stay_distinct() {
        let syms = symbols("class Color(Enum):\n    RED = 1\n\nclass Status(Enum):\n    RED = 9\n");
        let fields: Vec<&(String, String)> = syms.iter().filter(|(k, _)| k == "field").collect();
        assert_eq!(
            fields,
            [&pair("field", "Color.RED"), &pair("field", "Status.RED")]
        );
    }

    /// Only module-level and class-level bindings are symbols. A local is not
    /// addressable, and indexing locals buries the graph in noise.
    #[test]
    fn function_local_bindings_are_not_captured() {
        let syms = symbols(
            "TOP = 1\n\ndef run():\n    inner = 2\n    return inner\n\nclass C:\n    ATTR = 3\n\n    def m(self):\n        local = 4\n        return local\n",
        );
        let bound: Vec<&(String, String)> = syms
            .iter()
            .filter(|(k, _)| k == "variable" || k == "field")
            .collect();
        assert_eq!(bound, [&pair("variable", "TOP"), &pair("field", "C.ATTR")]);
    }

    /// The `@def.variable` capture used to sit on the enclosing `module`, which is
    /// the node whose span becomes the definition's — so every module-level
    /// constant claimed the whole file, and any hunk in it was attributed there.
    #[test]
    fn a_module_variable_spans_only_its_assignment() {
        let src = "VERSION = \"1\"\n\n\ndef run():\n    pass\n";
        let tree = parse(src);
        let bytes = src.as_bytes();
        let var = captured(&tree, src)
            .into_iter()
            .find(|(kind, _, _)| kind == "variable")
            .expect("the module-level binding is captured");
        assert_eq!(var.2.utf8_text(bytes).expect("text"), "VERSION = \"1\"");
    }

    /// The export table is keyed by bare name, so a class member in it shadows a
    /// module-level definition that shares the name — `from m import DEBUG` would
    /// land on `Config.DEBUG` instead of the constant. A member is not importable.
    #[test]
    fn class_members_are_not_module_exports() {
        let src = "DEBUG = True\n\ndef send():\n    pass\n\nclass Config:\n    DEBUG = False\n\n    def send(self):\n        pass\n";
        let tree = parse(src);
        let adapter = Adapter::new();
        let bytes = src.as_bytes();
        let mut flags: Vec<(String, bool)> = Vec::new();
        for (kind, name, node) in captured(&tree, src) {
            let node_kind =
                NodeKind::from_capture(&format!("def.{kind}")).expect("a legal capture name");
            let qualified = adapter.qualified_name(node_kind, &name, node, bytes);
            if flags.iter().any(|(q, _)| *q == qualified) {
                continue;
            }
            flags.push((qualified, adapter.is_exported(node, bytes)));
        }
        assert_eq!(
            flags,
            [
                ("DEBUG".to_owned(), true),
                ("send".to_owned(), true),
                ("Config".to_owned(), true),
                ("Config.DEBUG".to_owned(), false),
                ("Config.send".to_owned(), false),
            ]
        );
    }
}
