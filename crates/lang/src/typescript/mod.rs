//! TypeScript / TSX adapter (v0, Tier 0).
//!
//! Two flavours, because tree-sitter ships two grammars: JSX nodes exist only in
//! the TSX one. Parsing `.tsx` with the plain TypeScript grammar left every JSX
//! body as an error node, so anything a component rendered was invisible — the
//! rendered component itself, and any call inside a JSX expression.

use crate::{resolve_import, LanguageAdapter, Workspace};
use std::path::{Path, PathBuf};

/// Every extension an import from TypeScript may land on, regardless of which
/// flavour is doing the importing.
const TS_FAMILY: &[&str] = &["*.ts", "*.tsx", "*.mts", "*.cts"];

/// Which grammar (and therefore which file set) this instance serves.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Flavour {
    Ts,
    Tsx,
}

pub struct Adapter {
    flavour: Flavour,
}

impl Adapter {
    /// Plain TypeScript (`.ts`, `.mts`, `.cts`).
    pub fn new() -> Self {
        Adapter {
            flavour: Flavour::Ts,
        }
    }

    /// TSX (`.tsx`) — the grammar that knows JSX.
    pub fn tsx() -> Self {
        Adapter {
            flavour: Flavour::Tsx,
        }
    }
}

impl Default for Adapter {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAdapter for Adapter {
    /// Distinct ids: queries are compiled once per id, and the two flavours compile
    /// against different grammars. Both are TypeScript to a language server, which
    /// is why `lsp::defaults` lists the same command for each.
    fn id(&self) -> &'static str {
        match self.flavour {
            Flavour::Ts => "typescript",
            Flavour::Tsx => "tsx",
        }
    }

    fn grammar(&self) -> tree_sitter::Language {
        match self.flavour {
            Flavour::Ts => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Flavour::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        }
    }

    fn file_globs(&self) -> &'static [&'static str] {
        match self.flavour {
            Flavour::Ts => &["*.ts", "*.mts", "*.cts"],
            Flavour::Tsx => &["*.tsx"],
        }
    }

    fn tags_query(&self) -> &'static str {
        include_str!("queries/tags.scm")
    }

    fn imports_query(&self) -> Option<&'static str> {
        Some(include_str!("queries/imports.scm"))
    }

    fn refs_query(&self) -> Option<&'static str> {
        Some(match self.flavour {
            Flavour::Ts => include_str!("queries/refs.scm"),
            // JSX patterns are only valid against the TSX grammar
            Flavour::Tsx => concat!(
                include_str!("queries/refs.scm"),
                include_str!("queries/refs-jsx.scm")
            ),
        })
    }

    fn bindings_query(&self) -> Option<&'static str> {
        Some(include_str!("queries/bindings.scm"))
    }

    fn resolve_import(&self, spec: &str, from_file: &Path, ws: &Workspace) -> Option<PathBuf> {
        // Probe the whole family, not this flavour's globs: a `.tsx` module importing
        // a `.ts` one is the common case, and probing only `*.tsx` silently resolved
        // nothing — every aliased import in a TSX file disappeared.
        let globs = TS_FAMILY;
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
