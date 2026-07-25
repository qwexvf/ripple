//! TypeScript / TSX adapter (v0, Tier 0).

use crate::{resolve_import, LanguageAdapter, Workspace};
use std::path::{Path, PathBuf};

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
        "typescript"
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    }

    fn file_globs(&self) -> &'static [&'static str] {
        &["*.ts", "*.tsx", "*.mts", "*.cts"]
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

    fn resolve_import(&self, spec: &str, from_file: &Path, ws: &Workspace) -> Option<PathBuf> {
        let globs = self.file_globs();
        resolve_import::relative(spec, from_file, globs)
            .or_else(|| resolve_import::tsconfig_paths(spec, ws, globs))
            .or_else(|| resolve_import::workspace_package(spec, ws, globs))
    }

    fn extract_cross(&self, root: tree_sitter::Node, src: &[u8]) -> crate::cross::CrossFacts {
        crate::cross::typescript(root, src)
    }

    /// Exported if an ancestor is an `export_statement` (covers `export fn/class/
    /// const`, `export default`); a class member is never a module export.
    fn is_exported(&self, def: tree_sitter::Node, _src: &[u8]) -> bool {
        let mut cur = def.parent();
        while let Some(n) = cur {
            match n.kind() {
                "export_statement" => return true,
                "class_body" | "interface_body" | "enum_body" | "object_type"
                | "statement_block" | "object" | "arguments" => return false,
                _ => {}
            }
            cur = n.parent();
        }
        false
    }

    /// Methods/fields are qualified by their enclosing type (`Class.method`).
    fn qualified_name(
        &self,
        kind: ir::NodeKind,
        name: &str,
        def: tree_sitter::Node,
        src: &[u8],
    ) -> String {
        use ir::NodeKind::{Field, Method};
        if matches!(kind, Method | Field) {
            if let Some(ty) = enclosing_type_name(def, src) {
                return format!("{ty}.{name}");
            }
        }
        name.to_owned()
    }
}

/// Name of the class/interface/enum enclosing a member definition, if any.
fn enclosing_type_name(node: tree_sitter::Node, src: &[u8]) -> Option<String> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        match n.kind() {
            "class_declaration"
            | "abstract_class_declaration"
            | "interface_declaration"
            | "enum_declaration" => {
                return n
                    .child_by_field_name("name")
                    .and_then(|name| name.utf8_text(src).ok())
                    .map(str::to_owned);
            }
            _ => {}
        }
        cur = n.parent();
    }
    None
}
