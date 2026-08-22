//! Cross-file resolution: walk a project, extract each file, then link imports
//! and calls (bare + member) into an IR node/edge set. See docs/v0-plan.md.
//!
//! Three phases: `discover` (parse every file), `index_defs` (build lookup
//! tables), `link` (resolve imports + calls). M2 member-call resolution is
//! shallow-but-honest (no full type inference): `this.` / `new X().` / typed-
//! `ident.` resolve to a class method; otherwise all methods of that name
//! become candidate edges at confidence 1/N.

use anyhow::{Context, Result};
use ir::{Edge, EdgeKind, EdgeSource, Node, NodeKind, Span, SymbolId};
use lang::LanguageAdapter;
use parse::{CachedFile, Queries, Receiver, RefKind};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

mod crossservice;
mod routes;
mod testlink;
mod workspace;

use workspace::Workspaces;

pub use crossservice::{link_cross_service, CrossEdges};
pub use routes::{Quality, RouteIndex};
pub use testlink::{link_tests, TestScopes};

/// Directories that hold code nobody is going to change in this repo:
/// dependencies, build output, and tool caches. Indexing them is not just waste —
/// it drowns the graph. On a real Elixir umbrella, `deps/` held 2176 source files
/// against 762 of the project's own, so three quarters of the call graph belonged
/// to third-party libraries.
///
/// Matched by directory name, not gitignore rules: generated sources that a repo
/// ignores (GraphQL documents, protobuf output) are exactly the code cross-service
/// resolution depends on, so "ignored by git" is the wrong test.
const IGNORED_DIRS: &[&str] = &[
    // dependencies
    "node_modules",
    "deps",
    "vendor",
    "site-packages",
    ".venv",
    "venv",
    // build output
    "_build",
    "dist",
    "build",
    "out",
    "target",
    ".next",
    // caches, tooling, coverage
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".tox",
    "coverage",
    "cover",
    ".git",
    ".ripple",
    ".dexter",
    ".elixir_ls",
    ".lexical",
    ".expert",
    ".bsp",
    ".claude",
];

// Edge confidences, ordered by how much syntax pins the target (see docs/06).
const CONF_IMPORT: f32 = 0.95; // resolved import → exported symbol
const CONF_LOCAL_CALL: f32 = 0.95; // call to an in-scope local/imported def
const CONF_KNOWN_RECEIVER: f32 = 0.9; // this.m() / new X().m() → X.m
const CONF_TYPED_RECEIVER: f32 = 0.85; // typed param x: X → x.m() → X.m
const CONF_CANDIDATE: f32 = 0.6; // by-name member fallback (before the 1/N split)
const CONF_QUALIFIED_OWNER: f32 = 0.9; // `Client::new()` → a `new` defined on `Client`
const CONF_QUALIFIED_NAME: f32 = 0.75; // `resolve::link()` → a `link` defined elsewhere
/// Above this many same-named candidates for a path call, the answer is noise
/// rather than a blast radius (`Vec::new`-style names match everywhere).
const MAX_PATH_CANDIDATES: usize = 4;

pub struct BuildResult {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub files_indexed: usize,
}

/// Per-run change accounting (vs the previous index's cache).
#[derive(Debug, Default, Clone, Copy)]
pub struct IndexStats {
    pub added: usize,
    pub changed: usize,
    pub unchanged: usize,
    pub removed: usize,
}

/// Full incremental result: the graph, the per-file cache to persist, and stats.
pub struct Indexed {
    pub result: BuildResult,
    pub files: Vec<CachedFile>,
    /// (tag, canonical root) per indexed root — for per-root git overlay.
    pub roots: Vec<(String, PathBuf)>,
    pub stats: IndexStats,
}

/// Build from scratch (no cache), single root.
pub fn build(root: &Path) -> Result<BuildResult> {
    Ok(build_incremental(std::slice::from_ref(&root.to_path_buf()), &HashMap::new())?.result)
}

/// Build one or more roots into a single graph, reusing `cached` per-file
/// extracts (unchanged files skip the parse).
///
/// Discovery is per root (its own tag, its own relative paths); resolution is not.
/// One `index_defs`/`link` pass over every file is what lets an import name a
/// package that lives in another indexed repo — the frontend's
/// `import "@org/api-client"` landing on the backend's source. What stays inside a
/// root is name-guessed resolution (see `DefIndex`) and tsconfig context (see
/// `Workspaces`), so adding a second repo cannot change the first one's edges.
///
/// Module paths are namespaced by the per-root tag, so same-relative-path files in
/// different repos don't collide on SymbolId. See docs/08-roadmap.md.
pub fn build_incremental(
    roots: &[PathBuf],
    cached: &HashMap<String, CachedFile>,
) -> Result<Indexed> {
    let registry = lang::registry();
    // compile queries once, shared across all roots + rayon threads
    let queries: HashMap<&str, Queries> = ir::timing::step("compile_queries", || {
        registry
            .iter()
            .map(|a| Ok((a.id(), Queries::compile(a.as_ref())?)))
            .collect::<Result<_>>()
    })?;

    let single = roots.len() == 1;
    let mut tags = HashMap::new();
    let mut all_files: Vec<CachedFile> = Vec::new();
    // which root each file in `all_files` came from, same order
    let mut file_root: Vec<usize> = Vec::new();
    let mut used_roots: Vec<(String, PathBuf)> = Vec::new();
    let mut stats = IndexStats::default();
    // dedup files across roots (a root nested inside another would double-index)
    let mut seen_canon: HashSet<PathBuf> = HashSet::new();

    let canonical: Vec<PathBuf> = roots
        .iter()
        .map(|root| {
            root.canonicalize()
                .with_context(|| format!("cannot access {}", root.display()))
        })
        .collect::<Result<_>>()?;
    for root in &canonical {
        let tag = if single {
            String::new()
        } else {
            unique_tag(root, &mut tags)
        };
        used_roots.push((tag, root.clone()));
    }

    // Discover deepest root first, so a root nested inside another claims its own
    // files. `seen_canon` gives a file to whoever reaches it first, and doing that
    // in argv order made `index /repo /repo/api` and `index /repo/api /repo` produce
    // different module paths — and therefore different SymbolIds — for the same
    // file. Tags and reporting keep the order the caller gave.
    let mut order: Vec<usize> = (0..canonical.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(canonical[i].as_os_str().len()));

    let mut per_root: Vec<Vec<CachedFile>> = vec![Vec::new(); canonical.len()];
    let discover_span = ir::timing::start("discover+parse");
    let mut discovered = 0usize;
    for &i in &order {
        let (files, s) = discover(
            &canonical[i],
            &used_roots[i].0,
            &registry,
            &queries,
            cached,
            &mut seen_canon,
        )?;
        stats.added += s.added;
        stats.changed += s.changed;
        stats.unchanged += s.unchanged;
        discovered += files.len();
        per_root[i] = files;
    }
    discover_span.stop(discovered);
    // reassembled in the caller's order, so file order — and therefore edge order —
    // does not depend on how deep the roots happen to be
    for (i, files) in per_root.into_iter().enumerate() {
        file_root.extend(std::iter::repeat_n(i, files.len()));
        all_files.extend(files);
    }

    let ws = ir::timing::step("discover_workspaces", || {
        Workspaces::discover_all(&used_roots)
    });
    let idx_span = ir::timing::start("index_defs");
    let (index, mut nodes) = index_defs(&all_files, &file_root);
    idx_span.stop(nodes.len());

    let link_span = ir::timing::start("link");
    let (edges, external_nodes) = link(&all_files, &file_root, &index, &registry, &ws);
    link_span.stop(edges.len());
    // external-import binding mints nodes for out-of-root symbols during `link`
    nodes.extend(external_nodes);

    // removed = cached entries no longer present in any root
    let seen: HashSet<&str> = all_files.iter().map(|f| f.module_path.as_str()).collect();
    stats.removed = cached.keys().filter(|k| !seen.contains(k.as_str())).count();

    Ok(Indexed {
        result: BuildResult {
            files_indexed: all_files.len(),
            nodes,
            edges,
        },
        files: all_files,
        roots: used_roots,
        stats,
    })
}

/// Namespace a repo-relative path by its root tag (empty tag = no prefix).
pub fn namespace(tag: &str, rel: &str) -> String {
    if tag.is_empty() {
        rel.to_owned()
    } else {
        format!("{tag}/{rel}")
    }
}

/// A unique, stable tag for a root (its dir name, de-duped with a suffix).
fn unique_tag(root: &Path, seen: &mut HashMap<String, u32>) -> String {
    let base = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo")
        .to_owned();
    let n = seen.entry(base.clone()).or_insert(0);
    *n += 1;
    if *n == 1 {
        base
    } else {
        format!("{base}~{n}")
    }
}

/// Phase 1 (per root): find supported files; reuse cached extract on hash match,
/// else re-parse in parallel. `module_path` is namespaced by `tag`.
fn discover(
    root: &Path,
    tag: &str,
    registry: &[Box<dyn LanguageAdapter>],
    queries: &HashMap<&str, Queries>,
    cached: &HashMap<String, CachedFile>,
    seen_canon: &mut HashSet<PathBuf>,
) -> Result<(Vec<CachedFile>, IndexStats)> {
    let specs = lang::spec::registry();
    let mut candidates: Vec<(PathBuf, String)> = Vec::new();
    // files that carry boundary facts without being code (an OpenAPI document).
    // Kept apart from `candidates`: they have no grammar, so nothing below the
    // cross facts applies to them — no defs, no refs, no adapter.
    let mut spec_files: Vec<(PathBuf, String)> = Vec::new();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !is_ignored_dir(e))
    {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let is_code = lang::adapter_for(registry, path).is_some();
        let is_spec = !is_code && lang::spec::detector_for(&specs, path).is_some();
        if !is_code && !is_spec {
            continue;
        }
        let Ok(canonical) = path.canonicalize() else {
            continue;
        };
        if !seen_canon.insert(canonical.clone()) {
            continue; // already indexed under an earlier (more specific) root
        }
        let module_path = namespace(tag, &rel_module_path(root, &canonical));
        if is_spec {
            spec_files.push((canonical, module_path));
        } else {
            candidates.push((canonical, module_path));
        }
    }
    // WalkDir hands back readdir order, which differs between machines and between
    // runs after a rewrite. File order is edge order, and the store keys edges by
    // insertion index, so this is what makes two indexes of one tree comparable.
    candidates.sort();

    spec_files.sort();
    let mut parsed: Vec<(CachedFile, Change)> = candidates
        .par_iter()
        .map(|(canonical, module_path)| {
            parse_one(registry, queries, cached, canonical, module_path)
        })
        .collect::<Result<Vec<_>>>()?;

    parsed.extend(
        spec_files
            .par_iter()
            .filter_map(|(canonical, module_path)| {
                read_spec(&specs, cached, canonical, module_path)
            })
            .collect::<Vec<_>>(),
    );

    let mut stats = IndexStats::default();
    let mut files = Vec::with_capacity(parsed.len());
    for (file, change) in parsed {
        match change {
            Change::Added => stats.added += 1,
            Change::Changed => stats.changed += 1,
            Change::Unchanged => stats.unchanged += 1,
        }
        files.push(file);
    }
    Ok((files, stats))
}

enum Change {
    Added,
    Changed,
    Unchanged,
}

/// A spec file's facts, cached on content hash exactly as a parsed file's are.
///
/// `None` when the text is not the kind of document its extension allows — a repo
/// holds far more YAML than it holds API descriptions, and the alternative is
/// parsing all of it.
fn read_spec(
    specs: &[Box<dyn lang::spec::SpecDetector>],
    cached: &HashMap<String, CachedFile>,
    canonical: &Path,
    module_path: &str,
) -> Option<(CachedFile, Change)> {
    let detector = lang::spec::detector_for(specs, canonical)?;
    let source = std::fs::read_to_string(canonical).ok()?;
    if !detector.looks_like_one(&source) {
        return None;
    }
    let hash = parse::content_hash(&source);
    let (extract, change) = match cached.get(module_path) {
        Some(c) if c.hash == hash => (c.extract.clone(), Change::Unchanged),
        Some(_) => (spec_extract(detector, &source), Change::Changed),
        None => (spec_extract(detector, &source), Change::Added),
    };
    Some((
        CachedFile {
            canonical: canonical.to_owned(),
            module_path: module_path.to_owned(),
            hash,
            extract,
        },
        change,
    ))
}

fn spec_extract(detector: &dyn lang::spec::SpecDetector, source: &str) -> parse::FileExtract {
    parse::FileExtract {
        cross: detector.facts(source),
        ..Default::default()
    }
}

fn parse_one(
    registry: &[Box<dyn LanguageAdapter>],
    queries: &HashMap<&str, Queries>,
    cached: &HashMap<String, CachedFile>,
    canonical: &Path,
    module_path: &str,
) -> Result<(CachedFile, Change)> {
    let adapter = lang::adapter_for(registry, canonical)
        .context("adapter vanished between discovery and parse")?;
    let source = std::fs::read_to_string(canonical)
        .with_context(|| format!("cannot read {}", canonical.display()))?;
    let hash = parse::content_hash(&source);

    let parse = || {
        let q = queries
            .get(adapter.id())
            .context("missing compiled queries for adapter")?;
        // a file may embed another language (a `.vue`/`.html` <script>); hand the
        // extractor every adapter + its queries so it can parse those regions too
        let embed = parse::EmbedCtx { registry, queries };
        parse::extract_file(&source, adapter, module_path, q, Some(&embed))
    };
    let (extract, change) = match cached.get(module_path) {
        Some(c) if c.hash == hash => (c.extract.clone(), Change::Unchanged),
        Some(_) => (parse()?, Change::Changed),
        None => (parse()?, Change::Added),
    };
    Ok((
        CachedFile {
            canonical: canonical.to_owned(),
            module_path: module_path.to_owned(),
            hash,
            extract,
        },
        change,
    ))
}

/// Lookup tables built once from all definitions, across every root.
///
/// The split down the middle is the rule that makes a multi-repo index safe:
/// **path-evidenced lookups cross roots, name-guessed ones don't.** An import that
/// resolved to a file landed on *that* file, wherever it lives. A bare identifier
/// matching in another repo is not evidence of anything — and letting it match
/// would change repo A's edges whenever repo B is added to the index, both by
/// diluting 1/N confidences and by pushing candidate sets past
/// `MAX_PATH_CANDIDATES` until an edge disappears.
#[derive(Default)]
struct DefIndex {
    /// (file, exported name) → symbol
    export_table: HashMap<(PathBuf, String), SymbolId>,
    /// file → (name → symbols)
    file_defs: HashMap<PathBuf, HashMap<String, Vec<SymbolId>>>,
    /// file → exported symbols (for the single-default-export heuristic)
    file_exports: HashMap<PathBuf, Vec<SymbolId>>,
    /// (package directory, exported name) → symbols. A Go package is a directory of
    /// files sharing one namespace, so a `config.Foo()` call resolves against every
    /// file in `internal/config/`, not one. Keyed by the file's parent dir. See #85.
    pkg_exports: HashMap<(PathBuf, String), Vec<SymbolId>>,
    /// (root, class, method) → symbols
    methods_by_class: HashMap<(usize, String, String), Vec<SymbolId>>,
    /// (root, method) → symbols (candidate fallback)
    methods_by_name: HashMap<(usize, String), Vec<SymbolId>>,
    /// (root, owner, name) → symbols, for qualified calls. The owner is whatever the
    /// language puts before the name in a qualified name: `Client` for Rust's
    /// `Client::new`, `A` for TypeScript's `A.foo`.
    by_owner: HashMap<(usize, String, String), Vec<SymbolId>>,
    /// (root, name) → every definition with that name in that root. Only consulted
    /// for a qualified call whose owner didn't match, and only when few enough
    /// candidates remain to mean something.
    by_name: HashMap<(usize, String), Vec<SymbolId>>,
    /// which root each symbol came from, so an edge that leaves one can be priced
    /// for it
    root_of: HashMap<SymbolId, usize>,
    /// symbols a bare call can never name. A struct field and a getter routinely
    /// share a name (`Config.path` and `fn path(&self)`), so once fields became
    /// symbols every such call split its confidence across both — 26 call edges in
    /// this repo alone dropped from 0.95 to 0.475 against a field that cannot be
    /// invoked. Kept as the exclusion rather than a callable allow-list so a kind
    /// added later is a candidate until someone rules it out, which is the
    /// direction that fails loudly.
    uncallable: HashSet<SymbolId>,
}

/// The trailing identifier of a qualified name or path — `Client` from
/// `crate::store::Client`, `A` from `A.foo`. Both separators appear across the
/// languages ripple indexes, and only the last segment is ever the owner.
fn last_segment(path: &str) -> &str {
    // trailing separators first: an owner is derived by cutting the name off a
    // qualified name, which leaves `Client::` — and splitting that yields nothing
    let path = path.trim().trim_end_matches([':', '.']);
    path.rsplit([':', '.']).next().unwrap_or(path).trim()
}

/// Phase 2: emit def + module nodes and build the lookup tables. `file_root[i]` is
/// which indexed root `files[i]` came from — one entry per file, in the same order.
fn index_defs(files: &[CachedFile], file_root: &[usize]) -> (DefIndex, Vec<Node>) {
    let mut idx = DefIndex::default();
    let mut nodes: Vec<Node> = Vec::new();
    // identity is (path, qualified name), so several definitions of one symbol share
    // an id — clauses, overloads, a reopened class. They used to be pushed as
    // separate nodes and then silently overwrite each other in the store's id-keyed
    // table, losing every definition site but the last. Collapse here instead, and
    // keep the spans.
    let mut at: HashMap<SymbolId, usize> = HashMap::new();
    // a field and a function can collapse onto one id; that id is still callable
    let mut callable: HashSet<SymbolId> = HashSet::new();

    for (f, &root) in files.iter().zip(file_root) {
        let by_name = idx.file_defs.entry(f.canonical.clone()).or_default();
        for d in &f.extract.defs {
            by_name.entry(d.name.clone()).or_default().push(d.id);
            if d.is_exported {
                idx.export_table
                    .insert((f.canonical.clone(), d.name.clone()), d.id);
                idx.file_exports
                    .entry(f.canonical.clone())
                    .or_default()
                    .push(d.id);
                if let Some(dir) = f.canonical.parent() {
                    idx.pkg_exports
                        .entry((dir.to_path_buf(), d.name.clone()))
                        .or_default()
                        .push(d.id);
                }
            }
            if d.kind == NodeKind::Method {
                if let Some((class, method)) = d.qualified_name.split_once('.') {
                    idx.methods_by_class
                        .entry((root, class.to_owned(), method.to_owned()))
                        .or_default()
                        .push(d.id);
                    idx.methods_by_name
                        .entry((root, method.to_owned()))
                        .or_default()
                        .push(d.id);
                }
            }
            idx.by_name
                .entry((root, d.name.clone()))
                .or_default()
                .push(d.id);
            if d.kind == NodeKind::Field {
                idx.uncallable.insert(d.id);
            } else {
                callable.insert(d.id);
            }
            idx.root_of.insert(d.id, root);
            // an owner is anything the qualified name carries in front of the name
            if d.qualified_name.len() > d.name.len() && d.qualified_name.ends_with(&d.name) {
                let owner =
                    last_segment(&d.qualified_name[..d.qualified_name.len() - d.name.len()]);
                if !owner.is_empty() {
                    idx.by_owner
                        .entry((root, owner.to_owned(), d.name.clone()))
                        .or_default()
                        .push(d.id);
                }
            }
            match at.get(&d.id) {
                Some(&i) => {
                    let node: &mut Node = &mut nodes[i];
                    if !node.definition_spans().any(|s| s == d.span) {
                        node.extra_spans.push(d.span);
                    }
                    // `const f = () => …` matches both the variable pattern and the
                    // function one, and which arrives first is a property of the
                    // query engine rather than of the language. The more specific
                    // kind wins, so a bound function is a function (#53).
                    if node.kind == NodeKind::Variable && d.kind == NodeKind::Function {
                        node.kind = NodeKind::Function;
                    }
                }
                None => {
                    at.insert(d.id, nodes.len());
                    nodes.push(d.clone());
                }
            }
        }
        let module = module_node(&f.module_path);
        idx.root_of.insert(module.id, root);
        nodes.push(module);
    }
    idx.uncallable.retain(|id| !callable.contains(id));
    (idx, nodes)
}

/// Phase 3: resolve imports and calls into edges, over every root at once.
///
/// One pass rather than one per root: an import that names a package declared in
/// another indexed repo resolves to a file that only a shared `by_path` knows
/// about. What stays per-root is the *guessing* — see `DefIndex` — and the
/// tsconfig context, which `Workspaces` picks per file.
fn link(
    files: &[CachedFile],
    file_root: &[usize],
    idx: &DefIndex,
    registry: &[Box<dyn LanguageAdapter>],
    ws: &Workspaces,
) -> (Vec<Edge>, Vec<Node>) {
    let mut sink = LinkSink::default();
    // a barrel file has to be looked up by path when an import lands on it
    let by_path: HashMap<&Path, &CachedFile> =
        files.iter().map(|f| (f.canonical.as_path(), f)).collect();
    for (f, &root) in files.iter().zip(file_root) {
        let module_id = module_symbol(&f.module_path);
        let scope = resolve_imports(f, root, idx, &by_path, registry, ws, &mut sink);
        resolve_calls(f, root, idx, module_id, &scope, &mut sink);
    }
    // deterministic node order: edge order already is, and the store keys by id,
    // but a stable node list keeps two indexes of one tree comparable
    let mut external_nodes: Vec<Node> = sink.externals.into_values().collect();
    external_nodes.sort_by_key(|n| n.id.0);
    (sink.edges, external_nodes)
}

/// The two accumulators `link` fills as it resolves each file: the edge list and
/// the deduped set of `External` nodes minted by the binding pass. Bundled so
/// `resolve_imports` stays under the argument limit.
#[derive(Default)]
struct LinkSink {
    edges: Vec<Edge>,
    externals: HashMap<SymbolId, Node>,
}

impl LinkSink {
    /// Mint (or reuse) an external `dep.symbol` node and return its id. Deduped
    /// by id, so every reference to the same external symbol shares one node.
    fn external_symbol(&mut self, dep: &str, symbol: &str) -> SymbolId {
        let sym_id = external_symbol_id(dep, symbol);
        let qn = format!("{dep}.{symbol}");
        self.externals
            .entry(sym_id)
            .or_insert_with(|| external_node(sym_id, symbol, &qn, dep));
        sym_id
    }
}

/// What a file's imports bind, for resolving its call sites. Names bound to a
/// project symbol, to a local module file (namespace import), or to an external
/// dependency key (namespace / plain `import pkg`).
#[derive(Default)]
struct ImportScope {
    /// local name → the single project symbol it imports
    bindings: HashMap<String, SymbolId>,
    /// local name → the local file it names as a whole (`import * as ns`)
    modules: HashMap<String, PathBuf>,
    /// local name → external dep-key, for a namespace/plain import that resolved
    /// outside the roots (`import * as React from "react"`, `import os`). A member
    /// call on such a name binds to `dep.method`.
    ext_modules: HashMap<String, String>,
}

/// Id of the external module (dep-key) node. Kept in sync with
/// [`external_module_node`] so `link` and any consumer agree on identity.
fn external_module_id(dep: &str) -> SymbolId {
    SymbolId::of(dep, dep)
}

/// Id of an external `dep.symbol` node.
fn external_symbol_id(dep: &str, symbol: &str) -> SymbolId {
    SymbolId::of(dep, &format!("{dep}.{symbol}"))
}

fn external_node(id: SymbolId, name: &str, qualified_name: &str, dep: &str) -> Node {
    Node {
        id,
        kind: NodeKind::External,
        name: name.to_owned(),
        qualified_name: qualified_name.to_owned(),
        module_path: dep.to_owned(),
        span: Span {
            start_line: 0,
            start_col: 0,
            end_line: 0,
            end_col: 0,
        },
        extra_spans: Vec::new(),
        is_exported: false,
        risk: ir::RiskScores::default(),
        doc: None,
        route_path: None,
    }
}

/// Bind a bare import that resolved outside the indexed roots to `External`
/// nodes, minting the dep's module node and an `Imports` edge to it (the
/// import-level floor) for every kind of import, then:
/// - side-effect (`import "polyfill"`): nothing more — no name is bound;
/// - namespace / plain (`import * as ns`, `import os`): record name → dep-key in
///   `ext_modules`, so a later `ns.f()` / `os.f()` member call binds `dep.f`;
/// - named / default: mint the `dep.symbol` node and bind the local name, so a
///   later call to it becomes a `Calls` edge.
fn bind_external(
    adapter: &dyn LanguageAdapter,
    imp: &parse::ImportRec,
    file_module_id: SymbolId,
    scope: &mut ImportScope,
    sink: &mut LinkSink,
) {
    let Some(dep) = adapter.external_dep_key(&imp.specifier) else {
        return; // relative/unresolvable specifier that is not an external package
    };
    let mod_id = external_module_id(&dep);
    sink.externals
        .entry(mod_id)
        .or_insert_with(|| external_node(mod_id, &dep, &dep, &dep));
    sink.edges.push(Edge {
        src: file_module_id,
        dst: mod_id,
        kind: EdgeKind::Imports,
        confidence: CONF_IMPORT,
        site: imp.site,
        source: EdgeSource::Extracted,
    });
    // a side-effect import (`import "polyfill"`) binds nothing — the module node
    // and its import edge are the whole story.
    if imp.is_side_effect() {
        return;
    }
    // a namespace / plain module import (`import * as React`, `import os`) binds the
    // whole module to one name. Record the name → dep-key so a later `React.f()` /
    // `os.f()` member call binds to the external `dep.f` symbol (see resolve_calls).
    if imp.is_namespace() {
        scope.ext_modules.insert(imp.local_name.clone(), dep);
        return;
    }
    let sym_id = sink.external_symbol(&dep, &imp.imported_name);
    scope.bindings.insert(imp.local_name.clone(), sym_id);
}

/// How much a resolution that leaves its repo is worth. The syntax pins the target
/// exactly as well as it does in-repo; what is weaker is the premise underneath —
/// that these two working trees are one program. A consumer usually resolves a
/// published artifact, so the file on disk is the right symbol at a version nobody
/// checked. Multiplied into the edge's own confidence, so an already-ambiguous
/// 1/N call stays proportionally weak (invariant 5).
const CROSS_ROOT: f32 = 0.85;

/// Price an edge for the boundary it crosses, if it crosses one.
fn cross_root_conf(idx: &DefIndex, from_root: usize, dst: SymbolId, conf: f32) -> f32 {
    match idx.root_of.get(&dst) {
        Some(&r) if r != from_root => conf * CROSS_ROOT,
        _ => conf,
    }
}

/// How many re-export hops to follow. Barrels nest (a package index re-exporting
/// feature indexes), but not deeply, and a cycle must not cost anything.
const MAX_REEXPORT_HOPS: usize = 4;

/// The symbol a file exposes under `name`, following `export … from` chains.
///
/// A barrel defines nothing: `import { getFragmentData } from "@/generated/graphql"`
/// lands on an `index.ts` whose entire content is `export * from "./masking"`. Without
/// following that, the import resolves to nothing and every consumer edge is lost —
/// 693 of them on one real app, all through a single generated barrel (issue #27).
fn resolve_export(
    idx: &DefIndex,
    by_path: &HashMap<&Path, &CachedFile>,
    registry: &[Box<dyn LanguageAdapter>],
    ws: &Workspaces,
    file: &Path,
    name: &str,
    hops: usize,
) -> Option<SymbolId> {
    if let Some(&sym) = idx.export_table.get(&(file.to_path_buf(), name.to_owned())) {
        return Some(sym);
    }
    if hops == 0 {
        return None;
    }
    let barrel = by_path.get(file)?;
    let adapter = lang::adapter_for(registry, file)?;
    for re in &barrel.extract.reexports {
        // `export { a as b } from` exposes `b`; the source file knows it as `a`.
        // `export * from` passes every name through unchanged.
        let source_name = match re.exposed_as.as_str() {
            "*" => name,
            exposed if exposed == name => re.name.as_str(),
            _ => continue,
        };
        // the barrel's own context, not the importer's: a chain can hop into
        // another root, where a different tsconfig applies
        let Some(next) = adapter.resolve_import(&re.specifier, file, ws.for_file(file)) else {
            continue;
        };
        // `next` differs from `file` for any real re-export, so a chain shortens the
        // hop budget and a cycle terminates
        if let Some(sym) = resolve_export(idx, by_path, registry, ws, &next, source_name, hops - 1)
        {
            return Some(sym);
        }
    }
    None
}

/// Resolve a file's imports to symbols, emit Imports edges, and return the
/// import scope (local name → symbol / local module / external dep) used by call
/// resolution.
fn resolve_imports(
    f: &CachedFile,
    root: usize,
    idx: &DefIndex,
    by_path: &HashMap<&Path, &CachedFile>,
    registry: &[Box<dyn LanguageAdapter>],
    ws: &Workspaces,
    sink: &mut LinkSink,
) -> ImportScope {
    let module_id = module_symbol(&f.module_path);
    let adapter = lang::adapter_for(registry, &f.canonical);
    let mut scope = ImportScope::default();

    for imp in &f.extract.imports {
        let Some(adapter) = adapter else { continue };
        let Some(target) =
            adapter.resolve_import(&imp.specifier, &f.canonical, ws.for_file(&f.canonical))
        else {
            // bare/unresolved specifier — bind it as an external package so a
            // call to it has a real target node (external-import binding pass)
            bind_external(adapter, imp, module_id, &mut scope, sink);
            continue;
        };
        if imp.is_namespace() {
            // the binding names a module, so there is no single symbol to point at;
            // the file's module node is the honest target
            scope.modules.insert(imp.local_name.clone(), target.clone());
            if let Some(m) = by_path.get(target.as_path()) {
                let dst = module_symbol(&m.module_path);
                sink.edges.push(Edge {
                    src: module_id,
                    dst,
                    kind: EdgeKind::Imports,
                    confidence: cross_root_conf(idx, root, dst, CONF_IMPORT),
                    site: imp.site,
                    source: EdgeSource::Extracted,
                });
            }
            continue;
        }
        let resolved = if imp.imported_name == "default" {
            default_export(idx, &target)
        } else {
            resolve_export(
                idx,
                by_path,
                registry,
                ws,
                &target,
                &imp.imported_name,
                MAX_REEXPORT_HOPS,
            )
        };
        if let Some(sym) = resolved {
            scope.bindings.insert(imp.local_name.clone(), sym);
            sink.edges.push(Edge {
                src: module_id,
                dst: sym,
                kind: EdgeKind::Imports,
                confidence: cross_root_conf(idx, root, sym, CONF_IMPORT),
                site: imp.site,
                source: EdgeSource::Extracted,
            });
        }
    }
    scope
}

/// Resolve a file's call sites (bare + member) into Calls edges.
fn resolve_calls(
    f: &CachedFile,
    root: usize,
    idx: &DefIndex,
    module_id: SymbolId,
    scope: &ImportScope,
    sink: &mut LinkSink,
) {
    let types = Bindings::new(&f.extract.bindings);

    let empty = HashMap::new();
    let local = idx.file_defs.get(&f.canonical).unwrap_or(&empty);

    // defs sorted by start position → O(log n + siblings) enclosing lookup per ref
    let mut defs_by_start: Vec<&Node> = f.extract.defs.iter().collect();
    defs_by_start.sort_by_key(|d| (d.span.start_line, d.span.start_col));

    for r in &f.extract.refs {
        let enclosing = enclosing_def(&defs_by_start, r.site);
        let src_id = enclosing.map_or(module_id, |n| n.id);

        // In languages where a definition is itself a call (Elixir's `def f(x)`),
        // the definition's own name parses as a call in its header. Such a ref
        // names the definition, so drop it — otherwise every function gains a
        // self-edge, and every multi-clause function links its clauses together.
        //
        // Unqualified calls only: a member call is never a definition header, and
        // treating it as one silently dropped `class A { foo() { b.foo(); } }`,
        // where the real call to `B.foo` shares a line with `A.foo`'s definition.
        let is_def_header = r.kind == RefKind::Call
            && enclosing
                .is_some_and(|d| d.name == r.name && r.site.start_line == d.span.start_line);
        if is_def_header {
            continue;
        }

        let (mut targets, base_conf) = match r.kind {
            RefKind::Call => {
                match resolve_qualified(&r.name, r.qualifier.as_deref(), local, idx, root) {
                    // an explicit qualifier names its target, so it decides — including
                    // deciding that nothing here matches. Consulting same-file names
                    // first made `Client::new()` resolve to whatever local `new` existed.
                    Some(resolved) => resolved,
                    None => match local.get(&r.name) {
                        Some(ids) if !ids.is_empty() => (ids.clone(), CONF_LOCAL_CALL),
                        _ => (
                            scope.bindings.get(&r.name).into_iter().copied().collect(),
                            CONF_LOCAL_CALL,
                        ),
                    },
                }
            }
            RefKind::Member => {
                let type_map = types.at(r.site, enclosing, &defs_by_start);
                // a local declaration of the receiver name shadows any import of the
                // same name: `const React = makeFake(); React.foo()` is not the
                // imported `React`, so it must not bind to an external/module symbol.
                let shadowed = matches!(
                    r.receiver.as_ref(),
                    Some(Receiver::Ident(v)) if type_map.contains_key(v.as_str())
                );
                let ext_dep = if shadowed {
                    None
                } else {
                    external_module_receiver(r.receiver.as_ref(), &scope.ext_modules)
                };
                // a receiver bound to a whole local module is pinned by the import,
                // so the method is whatever that module exports under this name. The
                // target is a single file (a TS `import * as ns`) or a package
                // directory (a Go import); try the file export table, then the
                // directory-keyed package exports (#85).
                if let Some(target) = module_receiver(r.receiver.as_ref(), &scope.modules) {
                    if let Some(sym) = idx.export_table.get(&(target.clone(), r.name.clone())) {
                        (vec![*sym], CONF_KNOWN_RECEIVER)
                    } else if let Some(syms) = idx.pkg_exports.get(&(target, r.name.clone())) {
                        (syms.clone(), CONF_KNOWN_RECEIVER)
                    } else {
                        (Vec::new(), CONF_KNOWN_RECEIVER)
                    }
                } else if let Some(dep) = ext_dep {
                    // `React.useState()` / `os.system()` — the receiver names an
                    // external module, so the method is that dep's `dep.method` symbol
                    let sym = sink.external_symbol(&dep, &r.name);
                    (vec![sym], CONF_KNOWN_RECEIVER)
                } else {
                    resolve_member(
                        &r.name,
                        r.receiver.as_ref(),
                        enclosing,
                        &type_map,
                        idx,
                        root,
                    )
                }
            }
        };

        // `plan(…)` names the function `plan`, never the struct field beside it.
        // Dropping these before the count matters as much as the edge itself: a
        // candidate that cannot be called still divides the 1/N confidence.
        if r.kind == RefKind::Call {
            targets.retain(|id| !idx.uncallable.contains(id));
        }

        // several definitions can share one id — Elixir's multi-clause functions
        // and default args all key to (module, name). They are one target, so
        // dedup before counting or the confidence is diluted by a phantom
        // ambiguity and the same edge is emitted once per clause.
        targets.sort_unstable();
        targets.dedup();

        let n = targets.len() as f32;
        for t in targets.into_iter().filter(|&t| t != src_id) {
            let conf = if n <= 1.0 { base_conf } else { base_conf / n };
            sink.edges.push(Edge {
                src: src_id,
                dst: t,
                kind: EdgeKind::Calls,
                confidence: cross_root_conf(idx, root, t, conf),
                site: r.site,
                source: EdgeSource::Extracted,
            });
        }
    }
}

/// Qualified-call resolution ladder. Returns (targets, base confidence).
///
/// A path call is the normal way one Rust module calls another, so resolving only
/// same-file names left `impact` blind on any Rust project — it reported zero
/// dependents for functions used across crates.
///
/// The qualifier is what makes this safe: `Client::new` prefers a `new` defined on
/// `Client`, so it can't drag in every other `new`. Only when no owner matches does
/// it fall back to the name alone, and then only if the candidates are few enough to
/// be worth showing.
/// `None` means "no qualifier, no opinion" — fall back to scope. `Some(empty)` is a
/// verdict: the qualifier named something this graph doesn't contain.
fn resolve_qualified(
    name: &str,
    qualifier: Option<&str>,
    local: &HashMap<String, Vec<SymbolId>>,
    idx: &DefIndex,
    root: usize,
) -> Option<(Vec<SymbolId>, f32)> {
    let qualifier = qualifier?;
    let owner = last_segment(qualifier);
    if let Some(ids) = idx.by_owner.get(&(root, owner.to_owned(), name.to_owned())) {
        return Some((prefer_local(ids, name, local), CONF_QUALIFIED_OWNER));
    }
    // A capitalized qualifier names a *type*, and the type is either ours or it
    // isn't: `Vec::new()` and `HashMap::new()` must resolve to nothing rather than
    // to whatever `new` happens to exist. Falling back on the bare name linked
    // every collection constructor in the repo to an unrelated `Adapter::new`.
    if owner.starts_with(char::is_uppercase) {
        return Some((Vec::new(), CONF_QUALIFIED_OWNER));
    }
    // A lowercase qualifier is a module or crate path (`resolve::link_cross_service`),
    // where the name alone is the only handle we have.
    Some(match idx.by_name.get(&(root, name.to_owned())) {
        Some(ids) if ids.len() <= MAX_PATH_CANDIDATES => {
            (prefer_local(ids, name, local), CONF_QUALIFIED_NAME)
        }
        _ => (Vec::new(), CONF_QUALIFIED_NAME),
    })
}

/// Candidates defined in the calling file, if any. Several files can define the
/// same qualified name — ripple itself has four `Adapter::new` — and inside one of
/// them the call plainly means its own.
fn prefer_local(
    candidates: &[SymbolId],
    name: &str,
    local: &HashMap<String, Vec<SymbolId>>,
) -> Vec<SymbolId> {
    let here: Vec<SymbolId> = local
        .get(name)
        .map(|ids| {
            ids.iter()
                .filter(|id| candidates.contains(id))
                .copied()
                .collect()
        })
        .unwrap_or_default();
    if here.is_empty() {
        candidates.to_vec()
    } else {
        here
    }
}

/// Member-call resolution ladder. Returns (targets, base confidence).
fn resolve_member(
    method: &str,
    receiver: Option<&Receiver>,
    enclosing: Option<&Node>,
    type_map: &HashMap<&str, &str>,
    idx: &DefIndex,
    root: usize,
) -> (Vec<SymbolId>, f32) {
    let candidates = || {
        idx.methods_by_name
            .get(&(root, method.to_owned()))
            .cloned()
            .unwrap_or_default()
    };
    let in_class = |class: &str| {
        idx.methods_by_class
            .get(&(root, class.to_owned(), method.to_owned()))
            .cloned()
            .unwrap_or_default()
    };
    // resolve to a class, else fall back to candidate methods by name
    let by_class = |class: &str, conf: f32| {
        let t = in_class(class);
        if t.is_empty() {
            (candidates(), CONF_CANDIDATE)
        } else {
            (t, conf)
        }
    };

    match receiver {
        Some(Receiver::This) => match enclosing.and_then(class_of) {
            Some(class) => by_class(class, CONF_KNOWN_RECEIVER),
            None => (candidates(), CONF_CANDIDATE),
        },
        Some(Receiver::New(ctor)) => by_class(ctor, CONF_KNOWN_RECEIVER),
        Some(Receiver::Ident(v)) => match type_map.get(v.as_str()) {
            Some(class) => by_class(class, CONF_TYPED_RECEIVER),
            None => (candidates(), CONF_CANDIDATE),
        },
        _ => (candidates(), CONF_CANDIDATE),
    }
}

/// A file's identifier → type bindings, answered by position rather than file-wide.
///
/// `type_map` used to be one flat map per file, last declaration wins. Two functions
/// binding the same name to different types is ordinary code (`const client = new
/// AdminClient()` in one, `new UserClient()` in the next), and the flat map sent every
/// `client.foo()` in the file to whichever came last.
struct Bindings<'a> {
    /// sorted by declaration position, so the nearest preceding one is findable
    by_site: Vec<&'a parse::BindRec>,
}

impl<'a> Bindings<'a> {
    fn new(records: &'a [parse::BindRec]) -> Bindings<'a> {
        let mut by_site: Vec<&parse::BindRec> = records.iter().collect();
        by_site.sort_by_key(|b| (b.site.start_line, b.site.start_col));
        Bindings { by_site }
    }

    /// The bindings visible at `site`: those declared inside the enclosing definition
    /// before it, plus file-level ones outside every definition.
    ///
    /// Nearest-preceding wins, which is the closest thing to scope that spans alone can
    /// express — no language knowledge, and shadowing inside a block resolves the same
    /// way a reader would resolve it.
    /// `defs` is the file's definitions, so a binding inside *another* function can be
    /// told apart from one at module level — otherwise one function's local leaks into
    /// another's calls, which is the bug this replaces.
    fn at(
        &self,
        site: Span,
        enclosing: Option<&Node>,
        defs: &[&Node],
    ) -> HashMap<&'a str, &'a str> {
        // name → (declaration site, type). One declarator is captured twice — once
        // typed (`const b: Bar`/`new Bar()`), once bare — at the same site; the bare
        // capture carries an empty type. Prefer the non-empty one at a given site so
        // an untyped record never downgrades a known type, while a genuinely later
        // (nearest-preceding) redeclaration still shadows an earlier one.
        let mut out: HashMap<&str, (Span, &str)> = HashMap::new();
        for b in &self.by_site {
            if (b.site.start_line, b.site.start_col) > (site.start_line, site.start_col) {
                break; // sorted: nothing further can precede the reference
            }
            let line = b.site.start_line;
            let visible = match enclosing {
                // inside the same definition, or at module level (inside none)
                Some(d) => d.contains_line(line) || !defs.iter().any(|x| x.contains_line(line)),
                None => !defs.iter().any(|x| x.contains_line(line)),
            };
            if !visible {
                continue;
            }
            match out.get(b.name.as_str()) {
                // same declarator seen twice: keep whichever pins a type
                Some((esite, etype)) if *esite == b.site && !etype.is_empty() => {}
                _ => {
                    out.insert(b.name.as_str(), (b.site, b.type_name.as_str()));
                }
            }
        }
        out.into_iter().map(|(k, (_, ty))| (k, ty)).collect()
    }
}

/// The file a member call's receiver names, when the receiver is a namespace import.
fn module_receiver(
    receiver: Option<&Receiver>,
    modules: &HashMap<String, PathBuf>,
) -> Option<PathBuf> {
    match receiver {
        Some(Receiver::Ident(name)) => modules.get(name.as_str()).cloned(),
        _ => None,
    }
}

/// The external dep-key a member call's receiver names, when the receiver is an
/// external namespace/plain module import (`React.f()` → `react`, `os.f()` → `os`).
fn external_module_receiver(
    receiver: Option<&Receiver>,
    ext_modules: &HashMap<String, String>,
) -> Option<String> {
    match receiver {
        Some(Receiver::Ident(name)) => ext_modules.get(name.as_str()).cloned(),
        _ => None,
    }
}

/// Class name of a member node (`Class.method` → `Class`).
fn class_of(node: &Node) -> Option<&str> {
    if node.kind == NodeKind::Method {
        node.qualified_name.split_once('.').map(|(c, _)| c)
    } else {
        None
    }
}

fn module_symbol(module_path: &str) -> SymbolId {
    SymbolId::module(module_path)
}

fn module_node(module_path: &str) -> Node {
    Node {
        id: module_symbol(module_path),
        kind: NodeKind::Module,
        name: module_path.to_owned(),
        qualified_name: module_path.to_owned(),
        module_path: module_path.to_owned(),
        span: Span {
            start_line: 1,
            start_col: 1,
            end_line: 1,
            end_col: 1,
        },
        extra_spans: Vec::new(),
        is_exported: false,
        risk: ir::RiskScores::default(),
        doc: None,
        route_path: None,
    }
}

/// Innermost def whose span contains `site`, over defs pre-sorted by start.
/// Binary-searches to the last def starting at/before the site, then walks back
/// to the first container — O(log n + siblings) instead of O(defs) per ref.
fn enclosing_def<'a>(defs_by_start: &[&'a Node], site: Span) -> Option<&'a Node> {
    let key = (site.start_line, site.start_col);
    let upper = defs_by_start.partition_point(|d| (d.span.start_line, d.span.start_col) <= key);
    defs_by_start[..upper]
        .iter()
        .rev()
        .find(|d| is_container(d.kind) && contains(d.span, site))
        .copied()
}

/// Can a call be *made by* this kind of definition?
///
/// A `const x = f()` captures `x` as a variable whose span contains the call, and it
/// was the innermost definition, so the edge said `x` called `f` — in const-heavy
/// TypeScript that meant a reviewer read a variable name where the calling function
/// belonged (`flattenKeys ← localeKeys`, where the caller is `checkTranslations`).
/// A callable declared as a const is captured as a function, not a variable, so
/// nothing is lost by excluding value bindings.
fn is_container(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function
            | NodeKind::Method
            | NodeKind::Class
            | NodeKind::Module
            // a single-file component owns the render calls in its template — a ref
            // that falls outside every script function belongs to the component
            | NodeKind::Component
    )
}

fn contains(outer: Span, inner: Span) -> bool {
    (outer.start_line, outer.start_col) <= (inner.start_line, inner.start_col)
        && (outer.end_line, outer.end_col) >= (inner.end_line, inner.end_col)
}

fn default_export(idx: &DefIndex, target: &Path) -> Option<SymbolId> {
    match idx.file_exports.get(target) {
        Some(v) if v.len() == 1 => Some(v[0]),
        _ => None,
    }
}

pub(crate) fn is_ignored_dir(e: &walkdir::DirEntry) -> bool {
    e.file_type().is_dir()
        && e.file_name()
            .to_str()
            .is_some_and(|n| IGNORED_DIRS.contains(&n))
}

pub(crate) fn rel_module_path(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}
