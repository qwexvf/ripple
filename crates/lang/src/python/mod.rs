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
    fn is_exported(&self, def: Node, _src: &[u8]) -> bool {
        let mut cur = def.parent();
        while let Some(n) = cur {
            match n.kind() {
                "module" => return true,
                // a nested def is reachable only through its owner
                "function_definition" | "block" if is_inside_function(n) => return false,
                _ => {}
            }
            cur = n.parent();
        }
        true
    }

    /// A method is qualified by its class, so `Client.send` and `Server.send` are
    /// different symbols.
    fn qualified_name(&self, kind: ir::NodeKind, name: &str, def: Node, src: &[u8]) -> String {
        if kind != ir::NodeKind::Function {
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
