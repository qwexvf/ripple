//! Language-agnostic tree-sitter driver: parse a file once, run the adapter's
//! queries, emit IR nodes + pre-resolution records (imports, refs, type
//! bindings). Reads capture names generically. See docs/04-architecture.md, v0-plan.md.

use anyhow::{Context, Result};
use ir::{Node, NodeKind, Span, SymbolId};
use lang::LanguageAdapter;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use streaming_iterator::StreamingIterator;
use tree_sitter::{
    Node as TsNode, Parser, Query, QueryCursor, QueryMatch, QueryPredicateArg, Tree,
};

/// The content hash a `CachedFile` is keyed on. One definition, because both the
/// indexer (is this file unchanged?) and the query side (is this answer still
/// true?) have to agree on what "unchanged" means.
pub fn content_hash(source: &str) -> String {
    blake3::hash(source.as_bytes()).to_hex().to_string()
}

/// A file's extraction plus its content hash — the unit of incremental caching.
/// Unchanged files reuse this across `index` runs, skipping the parse. See
/// docs/v0-plan.md "incremental".
mod schema;
pub use schema::{extract_cache_key, extract_shape};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedFile {
    pub canonical: PathBuf,
    pub module_path: String,
    pub hash: String,
    pub extract: FileExtract,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportRec {
    pub local_name: String,
    /// Name in the *source* module. `default` for a default import, `*` for a
    /// namespace import (`import * as ns`, `alias Foo.Bar` — the whole module is bound
    /// to one local name).
    pub imported_name: String,
    pub specifier: String,
    pub site: Span,
}

impl ImportRec {
    /// Does this binding name a whole module rather than one symbol?
    ///
    /// A namespace binding resolves differently: `ns.foo()` has to look `foo` up in the
    /// target module's exports, which is a member call whose receiver is pinned by the
    /// import rather than inferred from a type.
    pub fn is_namespace(&self) -> bool {
        self.imported_name == "*"
    }

    /// A module imported purely for its side effects (`import "polyfill"`): it
    /// binds no local name, so there is nothing to call — only the module-level
    /// import fact. Signalled by both name fields being empty.
    pub fn is_side_effect(&self) -> bool {
        self.local_name.is_empty() && self.imported_name.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefKind {
    Call,   // foo()
    Member, // obj.foo()
}

/// The receiver of a member call, classified syntactically (M2, no type inference).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Receiver {
    This,          // this.foo()
    Ident(String), // b.foo()  — resolved via local type bindings
    New(String),   // new Bar().foo()
    Other,         // chained / computed — falls back to candidates
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefRec {
    pub name: String,
    pub kind: RefKind,
    pub site: Span,
    pub receiver: Option<Receiver>, // Some for Member
    /// What preceded the name in a qualified call — `Client` in `Client::new(x)`,
    /// `resolve` in `resolve::link(x)`. Lets resolution prefer a definition that
    /// belongs to the qualifier instead of every same-named definition anywhere.
    /// `default` so a cache written before this field still loads.
    #[serde(default)]
    pub qualifier: Option<String>,
}

/// A local identifier → type-name binding (`const b = new Bar()`, `(b: Bar)`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindRec {
    pub name: String,
    pub type_name: String,
    /// Where the binding is declared. Two functions routinely bind the same name to
    /// different types, and a file-wide map answered whichever came last.
    pub site: Span,
}

/// A symbol this file passes through from another one: `export { a } from "./x"`,
/// `export { a as b } from "./x"`, or `export * from "./x"` (name `*`).
///
/// A barrel file defines nothing, so an import that resolves to it finds nothing
/// unless the chain is followed. One generated GraphQL barrel cost 693 edges on a
/// real app, because everything imports through it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReexportRec {
    /// The name as the *source* file knows it, or `*` for a whole-module re-export.
    pub name: String,
    /// The name consumers of *this* file import (differs only when aliased).
    pub exposed_as: String,
    pub specifier: String,
    pub site: Span,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct FileExtract {
    pub defs: Vec<Node>,
    pub imports: Vec<ImportRec>,
    /// Symbols re-exported from another module. No `serde(default)` on purpose: a
    /// cache row written before this field existed must be a miss, not a silent
    /// "this file re-exports nothing".
    pub reexports: Vec<ReexportRec>,
    pub refs: Vec<RefRec>,
    pub bindings: Vec<BindRec>,
    /// Spans a language marked test-only from inside the file (Rust's
    /// `#[cfg(test)] mod tests`). A definition inside one is test-side even
    /// though its path says nothing. No `serde(default)`, same reason as
    /// `reexports`: an older cache row must be a miss, not a silent "no tests".
    pub test_scopes: Vec<Span>,
    /// Cross-service facts (Absinthe fields, GraphQL ops, TS Document usage, …),
    /// extracted from the same parse so files aren't parsed twice.
    pub cross: lang::cross::CrossFacts,
}

/// An adapter's queries, compiled once and reused across all files. Compiling a
/// tree-sitter `Query` is expensive (builds automata); doing it per file per
/// query dominated indexing before this. `Query` is `Sync`, so a shared `&Queries`
/// is safe across rayon threads.
pub struct Queries {
    tags: Matcher,
    imports: Option<Matcher>,
    refs: Option<Matcher>,
    bindings: Option<Matcher>,
}

/// Predicates this engine actually evaluates. Anything else is refused at compile
/// time rather than ignored at match time: `predicates_hold` treats an unknown
/// operator as *passing*, so a query that filters on one silently matches
/// everything. That is how a `#match?` guarding JSX element names spent months
/// filtering nothing (#51), and how a `#match?` on `#[cfg(test)]` would have
/// marked every module in a repository as tests.
const SUPPORTED_PREDICATES: [&str; 6] = [
    "eq?",
    "not-eq?",
    "any-of?",
    "not-any-of?",
    "match?",
    "not-match?",
];

/// A `#match?` compiled once, at query-compile time.
///
/// Compiling the regex per match would put it in the hot loop; a query is compiled
/// once per language per index, so this is where it belongs.
struct RegexPred {
    pattern_index: usize,
    capture: u32,
    negated: bool,
    regex: regex::Regex,
}

/// A query plus the regex predicates the engine has to evaluate itself.
struct Matcher {
    query: Query,
    regexes: Vec<RegexPred>,
}

impl Matcher {
    fn compile(src: &str, lang: &tree_sitter::Language, what: &str, id: &str) -> Result<Matcher> {
        let query =
            Query::new(lang, src).with_context(|| format!("invalid {what} query for {id}"))?;
        let mut regexes = Vec::new();
        for pattern_index in 0..query.pattern_count() {
            for pred in query.general_predicates(pattern_index) {
                let op = &*pred.operator;
                if !SUPPORTED_PREDICATES.contains(&op) {
                    anyhow::bail!(
                        "{what} for {id} uses #{op}, which this engine does not evaluate — \
                         it would match everything. Supported: {}",
                        SUPPORTED_PREDICATES.join(", ")
                    );
                }
                if op != "match?" && op != "not-match?" {
                    continue;
                }
                let [QueryPredicateArg::Capture(capture), QueryPredicateArg::String(pattern)] =
                    &pred.args[..]
                else {
                    anyhow::bail!("{what} for {id}: #{op} wants a capture and a pattern");
                };
                regexes.push(RegexPred {
                    pattern_index,
                    capture: *capture,
                    negated: op == "not-match?",
                    regex: regex::Regex::new(pattern)
                        .with_context(|| format!("{what} for {id}: bad regex {pattern}"))?,
                });
            }
        }
        Ok(Matcher { query, regexes })
    }

    /// Does every regex predicate on this match's pattern hold?
    fn regexes_hold(&self, m: &QueryMatch, src: &[u8]) -> bool {
        self.regexes
            .iter()
            .filter(|r| r.pattern_index == m.pattern_index)
            .all(|r| {
                let text = m
                    .captures
                    .iter()
                    .find(|c| c.index == r.capture)
                    .and_then(|c| c.node.utf8_text(src).ok());
                match text {
                    Some(text) => r.regex.is_match(text) != r.negated,
                    None => true, // the capture is not in this match; nothing to judge
                }
            })
    }
}

impl Queries {
    pub fn compile(adapter: &dyn LanguageAdapter) -> Result<Queries> {
        let lang = adapter.grammar();
        let compile = |src: &str, what: &str| Matcher::compile(src, &lang, what, adapter.id());
        Ok(Queries {
            tags: compile(adapter.tags_query(), "tags.scm")?,
            imports: adapter
                .imports_query()
                .map(|q| compile(q, "imports.scm"))
                .transpose()?,
            refs: adapter
                .refs_query()
                .map(|q| compile(q, "refs.scm"))
                .transpose()?,
            bindings: adapter
                .bindings_query()
                .map(|q| compile(q, "bindings.scm"))
                .transpose()?,
        })
    }
}

/// What an embedded region needs to be extracted: the other adapters and their
/// compiled queries. A file that carries no embedded language never uses this, so
/// `extract_file` takes it as an `Option`. `parse::build_incremental` already holds
/// both, so passing them costs nothing.
pub struct EmbedCtx<'a> {
    pub registry: &'a [Box<dyn LanguageAdapter>],
    pub queries: &'a HashMap<&'a str, Queries>,
}

pub fn extract_file(
    source: &str,
    adapter: &dyn LanguageAdapter,
    module_path: &str,
    queries: &Queries,
    embed: Option<&EmbedCtx>,
) -> Result<FileExtract> {
    let language = adapter.grammar();
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .context("failed to set tree-sitter language")?;
    let tree = parser
        .parse(source, None)
        .context("tree-sitter returned no tree")?;
    let src = source.as_bytes();

    let mut defs = extract_defs(&tree, &queries.tags, src, module_path, adapter)?;
    // Defs with no defining AST node (a Svelte/Vue single-file component, named by
    // its file). The adapter mints these directly since no `@name` capture can. #47
    defs.extend(adapter.synthetic_defs(module_path, tree.root_node(), src));
    let mut imports = queries
        .imports
        .as_ref()
        .map(|q| extract_imports(&tree, q, src))
        .transpose()?
        .unwrap_or_default();
    let mut reexports = queries
        .imports
        .as_ref()
        .map(|q| extract_reexports(&tree, q, src))
        .transpose()?
        .unwrap_or_default();
    let mut refs = queries
        .refs
        .as_ref()
        .map(|q| extract_refs(&tree, q, src))
        .transpose()?
        .unwrap_or_default();
    let mut bindings = queries
        .bindings
        .as_ref()
        .map(|q| extract_bindings(&tree, q, src))
        .transpose()?
        .unwrap_or_default();
    let mut test_scopes = adapter.test_scopes(tree.root_node(), src);
    let mut cross = adapter.extract_cross(tree.root_node(), src);

    // Embedded regions (a `.vue`/`.svelte`/`.html` `<script>` block). Each is
    // re-parsed with the named adapter over the *whole* source but with
    // `included_ranges` pinned to the region, so the sub-tree's node positions are
    // already in host-file coordinates — the region's symbols and edges land at the
    // line a human and a language server see, with no offset arithmetic. (#46)
    if let Some(ctx) = embed {
        for (id, range) in adapter.embedded_regions(tree.root_node(), src) {
            let Some(ea) = ctx.registry.iter().find(|a| a.id() == id) else {
                continue; // the file names an adapter this build doesn't have
            };
            let Some(eq) = ctx.queries.get(id) else {
                continue;
            };
            let Some(etree) = parse_region(source, &ea.grammar(), range) else {
                continue;
            };
            let ea = ea.as_ref();
            let eroot = etree.root_node();
            defs.extend(extract_defs(&etree, &eq.tags, src, module_path, ea)?);
            if let Some(q) = eq.imports.as_ref() {
                imports.extend(extract_imports(&etree, q, src)?);
                reexports.extend(extract_reexports(&etree, q, src)?);
            }
            if let Some(q) = eq.refs.as_ref() {
                refs.extend(extract_refs(&etree, q, src)?);
            }
            if let Some(q) = eq.bindings.as_ref() {
                bindings.extend(extract_bindings(&etree, q, src)?);
            }
            test_scopes.extend(ea.test_scopes(eroot, src));
            cross.merge(ea.extract_cross(eroot, src));
        }
    }

    Ok(FileExtract {
        defs,
        imports,
        reexports,
        refs,
        bindings,
        test_scopes,
        cross,
    })
}

/// Parse just `range` of `source` with `grammar`, using `included_ranges` so the
/// tree's node positions stay in the host file's coordinates. `None` on any
/// tree-sitter error (an unusable range, a grammar that won't load).
fn parse_region(
    source: &str,
    grammar: &tree_sitter::Language,
    range: tree_sitter::Range,
) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(grammar).ok()?;
    parser.set_included_ranges(&[range]).ok()?;
    parser.parse(source, None)
}

/// Convenience for one-off use (compiles queries each call). Not for hot loops.
pub fn extract(source: &str, adapter: &dyn LanguageAdapter) -> Result<Vec<Node>> {
    let queries = Queries::compile(adapter)?;
    Ok(extract_file(source, adapter, "<file>", &queries, None)?.defs)
}

fn extract_defs(
    tree: &Tree,
    matcher: &Matcher,
    src: &[u8],
    module_path: &str,
    adapter: &dyn LanguageAdapter,
) -> Result<Vec<Node>> {
    let query = &matcher.query;
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), src);

    let mut nodes = Vec::new();
    while let Some(m) = matches.next() {
        if !predicates_hold(query, m, src) || !matcher.regexes_hold(m, src) {
            continue;
        }
        let mut kind = None;
        let mut def_node = None;
        let mut name = None;
        for cap in m.captures {
            let cap_name = names[cap.index as usize];
            if cap_name == "name" {
                name = cap.node.utf8_text(src).ok().map(str::to_owned);
            } else if let Some(k) = NodeKind::from_capture(cap_name) {
                kind = Some(k);
                def_node = Some(cap.node);
            }
        }
        if let (Some(kind), Some(def_node), Some(name)) = (kind, def_node, name) {
            // export/private + name qualification are language-specific → adapter.
            let qualified_name = adapter.qualified_name(kind, &name, def_node, src);
            nodes.push(Node {
                id: SymbolId::of(module_path, &qualified_name),
                kind,
                name,
                qualified_name,
                module_path: module_path.to_owned(),
                span: span_of(def_node),
                extra_spans: Vec::new(),
                is_exported: adapter.is_exported(def_node, src),
                risk: ir::RiskScores::default(),
                doc: adapter.doc(def_node, src),
                route_path: None,
            });
        }
    }
    Ok(nodes)
}

/// Re-export statements, from the same query as imports (`reexport.*` captures).
fn extract_reexports(tree: &Tree, matcher: &Matcher, src: &[u8]) -> Result<Vec<ReexportRec>> {
    let query = &matcher.query;
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), src);

    let mut out = Vec::new();
    while let Some(m) = matches.next() {
        if !predicates_hold(query, m, src) || !matcher.regexes_hold(m, src) {
            continue;
        }
        let mut specifier = None;
        let mut named: Vec<(String, Span)> = Vec::new();
        let mut alias: Option<String> = None;
        let mut star: Option<Span> = None;
        for cap in m.captures {
            let text = cap.node.utf8_text(src).unwrap_or("").to_owned();
            match names[cap.index as usize] {
                "reexport.source" => specifier = Some(text),
                "reexport.name" => named.push((text, span_of(cap.node))),
                "reexport.alias" => alias = Some(text),
                "reexport.star" => star = Some(span_of(cap.node)),
                _ => {}
            }
        }
        let Some(specifier) = specifier else { continue };
        if let Some(site) = star {
            out.push(ReexportRec {
                name: "*".to_owned(),
                exposed_as: "*".to_owned(),
                specifier: specifier.clone(),
                site,
            });
        }
        for (name, site) in named {
            out.push(ReexportRec {
                exposed_as: alias.clone().unwrap_or_else(|| name.clone()),
                name,
                specifier: specifier.clone(),
                site,
            });
        }
    }
    Ok(out)
}

fn extract_imports(tree: &Tree, matcher: &Matcher, src: &[u8]) -> Result<Vec<ImportRec>> {
    let query = &matcher.query;
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), src);

    let mut out = Vec::new();
    while let Some(m) = matches.next() {
        if !predicates_hold(query, m, src) || !matcher.regexes_hold(m, src) {
            continue;
        }
        let mut specifier = None;
        let mut named: Vec<(String, Span)> = Vec::new();
        let mut alias: Option<String> = None;
        let mut default: Option<(String, Span)> = None;
        let mut namespace: Option<(String, Span)> = None;
        // a side-effect import (`import "x"`) captures only the specifier text here
        let mut bare: Option<(String, Span)> = None;
        for cap in m.captures {
            let text = cap.node.utf8_text(src).unwrap_or("").to_owned();
            match names[cap.index as usize] {
                "import.source" => specifier = Some(text),
                "import.name" => named.push((text, span_of(cap.node))),
                "import.alias" => alias = Some(text),
                "import.default" => default = Some((text, span_of(cap.node))),
                "import.namespace" => namespace = Some((text, span_of(cap.node))),
                "import.bare" => bare = Some((text, span_of(cap.node))),
                _ => {}
            }
        }
        // `import "polyfill"` — no clause, so the specifier is the only capture
        if let Some((spec, site)) = bare {
            out.push(ImportRec {
                local_name: String::new(),
                imported_name: String::new(),
                specifier: spec,
                site,
            });
            continue;
        }
        let Some(specifier) = specifier else { continue };
        for (n, site) in named {
            out.push(ImportRec {
                // `import { a as b }` binds `b` locally to the source's `a`; comparing
                // the wrong one made every call through an alias unresolvable
                local_name: alias.clone().unwrap_or_else(|| n.clone()),
                imported_name: n,
                specifier: specifier.clone(),
                site,
            });
        }
        if let Some((n, site)) = namespace {
            // A namespace binding's local name is an explicit `as` alias, else the
            // node's own text. Gleam names the module by its path (`a/b/scrub`) and
            // binds the last segment (`scrub`); other languages capture a bare
            // identifier here, which has no `/`, so the segment split is a no-op.
            let local_name = alias
                .clone()
                .unwrap_or_else(|| n.rsplit('/').next().unwrap_or(&n).to_owned());
            out.push(ImportRec {
                local_name,
                imported_name: "*".to_owned(),
                specifier: specifier.clone(),
                site,
            });
        }
        if let Some((n, site)) = default {
            out.push(ImportRec {
                local_name: n,
                imported_name: "default".to_owned(),
                specifier: specifier.clone(),
                site,
            });
        }
    }
    Ok(out)
}

/// Reference sites, from `ref.call` / `ref.member` / `ref.recv` captures.
///
/// A `ref.ignore` capture marks a region where references don't count. Patterns
/// in a tree-sitter query match independently, so a broad pattern can't be
/// narrowed by a more specific one — an adapter marks the exceptions instead
/// (Elixir: everything inside `@spec get(id :: String.t()) :: t()` parses as
/// calls, but names types).
fn extract_refs(tree: &Tree, matcher: &Matcher, src: &[u8]) -> Result<Vec<RefRec>> {
    let query = &matcher.query;
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), src);

    let mut out = Vec::new();
    let mut ignored: Vec<Region> = Vec::new();
    while let Some(m) = matches.next() {
        if !predicates_hold(query, m, src) || !matcher.regexes_hold(m, src) {
            continue;
        }
        let mut call: Option<TsNode> = None;
        let mut member: Option<TsNode> = None;
        let mut recv: Option<TsNode> = None;
        let mut qualifier: Option<TsNode> = None;
        for cap in m.captures {
            match names[cap.index as usize] {
                "ref.call" => call = Some(cap.node),
                "ref.member" => member = Some(cap.node),
                "ref.recv" => recv = Some(cap.node),
                "ref.qualifier" => qualifier = Some(cap.node),
                "ref.ignore" => {
                    let s = span_of(cap.node);
                    ignored.push(((s.start_line, s.start_col), (s.end_line, s.end_col)));
                }
                _ => {}
            }
        }
        if let Some(n) = call {
            if let Ok(name) = n.utf8_text(src) {
                out.push(RefRec {
                    name: name.to_owned(),
                    kind: RefKind::Call,
                    site: span_of(n),
                    receiver: None,
                    qualifier: qualifier
                        .and_then(|q| q.utf8_text(src).ok())
                        .map(str::to_owned),
                });
            }
        } else if let Some(n) = member {
            if let Ok(name) = n.utf8_text(src) {
                out.push(RefRec {
                    name: name.to_owned(),
                    kind: RefKind::Member,
                    site: span_of(n),
                    receiver: Some(receiver_of(recv, src)),
                    qualifier: None,
                });
            }
        }
    }
    // merged once, then binary-searched per ref: the linear scan was O(refs ×
    // ignored), and an Elixir module that is mostly @spec puts thousands in both
    let ignored = merge_regions(ignored);
    out.retain(|r| {
        let at = (r.site.start_line, r.site.start_col);
        !is_ignored_at(&ignored, at)
    });
    Ok(out)
}

type Region = ((u32, u32), (u32, u32));

/// Sort and fuse ignored regions into disjoint ones, so membership is a single
/// binary search rather than a scan. Query patterns match independently, so the
/// same region can be captured twice and one can sit inside another.
fn merge_regions(mut regions: Vec<Region>) -> Vec<Region> {
    regions.sort_unstable();
    let mut out: Vec<Region> = Vec::with_capacity(regions.len());
    for (start, end) in regions {
        match out.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => out.push((start, end)),
        }
    }
    out
}

/// Is `at` inside any ignored region? `regions` must come from `merge_regions`:
/// disjoint and sorted, so only the nearest region starting at or before `at` can
/// contain it.
fn is_ignored_at(regions: &[Region], at: (u32, u32)) -> bool {
    let after = regions.partition_point(|&(start, _)| start <= at);
    after > 0 && at <= regions[after - 1].1
}

fn extract_bindings(tree: &Tree, matcher: &Matcher, src: &[u8]) -> Result<Vec<BindRec>> {
    let query = &matcher.query;
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), src);

    let mut out = Vec::new();
    while let Some(m) = matches.next() {
        if !predicates_hold(query, m, src) || !matcher.regexes_hold(m, src) {
            continue;
        }
        let mut name = None;
        let mut ty = None;
        let mut site = None;
        for cap in m.captures {
            let text = cap.node.utf8_text(src).unwrap_or("").to_owned();
            match names[cap.index as usize] {
                "bind.name" => {
                    site = Some(span_of(cap.node));
                    name = Some(text);
                }
                "bind.ctor" | "bind.type" => ty = Some(text),
                _ => {}
            }
        }
        // an untyped binding (a plain `const x = …` / bare parameter) still records
        // the declared name — with an empty type — so a local can be seen to shadow
        // an import. Call resolution prefers a non-empty type for the same name.
        if let (Some(name), Some(site)) = (name, site) {
            out.push(BindRec {
                name,
                type_name: ty.unwrap_or_default(),
                site,
            });
        }
    }
    Ok(out)
}

fn receiver_of(node: Option<TsNode>, src: &[u8]) -> Receiver {
    let Some(node) = node else {
        return Receiver::Other;
    };
    match node.kind() {
        "this" => Receiver::This,
        // `self`/`cls` are the self-reference convention in Python, Rust, Swift and
        // others the way `this` (its own node kind) is in JS/TS — the receiver is the
        // enclosing type's instance. Classifying them as `This` scopes `self.method()`
        // to the enclosing class instead of spraying across every same-named method in
        // sibling classes (found dogfooding a Python OOP codebase).
        "identifier" => match node.utf8_text(src).unwrap_or("") {
            "self" | "cls" => Receiver::This,
            other => Receiver::Ident(other.to_owned()),
        },
        "new_expression" => match node
            .child_by_field_name("constructor")
            .and_then(|c| c.utf8_text(src).ok())
        {
            Some(name) => Receiver::New(name.to_owned()),
            None => Receiver::Other,
        },
        // A pure dotted chain of identifiers (`os.path`, `a.b`) is treated as one
        // named receiver so a deep-chain member call like `os.path.join()` can
        // bind to the module imported as `os.path` (Python `import os.path`). A
        // chain broken by a call/subscript/`this` is not pinnable, so it stays
        // `Other` and falls back to candidate resolution.
        "attribute" => match dotted_ident_chain(node, src) {
            Some(text) => Receiver::Ident(text),
            None => Receiver::Other,
        },
        _ => Receiver::Other,
    }
}

/// The full dotted text of a receiver that is a pure chain of identifiers
/// (`os.path`, `a.b.c`), or `None` if any link is a call/subscript/other. Only
/// the Python `attribute` node reaches here; other grammars spell member access
/// differently and are unaffected.
fn dotted_ident_chain(node: TsNode, src: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => node.utf8_text(src).ok().map(str::to_owned),
        "attribute" => {
            let object = node.child_by_field_name("object")?;
            let attribute = node.child_by_field_name("attribute")?;
            let base = dotted_ident_chain(object, src)?;
            let name = attribute.utf8_text(src).ok()?;
            Some(format!("{base}.{name}"))
        }
        _ => None,
    }
}

/// Evaluate a match's general predicates (`#eq?`, `#not-eq?`, `#any-of?`,
/// `#not-any-of?`). Required for grammars like Elixir where a `def`/`defmodule`
/// is a plain `call` node distinguished only by a predicate on the target.
/// `#match?`/regex predicates are not yet supported and are treated as passing.
fn predicates_hold(query: &Query, m: &QueryMatch, src: &[u8]) -> bool {
    let arg_text = |arg: &QueryPredicateArg| -> Option<String> {
        match arg {
            QueryPredicateArg::String(s) => Some(s.to_string()),
            QueryPredicateArg::Capture(id) => m
                .captures
                .iter()
                .find(|c| c.index == *id)
                .and_then(|c| c.node.utf8_text(src).ok())
                .map(str::to_owned),
        }
    };
    for pred in query.general_predicates(m.pattern_index) {
        match &*pred.operator {
            "eq?" | "not-eq?" => {
                if let [a, b] = &pred.args[..] {
                    let eq = arg_text(a).is_some() && arg_text(a) == arg_text(b);
                    if eq != (&*pred.operator == "eq?") {
                        return false;
                    }
                }
            }
            "any-of?" | "not-any-of?" => {
                if let [QueryPredicateArg::Capture(id), rest @ ..] = &pred.args[..] {
                    let v = m
                        .captures
                        .iter()
                        .find(|c| c.index == *id)
                        .and_then(|c| c.node.utf8_text(src).ok());
                    let found = rest.iter().any(
                        |a| matches!(a, QueryPredicateArg::String(s) if Some(s.as_ref()) == v),
                    );
                    if found != (&*pred.operator == "any-of?") {
                        return false;
                    }
                }
            }
            _ => {} // match?/not-match? (regex) — treat as passing for now
        }
    }
    true
}

fn span_of(node: TsNode) -> Span {
    let s = node.start_position();
    let e = node.end_position();
    Span {
        start_line: s.row as u32 + 1,
        start_col: s.column as u32 + 1,
        end_line: e.row as u32 + 1,
        end_col: e.column as u32 + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::{is_ignored_at, merge_regions};

    #[test]
    fn merged_ignore_regions_answer_containment() {
        // captured twice, and one nested inside another — both happen because query
        // patterns match independently
        let merged = merge_regions(vec![
            ((10, 1), (10, 40)),
            ((3, 1), (5, 9)),
            ((3, 1), (5, 9)),
            ((4, 1), (4, 8)),
        ]);
        assert_eq!(merged, vec![((3, 1), (5, 9)), ((10, 1), (10, 40))]);

        assert!(is_ignored_at(&merged, (3, 1)), "the start is inside");
        assert!(is_ignored_at(&merged, (4, 2)), "a nested hit is inside");
        assert!(is_ignored_at(&merged, (5, 9)), "the end is inside");
        assert!(!is_ignored_at(&merged, (2, 9)), "before every region");
        assert!(!is_ignored_at(&merged, (5, 10)), "just past the end");
        assert!(!is_ignored_at(&merged, (9, 1)), "between regions");
        assert!(!is_ignored_at(&merged, (11, 1)), "after every region");
        assert!(!is_ignored_at(&[], (1, 1)));
    }

    use super::*;
    use ir::NodeKind;

    fn ts() -> lang::typescript::Adapter {
        lang::typescript::Adapter::new()
    }

    #[test]
    fn extracts_definition_kinds() {
        let nodes = extract(
            "export function greet() {}\n\
             const helper = (x: number) => x;\n\
             export class Widget { increment() {} }\n\
             export interface Options {}\n\
             export type Id = string;\n\
             export enum Color { Red }\n",
            &ts(),
        )
        .unwrap();
        let names: Vec<_> = nodes.iter().map(|n| (n.kind, n.name.as_str())).collect();
        assert!(names.contains(&(NodeKind::Function, "greet")));
        assert!(names.contains(&(NodeKind::Variable, "helper")));
        assert!(names.contains(&(NodeKind::Class, "Widget")));
        assert!(names.contains(&(NodeKind::Method, "increment")));
        assert!(names.contains(&(NodeKind::Interface, "Options")));
        assert!(names.contains(&(NodeKind::Type, "Id")));
        assert!(names.contains(&(NodeKind::Enum, "Color")));
    }

    /// An adapter can contribute a def that no query produces — one named by the
    /// file, with no defining AST node (a Svelte/Vue single-file component). The
    /// synthetic def flows through `extract_file` alongside the query-captured ones.
    #[test]
    fn synthetic_defs_reach_the_extract() {
        struct StubSfc;
        impl lang::LanguageAdapter for StubSfc {
            fn id(&self) -> &'static str {
                "stub-sfc"
            }
            fn grammar(&self) -> tree_sitter::Language {
                ts().grammar()
            }
            fn file_globs(&self) -> &'static [&'static str] {
                &["*.stub"]
            }
            fn tags_query(&self) -> &'static str {
                "" // the component is not a capture; it comes from synthetic_defs
            }
            fn synthetic_defs(
                &self,
                module_path: &str,
                _root: tree_sitter::Node,
                _src: &[u8],
            ) -> Vec<ir::Node> {
                let name = std::path::Path::new(module_path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(module_path)
                    .to_owned();
                vec![ir::Node {
                    id: ir::SymbolId::of(module_path, &name),
                    kind: NodeKind::Component,
                    name: name.clone(),
                    qualified_name: name,
                    module_path: module_path.to_owned(),
                    span: ir::Span {
                        start_line: 1,
                        start_col: 1,
                        end_line: 1,
                        end_col: 1,
                    },
                    extra_spans: Vec::new(),
                    is_exported: true,
                    risk: ir::RiskScores::default(),
                    doc: None,
                    route_path: None,
                }]
            }
        }

        let queries = Queries::compile(&StubSfc).unwrap();
        let fx = extract_file(
            "const x = 1;\n",
            &StubSfc,
            "src/Widget.stub",
            &queries,
            None,
        )
        .unwrap();
        let comp: Vec<_> = fx
            .defs
            .iter()
            .filter(|n| n.kind == NodeKind::Component)
            .collect();
        assert_eq!(comp.len(), 1, "the file-named component");
        assert_eq!(comp[0].name, "Widget");
        assert!(comp[0].is_exported, "a default import must resolve to it");
    }

    /// A module-scope `const` is a symbol; a function-local one is not. Capturing
    /// every `const` at any depth put test-block temporaries (`const cache = …`)
    /// into the graph and ranked them in `review` (#68). The arrow-function pattern
    /// stays depth-free, so a local callable still counts.
    #[test]
    fn only_module_scope_plain_consts_are_captured() {
        let nodes = extract(
            "export const config = { a: 1 };\n\
             const topLevel = makeThing();\n\
             function run() {\n\
               const local = makeThing();\n\
               const handler = () => local;\n\
               return handler(config, topLevel);\n\
             }\n",
            &ts(),
        )
        .unwrap();
        let names: Vec<_> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"config"), "exported module const kept");
        assert!(names.contains(&"topLevel"), "top-level const kept");
        assert!(names.contains(&"run"));
        assert!(
            !names.contains(&"local"),
            "a function-local plain const must not be a symbol"
        );
        assert!(
            names.contains(&"handler"),
            "a local arrow function is still a callable worth capturing"
        );
    }

    /// A module-scope array destructure binds each element as its own symbol — the
    /// React `const [count, setCount] = useState()` / Solid `createSignal` shape.
    /// Dogfooding a SolidJS app found every such binding invisible, so a call to a
    /// signal getter/setter dropped its edge.
    #[test]
    fn module_scope_array_destructure_binds_each_element() {
        let nodes = extract(
            "export const [now, setNow] = createSignal(0);\n\
             const [state, setState] = createStore({});\n\
             function comp() {\n\
               const [local, setLocal] = useState(0);\n\
               return local;\n\
             }\n",
            &ts(),
        )
        .unwrap();
        let names: Vec<_> = nodes.iter().map(|n| n.name.as_str()).collect();
        for expected in ["now", "setNow", "state", "setState"] {
            assert!(names.contains(&expected), "{expected} should be a symbol");
        }
        // a function-local destructure stays local, like any other function-local const
        assert!(
            !names.contains(&"local"),
            "function-local destructure is not a symbol"
        );
        assert!(!names.contains(&"setLocal"));
    }

    #[test]
    fn captures_a_leading_doc_comment() {
        let nodes = extract(
            "// limits repeated login attempts\n\
             export function guard() {}\n",
            &ts(),
        )
        .unwrap();
        let guard = nodes.iter().find(|n| n.name == "guard").unwrap();
        assert_eq!(guard.doc.as_deref(), Some("limits repeated login attempts"));
    }

    #[test]
    fn a_trailing_comment_is_not_taken_as_a_doc() {
        // the comment belongs to the statement above, not to `next`
        let nodes = extract(
            "const rate = 5; // requests per second\n\
             export function next() {}\n",
            &ts(),
        )
        .unwrap();
        let next = nodes.iter().find(|n| n.name == "next").unwrap();
        assert_eq!(next.doc, None);
    }

    #[test]
    fn a_doc_is_seen_through_an_attribute() {
        // the Rust attribute sits between the doc and the fn; the doc must survive it
        let rust = lang::rust::Adapter::new();
        let nodes = extract(
            "/// handles the login route\n\
             #[tracing::instrument]\n\
             pub fn login() {}\n",
            &rust,
        )
        .unwrap();
        let login = nodes.iter().find(|n| n.name == "login").unwrap();
        assert_eq!(login.doc.as_deref(), Some("handles the login route"));
    }

    #[test]
    fn methods_are_qualified_by_class() {
        let nodes = extract("class A { foo() {} }\nclass B { foo() {} }\n", &ts()).unwrap();
        let qns: Vec<_> = nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Method)
            .map(|n| n.qualified_name.as_str())
            .collect();
        assert!(qns.contains(&"A.foo"));
        assert!(qns.contains(&"B.foo"));
        // distinct ids despite same method name
        let a = nodes.iter().find(|n| n.qualified_name == "A.foo").unwrap();
        let b = nodes.iter().find(|n| n.qualified_name == "B.foo").unwrap();
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn extracts_member_receivers_and_bindings() {
        let adapter = ts();
        let queries = Queries::compile(&adapter).unwrap();
        let fx = extract_file(
            "class Bar { foo() {} }\n\
             function run(b: Bar) {\n\
               const c = new Bar();\n\
               b.foo();\n\
               c.foo();\n\
               new Bar().foo();\n\
               this.foo();\n\
             }\n",
            &adapter,
            "m.ts",
            &queries,
            None,
        )
        .unwrap();
        let members: Vec<_> = fx
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::Member)
            .map(|r| (r.name.as_str(), r.receiver.clone().unwrap()))
            .collect();
        assert!(members.contains(&("foo", Receiver::Ident("b".into()))));
        assert!(members.contains(&("foo", Receiver::New("Bar".into()))));
        assert!(members.contains(&("foo", Receiver::This)));
        assert!(fx
            .bindings
            .iter()
            .any(|b| b.name == "b" && b.type_name == "Bar"));
        assert!(fx
            .bindings
            .iter()
            .any(|b| b.name == "c" && b.type_name == "Bar"));
    }

    /// Rust unit tests sit in the file they test, so no path can see them. This is
    /// the only thing standing between #36's fix and "every Rust symbol is
    /// untested" — and an over-capture would be worse than none, so a plain `mod`
    /// and a `cfg` naming something else must not match.
    #[test]
    fn a_cfg_test_module_is_a_test_scope_and_a_plain_module_is_not() {
        let adapter = lang::rust::Adapter::new();
        let queries = Queries::compile(&adapter).unwrap();
        let scopes = |src: &str| {
            extract_file(src, &adapter, "lib.rs", &queries, None)
                .unwrap()
                .test_scopes
                .len()
        };

        assert_eq!(scopes("#[cfg(test)]\nmod tests { fn t() {} }\n"), 1);
        // the attribute is a preceding sibling, and anything may sit between it
        // and the module — an anchored query pattern missed exactly this
        assert_eq!(
            scopes("#[cfg(test)]\n#[allow(clippy::all)]\nmod tests { fn t() {} }\n"),
            1,
            "a second attribute must not hide the gate"
        );
        assert_eq!(
            scopes("#[cfg(all(test, feature = \"x\"))]\nmod tests { fn t() {} }\n"),
            1,
            "cfg(all(test, …)) is still a test gate"
        );
        assert_eq!(
            scopes("mod helpers { fn h() {} }\n"),
            0,
            "a plain mod is not"
        );
        assert_eq!(
            scopes("#[cfg(feature = \"testing\")]\nmod t { fn h() {} }\n"),
            0,
            "`test` must be a whole token, not a substring of a feature name"
        );

        let fx = extract_file(
            "pub fn real() {}\n\
             #[cfg(test)]\n\
             #[allow(unused)]\n\
             mod tests {\n\
                 #[test]\n\
                 fn covers_real() {}\n\
             }\n",
            &adapter,
            "lib.rs",
            &queries,
            None,
        )
        .unwrap();
        let scope = &fx.test_scopes[0];
        let inside = |name: &str| {
            let d = fx.defs.iter().find(|d| d.name == name).expect(name);
            d.span.start_line >= scope.start_line && d.span.end_line <= scope.end_line
        };
        assert!(inside("covers_real"), "the test fn is in the scope");
        assert!(!inside("real"), "the code under test is not");
    }
}
