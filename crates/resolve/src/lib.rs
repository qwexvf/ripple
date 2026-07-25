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
use lang::{LanguageAdapter, Workspace};
use parse::{CachedFile, Queries, Receiver, RefKind};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

mod crossservice;
mod workspace;

pub use crossservice::{link_cross_service, CrossEdges};

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
/// extracts (unchanged files skip the parse). Each root is resolved
/// independently (imports never cross repos by file path — that's cross-service,
/// v1 #5); module paths are namespaced by a per-root tag so same-relative-path
/// files in different repos don't collide on SymbolId. See docs/08-roadmap.md.
pub fn build_incremental(
    roots: &[PathBuf],
    cached: &HashMap<String, CachedFile>,
) -> Result<Indexed> {
    let registry = lang::registry();
    // compile queries once, shared across all roots + rayon threads
    let queries: HashMap<&str, Queries> = registry
        .iter()
        .map(|a| Ok((a.id(), Queries::compile(a.as_ref())?)))
        .collect::<Result<_>>()?;

    let single = roots.len() == 1;
    let mut tags = HashMap::new();
    let mut all_files: Vec<CachedFile> = Vec::new();
    let mut all_nodes: Vec<Node> = Vec::new();
    let mut all_edges: Vec<Edge> = Vec::new();
    let mut used_roots: Vec<(String, PathBuf)> = Vec::new();
    let mut stats = IndexStats::default();
    // dedup files across roots (a root nested inside another would double-index)
    let mut seen_canon: HashSet<PathBuf> = HashSet::new();

    for root in roots {
        let root = root
            .canonicalize()
            .with_context(|| format!("cannot access {}", root.display()))?;
        let tag = if single {
            String::new()
        } else {
            unique_tag(&root, &mut tags)
        };

        let (files, s) = discover(&root, &tag, &registry, &queries, cached, &mut seen_canon)?;
        let ws = workspace::discover(&root);
        let (index, mut nodes) = index_defs(&files);
        let mut edges = link(&files, &index, &registry, &ws);

        stats.added += s.added;
        stats.changed += s.changed;
        stats.unchanged += s.unchanged;
        all_nodes.append(&mut nodes);
        all_edges.append(&mut edges);
        all_files.extend(files);
        used_roots.push((tag, root));
    }

    // removed = cached entries no longer present in any root
    let seen: HashSet<&str> = all_files.iter().map(|f| f.module_path.as_str()).collect();
    stats.removed = cached.keys().filter(|k| !seen.contains(k.as_str())).count();

    Ok(Indexed {
        result: BuildResult {
            files_indexed: all_files.len(),
            nodes: all_nodes,
            edges: all_edges,
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
    let mut candidates: Vec<(PathBuf, String)> = Vec::new();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !is_ignored_dir(e))
    {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if lang::adapter_for(registry, path).is_none() {
            continue;
        }
        let Ok(canonical) = path.canonicalize() else {
            continue;
        };
        if !seen_canon.insert(canonical.clone()) {
            continue; // already indexed under an earlier (more specific) root
        }
        let module_path = namespace(tag, &rel_module_path(root, &canonical));
        candidates.push((canonical, module_path));
    }

    let parsed: Vec<(CachedFile, Change)> = candidates
        .par_iter()
        .map(|(canonical, module_path)| {
            parse_one(registry, queries, cached, canonical, module_path)
        })
        .collect::<Result<Vec<_>>>()?;

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
    let hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    let parse = || {
        let q = queries
            .get(adapter.id())
            .context("missing compiled queries for adapter")?;
        parse::extract_file(&source, adapter, module_path, q)
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

/// Lookup tables built once from all definitions.
#[derive(Default)]
struct DefIndex {
    /// (file, exported name) → symbol
    export_table: HashMap<(PathBuf, String), SymbolId>,
    /// file → (name → symbols)
    file_defs: HashMap<PathBuf, HashMap<String, Vec<SymbolId>>>,
    /// file → exported symbols (for the single-default-export heuristic)
    file_exports: HashMap<PathBuf, Vec<SymbolId>>,
    /// (class, method) → symbols
    methods_by_class: HashMap<(String, String), Vec<SymbolId>>,
    /// method → symbols (candidate fallback)
    methods_by_name: HashMap<String, Vec<SymbolId>>,
    /// (owner, name) → symbols, for qualified calls. The owner is whatever the
    /// language puts before the name in a qualified name: `Client` for Rust's
    /// `Client::new`, `A` for TypeScript's `A.foo`.
    by_owner: HashMap<(String, String), Vec<SymbolId>>,
    /// name → every definition with that name, anywhere. Only consulted for a
    /// qualified call whose owner didn't match, and only when few enough
    /// candidates remain to mean something.
    by_name: HashMap<String, Vec<SymbolId>>,
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

/// Phase 2: emit def + module nodes and build the lookup tables.
fn index_defs(files: &[CachedFile]) -> (DefIndex, Vec<Node>) {
    let mut idx = DefIndex::default();
    let mut nodes = Vec::new();

    for f in files {
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
            }
            if d.kind == NodeKind::Method {
                if let Some((class, method)) = d.qualified_name.split_once('.') {
                    idx.methods_by_class
                        .entry((class.to_owned(), method.to_owned()))
                        .or_default()
                        .push(d.id);
                    idx.methods_by_name
                        .entry(method.to_owned())
                        .or_default()
                        .push(d.id);
                }
            }
            idx.by_name.entry(d.name.clone()).or_default().push(d.id);
            // an owner is anything the qualified name carries in front of the name
            if d.qualified_name.len() > d.name.len() && d.qualified_name.ends_with(&d.name) {
                let owner =
                    last_segment(&d.qualified_name[..d.qualified_name.len() - d.name.len()]);
                if !owner.is_empty() {
                    idx.by_owner
                        .entry((owner.to_owned(), d.name.clone()))
                        .or_default()
                        .push(d.id);
                }
            }
            nodes.push(d.clone());
        }
        nodes.push(module_node(&f.module_path));
    }
    (idx, nodes)
}

/// Phase 3: resolve imports and calls into edges.
fn link(
    files: &[CachedFile],
    idx: &DefIndex,
    registry: &[Box<dyn LanguageAdapter>],
    ws: &Workspace,
) -> Vec<Edge> {
    let mut edges = Vec::new();
    for f in files {
        let module_id = module_symbol(&f.module_path);
        let bindings = resolve_imports(f, idx, registry, ws, &mut edges);
        resolve_calls(f, idx, module_id, &bindings, &mut edges);
    }
    edges
}

/// Resolve a file's imports to symbols, emit Imports edges, and return the
/// local-name → symbol binding map used by call resolution.
fn resolve_imports(
    f: &CachedFile,
    idx: &DefIndex,
    registry: &[Box<dyn LanguageAdapter>],
    ws: &Workspace,
    edges: &mut Vec<Edge>,
) -> HashMap<String, SymbolId> {
    let module_id = module_symbol(&f.module_path);
    let adapter = lang::adapter_for(registry, &f.canonical);
    let mut bindings = HashMap::new();

    for imp in &f.extract.imports {
        let Some(adapter) = adapter else { continue };
        let Some(target) = adapter.resolve_import(&imp.specifier, &f.canonical, ws) else {
            continue; // bare/unresolved specifier (external node_modules dep)
        };
        let resolved = if imp.imported_name == "default" {
            default_export(idx, &target)
        } else {
            idx.export_table
                .get(&(target, imp.imported_name.clone()))
                .copied()
        };
        if let Some(sym) = resolved {
            bindings.insert(imp.local_name.clone(), sym);
            edges.push(Edge {
                src: module_id,
                dst: sym,
                kind: EdgeKind::Imports,
                confidence: CONF_IMPORT,
                site: imp.site,
                source: EdgeSource::Extracted,
            });
        }
    }
    bindings
}

/// Resolve a file's call sites (bare + member) into Calls edges.
fn resolve_calls(
    f: &CachedFile,
    idx: &DefIndex,
    module_id: SymbolId,
    bindings: &HashMap<String, SymbolId>,
    edges: &mut Vec<Edge>,
) {
    // local identifier → class-name map (M2 approximation: file-wide, last wins)
    let type_map: HashMap<&str, &str> = f
        .extract
        .bindings
        .iter()
        .map(|b| (b.name.as_str(), b.type_name.as_str()))
        .collect();

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

        let (targets, base_conf) = match r.kind {
            RefKind::Call => match resolve_qualified(&r.name, r.qualifier.as_deref(), local, idx) {
                // an explicit qualifier names its target, so it decides — including
                // deciding that nothing here matches. Consulting same-file names
                // first made `Client::new()` resolve to whatever local `new` existed.
                Some(resolved) => resolved,
                None => match local.get(&r.name) {
                    Some(ids) if !ids.is_empty() => (ids.clone(), CONF_LOCAL_CALL),
                    _ => (
                        bindings.get(&r.name).into_iter().copied().collect(),
                        CONF_LOCAL_CALL,
                    ),
                },
            },
            RefKind::Member => {
                resolve_member(&r.name, r.receiver.as_ref(), enclosing, &type_map, idx)
            }
        };

        // several definitions can share one id — Elixir's multi-clause functions
        // and default args all key to (module, name). They are one target, so
        // dedup before counting or the confidence is diluted by a phantom
        // ambiguity and the same edge is emitted once per clause.
        let mut targets = targets;
        targets.sort_unstable();
        targets.dedup();

        let n = targets.len() as f32;
        for t in targets.into_iter().filter(|&t| t != src_id) {
            edges.push(Edge {
                src: src_id,
                dst: t,
                kind: EdgeKind::Calls,
                confidence: if n <= 1.0 { base_conf } else { base_conf / n },
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
) -> Option<(Vec<SymbolId>, f32)> {
    let qualifier = qualifier?;
    let owner = last_segment(qualifier);
    if let Some(ids) = idx.by_owner.get(&(owner.to_owned(), name.to_owned())) {
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
    Some(match idx.by_name.get(name) {
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
) -> (Vec<SymbolId>, f32) {
    let candidates = || idx.methods_by_name.get(method).cloned().unwrap_or_default();
    let in_class = |class: &str| {
        idx.methods_by_class
            .get(&(class.to_owned(), method.to_owned()))
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
        is_exported: false,
        risk: ir::RiskScores::default(),
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
        .find(|d| contains(d.span, site))
        .copied()
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
