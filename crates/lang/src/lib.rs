//! The language seam. Everything language-specific lives here (and in the
//! `.scm` query data files). Adding a language = a new module + a registry line;
//! nothing above `ir` changes. See docs/05-language-support.md.

pub mod cross;
pub mod elixir;
pub mod gleam;
pub mod go;
pub mod graphql;
pub mod html;
pub mod php;
pub mod python;
pub mod resolve_import;
pub mod ruby;
pub mod rust;
pub mod spec;
pub mod svelte;
pub mod typescript;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Leading-comment extraction shared by the default `LanguageAdapter::doc`.
mod doc_comment {
    /// The run of comment lines directly above `def`, stripped of comment markers
    /// and collapsed to one line. `None` when there is no adjacent comment.
    ///
    /// Adjacency is by row: a comment separated from the definition by a blank line
    /// is a section header or a leftover, not this symbol's doc, so the walk stops
    /// at the first gap. An attribute or decorator between the comment and the
    /// definition (`#[instrument]`, `@Get('/login')`) is seen through, since the doc
    /// sits above it — otherwise every decorated handler, the shape this feature most
    /// targets, would lose its doc. A comment with code before it on its line is a
    /// trailing comment on the statement above, not a doc, so it stops the walk.
    /// Capped so a license header above a top-level symbol cannot bloat the index.
    pub fn preceding(def: tree_sitter::Node, src: &[u8]) -> Option<String> {
        // The captured def is often nested — `export function f` wraps the function
        // in an `export_statement`, a decorated def wraps it with its decorator — so
        // the doc comment is a sibling of the wrapper, not of the def. Climb to that
        // wrapper first, or the walk starts from a node that has no prior sibling.
        let mut anchor = def;
        while let Some(p) = anchor.parent() {
            let same_line = p.start_position().row == def.start_position().row;
            if p.parent().is_some() && (same_line || is_wrapper(p.kind())) {
                anchor = p;
            } else {
                break;
            }
        }
        let mut parts: Vec<String> = Vec::new();
        let mut next_start = anchor.start_position().row;
        let mut cur = anchor.prev_sibling();
        while let Some(n) = cur {
            // a blank-line gap means this sibling is not attached to the definition
            if next_start.saturating_sub(n.end_position().row) > 1 {
                break;
            }
            let kind = n.kind();
            if kind.contains("comment") {
                if !line_leading(n, src) {
                    break; // a trailing comment on the line above, not a doc
                }
                if let Ok(t) = n.utf8_text(src) {
                    parts.push(strip(t));
                }
                next_start = n.start_position().row;
            } else if is_decorator(kind) {
                next_start = n.start_position().row; // see past it to the doc above
            } else {
                break;
            }
            cur = n.prev_sibling();
        }
        if parts.is_empty() {
            return None;
        }
        parts.reverse();
        let joined = parts.join(" ");
        let collapsed = joined.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            return None;
        }
        Some(collapsed.chars().take(400).collect())
    }

    /// Is `n` the first non-whitespace thing on its line? A comment that is not
    /// (code precedes it) is a trailing comment on that statement, not a doc.
    fn line_leading(n: tree_sitter::Node, src: &[u8]) -> bool {
        let mut i = n.start_byte();
        while i > 0 {
            let b = src[i - 1];
            if b == b'\n' {
                break;
            }
            if !b.is_ascii_whitespace() {
                return false;
            }
            i -= 1;
        }
        true
    }

    /// Attribute/decorator/annotation node kinds — the thing that can legitimately
    /// sit between a doc comment and the definition it documents.
    fn is_decorator(kind: &str) -> bool {
        kind.contains("attribute") || kind.contains("decorator") || kind.contains("annotation")
    }

    /// Node kinds that wrap a definition without being on its own line — a doc
    /// comment attaches above the wrapper, so the walk must start from the wrapper.
    fn is_wrapper(kind: &str) -> bool {
        kind.contains("export") || kind.contains("decorated") || kind.contains("attributed")
    }

    /// Strip line/block comment punctuation without knowing the language: every
    /// grammar's markers are a subset of these.
    fn strip(text: &str) -> String {
        text.lines()
            .map(|line| {
                line.trim()
                    .trim_start_matches("///")
                    .trim_start_matches("//!")
                    .trim_start_matches("//")
                    .trim_start_matches("/**")
                    .trim_start_matches("/*")
                    .trim_end_matches("*/")
                    .trim_start_matches('*')
                    .trim_start_matches('#')
                    .trim()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

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

    /// Leading doc comment for a definition, if any — searchable prose so a task
    /// described in words ("limit login attempts") can reach a symbol whose
    /// identifier shares none of them (`query::locate`). The default collects the
    /// run of comment lines directly above the definition, which covers every
    /// grammar whose comment node kind carries "comment" in its name (TS, Elixir,
    /// Rust, Go, Python, Gleam). Override for languages whose docs are not
    /// preceding comments (a Python docstring, an Elixir `@doc` attribute).
    fn doc(&self, def: tree_sitter::Node, src: &[u8]) -> Option<String> {
        doc_comment::preceding(def, src)
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

    /// Definitions this file contributes that no `tags.scm` capture can produce —
    /// because they have no defining AST node that carries a name. The Svelte/Vue
    /// single-file component is the case: the component *is* the file, named by the
    /// file, so there is nothing in the tree for `@name` to read.
    ///
    /// `module_path` is the file's module-relative path, so the adapter can derive
    /// the component name from the file stem and mint a stable `SymbolId`. Runs on
    /// the host parse only (never inside an embedded region). Default: none, so no
    /// existing adapter changes. See #47.
    fn synthetic_defs(
        &self,
        _module_path: &str,
        _root: tree_sitter::Node,
        _src: &[u8],
    ) -> Vec<ir::Node> {
        Vec::new()
    }

    /// Regions of this file written in another language, as `(adapter id, range)`
    /// in this file's own byte+point coordinates. A single-file component (`.vue`,
    /// `.svelte`, `.html` with inline `<script>`) is a template plus a script block
    /// that is really TypeScript. `parse` re-parses each range with the named
    /// adapter using tree-sitter `included_ranges`, so the region's nodes report
    /// positions in the host file — no span offsetting to get wrong. Default: none,
    /// the file is the one language its extension says. The range must come from a
    /// node of *this* adapter's parse of the file, so its points are already host
    /// coordinates. See #46.
    fn embedded_regions(
        &self,
        _root: tree_sitter::Node,
        _src: &[u8],
    ) -> Vec<(&'static str, tree_sitter::Range)> {
        Vec::new()
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

    /// The dependency key of a specifier that resolves *outside* the indexed
    /// roots, or `None` for a specifier that could only be local (a relative
    /// path). This is what lets the resolver mint an `External` node for a bare
    /// import like `urql` or `react-dom/client` (both keyed `react-dom`/`urql`).
    ///
    /// Only called when `resolve_import` returned `None`, so a specifier that
    /// *did* resolve locally (including a tsconfig path alias) never reaches here.
    /// Default `None` — a language with no external binding pass is unchanged.
    fn external_dep_key(&self, _spec: &str) -> Option<String> {
        None
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
        Box::new(python::Adapter::new()),
        Box::new(html::Adapter::new()),
        Box::new(php::Adapter::new()),
        Box::new(ruby::Adapter::new()),
        Box::new(svelte::Adapter::new()),
    ]
}

/// Bump when adapter *logic* changes what gets extracted without any `.scm` edit —
/// `embedded_regions`, `extract_cross`, `qualified_name`, `test_scopes`, etc. Query
/// sources and grammar changes are caught automatically by `registry_fingerprint`;
/// this is the escape hatch for the Rust that isn't a query string. See #71.
const EXTRACT_LOGIC_VERSION: u32 = 1;

/// A fingerprint of every adapter's extraction *inputs*: the `.scm` query sources,
/// the grammar identity, and `EXTRACT_LOGIC_VERSION`.
///
/// Folded into the extract-cache key (`parse::extract_cache_key`) so a query,
/// grammar, or adapter-logic change invalidates a warm `.ripple` the same way a
/// struct-shape change does. Without it, editing a `tags.scm` re-uses stale cached
/// extracts until the cache is deleted by hand — the whole point of #71.
pub fn registry_fingerprint() -> String {
    use std::hash::{Hash, Hasher};
    let mut adapters = registry();
    adapters.sort_by_key(|a| a.id());
    let mut h = std::collections::hash_map::DefaultHasher::new();
    EXTRACT_LOGIC_VERSION.hash(&mut h);
    for a in &adapters {
        a.id().hash(&mut h);
        let g = a.grammar();
        g.abi_version().hash(&mut h);
        g.node_kind_count().hash(&mut h);
        a.tags_query().hash(&mut h);
        a.imports_query().unwrap_or("").hash(&mut h);
        a.refs_query().unwrap_or("").hash(&mut h);
        a.bindings_query().unwrap_or("").hash(&mut h);
    }
    format!("{:016x}", h.finish())
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

#[cfg(test)]
mod tests {
    /// Two builds of the same adapters must agree, or the extract cache is discarded
    /// on every run and the incremental index stops being incremental.
    #[test]
    fn fingerprint_is_stable_across_calls() {
        assert_eq!(super::registry_fingerprint(), super::registry_fingerprint());
        assert_eq!(super::registry_fingerprint().len(), 16);
    }

    /// The fingerprint must actually fold in the query sources — a build with an
    /// empty tags query hashes differently from the real one. This is the property
    /// #71 turns on: change a `.scm`, change the key, miss the stale cache.
    #[test]
    fn fingerprint_depends_on_query_sources() {
        use std::hash::{Hash, Hasher};
        let real = super::registry_fingerprint();
        // same walk as `registry_fingerprint` but with the tags query blanked, to
        // prove the query text is an input rather than incidental.
        let mut adapters = super::registry();
        adapters.sort_by_key(|a| a.id());
        let mut h = std::collections::hash_map::DefaultHasher::new();
        super::EXTRACT_LOGIC_VERSION.hash(&mut h);
        for a in &adapters {
            a.id().hash(&mut h);
            let g = a.grammar();
            g.abi_version().hash(&mut h);
            g.node_kind_count().hash(&mut h);
            "".hash(&mut h); // tags query blanked
            a.imports_query().unwrap_or("").hash(&mut h);
            a.refs_query().unwrap_or("").hash(&mut h);
            a.bindings_query().unwrap_or("").hash(&mut h);
        }
        assert_ne!(real, format!("{:016x}", h.finish()));
    }
}
