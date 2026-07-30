//! The language seam. Everything language-specific lives here (and in the
//! `.scm` query data files). Adding a language = a new module + a registry line;
//! nothing above `ir` changes. See docs/05-language-support.md.

pub mod cross;
pub mod elixir;
pub mod gleam;
pub mod go;
pub mod graphql;
pub mod resolve_import;
pub mod rust;
pub mod typescript;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Project resolution context: tsconfig `paths`/`baseUrl` aliases and workspace
/// package locations. Discovered once per index and passed to `resolve_import`.
/// Empty by default (relative resolution still works). See docs/05-language-support.md.
#[derive(Debug, Default)]
pub struct Workspace {
    /// Absolute base directory for non-relative `paths` targets (tsconfig baseUrl).
    pub base_url: Option<PathBuf>,
    /// tsconfig `paths`: pattern (e.g. `@app/*`) → replacement targets (`src/*`).
    pub paths: Vec<(String, Vec<String>)>,
    /// Workspace package name → package directory.
    pub packages: HashMap<String, PathBuf>,
}

/// A language adapter. Tier 0 = grammar + tags query (required). Tier 1 =
/// imports query + `resolve_import`. Tier 2 = refs query. See docs/05-language-support.md.
pub trait LanguageAdapter: Send + Sync {
    fn id(&self) -> &'static str;
    fn grammar(&self) -> tree_sitter::Language;
    fn file_globs(&self) -> &'static [&'static str];
    /// tags.scm: captures (`@def.function`, `@name`) → IR nodes.
    fn tags_query(&self) -> &'static str;

    /// Does this repo-relative path hold tests? Convention, so it is language
    /// knowledge (`*.test.ts`, `*_test.go`, `test/…`). Default: no.
    ///
    /// A language whose tests live *inside* the file under test answers with
    /// `test_scopes` instead.
    fn is_test_path(&self, _rel_path: &str) -> bool {
        false
    }

    /// Regions of this file that are test-only, for languages where no path can
    /// tell (Rust's `#[cfg(test)] mod tests` sits in the file it tests). Spans, so
    /// nothing language-specific leaves the adapter. Default: none.
    fn test_scopes(&self, _root: tree_sitter::Node, _src: &[u8]) -> Vec<ir::Span> {
        Vec::new()
    }

    /// Is a definition exported/public? Language-specific (TS `export`, Elixir
    /// `def` vs `defp`, Go capitalization, …). Default: not exported.
    fn is_exported(&self, _def: tree_sitter::Node, _src: &[u8]) -> bool {
        false
    }

    /// Qualified name for a definition (e.g. TS methods → `Class.method` so
    /// same-named methods don't collide). Default: the bare name.
    fn qualified_name(
        &self,
        _kind: ir::NodeKind,
        name: &str,
        _def: tree_sitter::Node,
        _src: &[u8],
    ) -> String {
        name.to_owned()
    }

    /// Tier 1: imports.scm capturing `@import.source`, `@import.name`, `@import.default`.
    fn imports_query(&self) -> Option<&'static str> {
        None
    }
    /// Tier 2: refs.scm capturing `@ref.call`, `@ref.member`, `@ref.recv`.
    fn refs_query(&self) -> Option<&'static str> {
        None
    }
    /// Tier 2: bindings.scm capturing `@bind.name` with `@bind.ctor`/`@bind.type`
    /// for local identifier → type resolution of member calls.
    fn bindings_query(&self) -> Option<&'static str> {
        None
    }

    /// Tier 3: extract cross-service facts from the parsed AST (Absinthe fields,
    /// GraphQL operations, TS Document usage, Ecto refs). Runs during the single
    /// index parse; default = none. See `cross::CrossFacts`.
    fn extract_cross(&self, _root: tree_sitter::Node, _src: &[u8]) -> cross::CrossFacts {
        cross::CrossFacts::default()
    }
    /// Resolve a module specifier to a file on disk. Default handles relative
    /// paths; override for a language's own module system (tsconfig, packages).
    fn resolve_import(&self, spec: &str, from_file: &Path, _ws: &Workspace) -> Option<PathBuf> {
        resolve_import::relative(spec, from_file, self.file_globs())
    }
}

/// The GraphQL grammar (for parsing `.gql` operation documents in cross-service linking).
pub fn graphql_language() -> tree_sitter::Language {
    tree_sitter_graphql::LANGUAGE.into()
}

/// The adapter registry. One line per language.
pub fn registry() -> Vec<Box<dyn LanguageAdapter>> {
    vec![
        Box::new(typescript::Adapter::new()),
        Box::new(typescript::Adapter::tsx()),
        Box::new(elixir::Adapter::new()),
        Box::new(graphql::Adapter::new()),
        Box::new(rust::Adapter::new()),
        Box::new(go::Adapter::new()),
        Box::new(gleam::Adapter::new()),
    ]
}

/// Pick the adapter whose globs match a path, from an existing registry slice.
/// Prefer this in hot loops — it borrows instead of rebuilding the registry.
pub fn adapter_for<'a>(
    registry: &'a [Box<dyn LanguageAdapter>],
    path: &Path,
) -> Option<&'a dyn LanguageAdapter> {
    let name = path.file_name()?.to_str()?;
    registry
        .iter()
        .find(|a| a.file_globs().iter().any(|g| glob_match(g, name)))
        .map(AsRef::as_ref)
}

/// Convenience for one-off lookups (builds a fresh registry). Not for hot loops.
pub fn for_path(path: &Path) -> Option<Box<dyn LanguageAdapter>> {
    let name = path.file_name()?.to_str()?;
    registry()
        .into_iter()
        .find(|a| a.file_globs().iter().any(|g| glob_match(g, name)))
}

/// Minimal `*.ext` suffix matcher — enough for extension globs in v0.
fn glob_match(glob: &str, name: &str) -> bool {
    match glob.strip_prefix('*') {
        Some(suffix) => name.ends_with(suffix),
        None => glob == name,
    }
}
