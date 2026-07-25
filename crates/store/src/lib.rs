//! Persistence + in-memory graph. Per docs/04-architecture.md: the store is a
//! durable snapshot; query-time traversal runs over the in-RAM graph, never over
//! SQL/disk. v0/M1 backend: `redb` (pure-Rust KV). `SamyamaStore` lands in M3.

use anyhow::{Context, Result};
use ir::{Edge, EdgeKind, Node, SymbolId};
use parse::CachedFile;
use redb::{Database, ReadableTable, TableDefinition};
use std::collections::HashMap;
use std::path::PathBuf;

const NODES: TableDefinition<u64, &[u8]> = TableDefinition::new("nodes");
const EDGES: TableDefinition<u64, &[u8]> = TableDefinition::new("edges");
const EXTRACTS: TableDefinition<&str, &[u8]> = TableDefinition::new("extracts");
const ROOTS: TableDefinition<u64, &[u8]> = TableDefinition::new("roots");

/// Durable graph store. One writer, many readers; query happens after `load`.
/// Also persists the per-file extract cache for incremental re-indexing.
pub trait GraphStore {
    fn write(&mut self, nodes: &[Node], edges: &[Edge]) -> Result<()>;
    /// Persist graph, extract cache and roots as one unit.
    ///
    /// Indexing produces all three from the same pass, so they must land or fail
    /// together: written separately, a crash in between leaves a graph whose cache
    /// claims files it doesn't contain, or roots that name a graph that isn't there.
    fn write_index(
        &mut self,
        nodes: &[Node],
        edges: &[Edge],
        files: &[CachedFile],
        roots: &[(String, PathBuf)],
    ) -> Result<()>;
    fn load(&self) -> Result<InMemoryGraph>;
    /// Persist the per-file extract cache (overwrites the previous one).
    fn write_extracts(&mut self, files: &[CachedFile]) -> Result<()>;
    /// Load the extract cache keyed by module path; empty if none yet.
    fn read_extracts(&self) -> Result<HashMap<String, CachedFile>>;
    /// Persist the (tag, path) roots this index was built from. Commands that
    /// start from a filesystem path need the tag to namespace it the same way
    /// indexing did — without it, a multi-root graph looks empty to them.
    fn write_roots(&mut self, roots: &[(String, PathBuf)]) -> Result<()>;
    /// The roots of the last index; empty if none were recorded.
    fn read_roots(&self) -> Result<Vec<(String, PathBuf)>>;
}

pub struct RedbStore {
    path: PathBuf,
}

impl RedbStore {
    pub fn open(path: impl Into<PathBuf>) -> Self {
        RedbStore { path: path.into() }
    }

    /// Open the database, creating the parent directory if needed. Every write path
    /// goes through this so the "create dir, then create db" pairing lives once.
    fn db(&self) -> Result<Database> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        Database::create(&self.path).context("create redb")
    }
}

/// Replace the graph tables inside an open write transaction. The extract cache and
/// roots tables are left alone.
fn put_graph(wtx: &redb::WriteTransaction, nodes: &[Node], edges: &[Edge]) -> Result<()> {
    let _ = wtx.delete_table(NODES);
    let _ = wtx.delete_table(EDGES);
    {
        let mut t = wtx.open_table(NODES)?;
        for n in nodes {
            let bytes = serde_json::to_vec(n)?;
            t.insert(n.id.0, bytes.as_slice())?;
        }
    }
    let mut t = wtx.open_table(EDGES)?;
    for (i, e) in edges.iter().enumerate() {
        let bytes = serde_json::to_vec(e)?;
        t.insert(i as u64, bytes.as_slice())?;
    }
    Ok(())
}

fn put_extracts(wtx: &redb::WriteTransaction, files: &[CachedFile]) -> Result<()> {
    let _ = wtx.delete_table(EXTRACTS);
    let mut t = wtx.open_table(EXTRACTS)?;
    for f in files {
        let bytes = serde_json::to_vec(f)?;
        t.insert(f.module_path.as_str(), bytes.as_slice())?;
    }
    Ok(())
}

fn put_roots(wtx: &redb::WriteTransaction, roots: &[(String, PathBuf)]) -> Result<()> {
    let _ = wtx.delete_table(ROOTS);
    let mut t = wtx.open_table(ROOTS)?;
    for (i, r) in roots.iter().enumerate() {
        let bytes = serde_json::to_vec(r)?;
        t.insert(i as u64, bytes.as_slice())?;
    }
    Ok(())
}

impl GraphStore for RedbStore {
    fn write(&mut self, nodes: &[Node], edges: &[Edge]) -> Result<()> {
        let db = self.db()?;
        let wtx = db.begin_write()?;
        put_graph(&wtx, nodes, edges)?;
        wtx.commit()?;
        Ok(())
    }

    fn write_index(
        &mut self,
        nodes: &[Node],
        edges: &[Edge],
        files: &[CachedFile],
        roots: &[(String, PathBuf)],
    ) -> Result<()> {
        let db = self.db()?;
        let wtx = db.begin_write()?;
        put_graph(&wtx, nodes, edges)?;
        put_extracts(&wtx, files)?;
        put_roots(&wtx, roots)?;
        wtx.commit()?;
        Ok(())
    }

    fn write_extracts(&mut self, files: &[CachedFile]) -> Result<()> {
        let db = self.db()?;
        let wtx = db.begin_write()?;
        put_extracts(&wtx, files)?;
        wtx.commit()?;
        Ok(())
    }

    fn read_extracts(&self) -> Result<HashMap<String, CachedFile>> {
        let Ok(db) = Database::open(&self.path) else {
            return Ok(HashMap::new()); // no prior index
        };
        let rtx = db.begin_read()?;
        let mut out = HashMap::new();
        if let Ok(t) = rtx.open_table(EXTRACTS) {
            for row in t.iter()? {
                let (_k, v) = row?;
                // a row written by an older extract schema is a cache miss, not a
                // failure — the file is simply re-extracted
                let Ok(f) = serde_json::from_slice::<CachedFile>(v.value()) else {
                    continue;
                };
                out.insert(f.module_path.clone(), f);
            }
        }
        Ok(out)
    }

    fn write_roots(&mut self, roots: &[(String, PathBuf)]) -> Result<()> {
        let db = self.db()?;
        let wtx = db.begin_write()?;
        put_roots(&wtx, roots)?;
        wtx.commit()?;
        Ok(())
    }

    fn read_roots(&self) -> Result<Vec<(String, PathBuf)>> {
        let Ok(db) = Database::open(&self.path) else {
            return Ok(Vec::new()); // no prior index
        };
        let rtx = db.begin_read()?;
        let mut out = Vec::new();
        if let Ok(t) = rtx.open_table(ROOTS) {
            for row in t.iter()? {
                let (_k, v) = row?;
                out.push(serde_json::from_slice(v.value())?);
            }
        }
        Ok(out)
    }

    fn load(&self) -> Result<InMemoryGraph> {
        let db = Database::open(&self.path).with_context(|| {
            format!(
                "open redb at {} (run `ripple index` first)",
                self.path.display()
            )
        })?;
        let rtx = db.begin_read()?;

        let mut nodes = Vec::new();
        if let Ok(t) = rtx.open_table(NODES) {
            for row in t.iter()? {
                let (_k, v) = row?;
                nodes.push(serde_json::from_slice::<Node>(v.value())?);
            }
        }
        let mut edges = Vec::new();
        if let Ok(t) = rtx.open_table(EDGES) {
            for row in t.iter()? {
                let (_k, v) = row?;
                edges.push(serde_json::from_slice::<Edge>(v.value())?);
            }
        }
        Ok(InMemoryGraph::from_parts(nodes, edges))
    }
}

/// How a name query found what it found. A looser match still answers, but the
/// caller should say which rule fired: the answer may not be about what was meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    Exact,
    /// The query is the trailing segment of a qualified name — `LfgPost` finding
    /// `FiveNoobs.Lfgs.LfgPost`. Elixir names *are* their FQN, so an exact-only
    /// lookup makes every module unfindable by the name a human would type.
    QualifiedSuffix,
    /// The query appears somewhere in a name. Last resort, and ambiguous by nature.
    Substring,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Out,
    In,
}

pub struct Hop {
    pub edge: Edge,
    pub node: Node,
    pub depth: usize,
}

/// In-RAM graph hydrated from a store; traversal target for `neighbors`.
pub struct InMemoryGraph {
    nodes: HashMap<SymbolId, Node>,
    out: HashMap<SymbolId, Vec<Edge>>,
    inc: HashMap<SymbolId, Vec<Edge>>,
}

impl InMemoryGraph {
    pub fn from_parts(nodes: Vec<Node>, edges: Vec<Edge>) -> Self {
        let mut node_map = HashMap::with_capacity(nodes.len());
        for n in nodes {
            node_map.insert(n.id, n);
        }
        let mut out: HashMap<SymbolId, Vec<Edge>> = HashMap::new();
        let mut inc: HashMap<SymbolId, Vec<Edge>> = HashMap::new();
        for e in edges {
            out.entry(e.src).or_default().push(e.clone());
            inc.entry(e.dst).or_default().push(e);
        }
        InMemoryGraph {
            nodes: node_map,
            out,
            inc,
        }
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Iterate all nodes.
    pub fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.nodes.values()
    }

    pub fn get(&self, id: SymbolId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    /// Every edge, once. Order is unspecified — sort if you need determinism.
    pub fn edges(&self) -> impl Iterator<Item = &Edge> {
        self.out.values().flatten()
    }

    /// Edges pointing *into* `id` (dependents — used for impact/blast-radius).
    pub fn in_edges(&self, id: SymbolId) -> &[Edge] {
        self.inc.get(&id).map_or(&[], Vec::as_slice)
    }

    /// Edges pointing *out of* `id` (dependencies).
    pub fn out_edges(&self, id: SymbolId) -> &[Edge] {
        self.out.get(&id).map_or(&[], Vec::as_slice)
    }

    /// All nodes whose module path equals `path` or ends with `/path` (a path
    /// suffix on a segment boundary, so `bar.ts` won't match `foobar.ts`).
    pub fn nodes_in_file(&self, path: &str) -> Vec<&Node> {
        let matches =
            |mp: &str| mp == path || mp.strip_suffix(path).is_some_and(|p| p.ends_with('/'));
        let mut v: Vec<&Node> = self
            .nodes
            .values()
            .filter(|n| matches(&n.module_path))
            .collect();
        v.sort_by_key(|n| (n.module_path.clone(), n.span.start_line));
        v
    }

    pub fn find_by_name(&self, name: &str) -> Vec<&Node> {
        self.matching(|n| n.name == name || n.qualified_name == name)
    }

    /// Find symbols by name, widening the rule only when the stricter one finds
    /// nothing: exact, then qualified-name suffix, then substring.
    ///
    /// Returns which rule fired so a caller can say so — a substring hit is a guess
    /// worth showing, not a silent answer. `None` when nothing matched at all.
    pub fn lookup(&self, query: &str) -> Option<(Vec<&Node>, Match)> {
        let exact = self.find_by_name(query);
        if !exact.is_empty() {
            return Some((exact, Match::Exact));
        }
        let suffix = self.matching(|n| {
            let dotted = format!(".{query}");
            n.qualified_name.ends_with(&dotted) || n.name.ends_with(&dotted)
        });
        if !suffix.is_empty() {
            return Some((suffix, Match::QualifiedSuffix));
        }
        let lower = query.to_lowercase();
        let substring = self.matching(|n| {
            n.name.to_lowercase().contains(&lower)
                || n.qualified_name.to_lowercase().contains(&lower)
        });
        (!substring.is_empty()).then_some((substring, Match::Substring))
    }

    /// Nodes satisfying `pred`, in a stable order (module path, then line).
    fn matching(&self, pred: impl Fn(&Node) -> bool) -> Vec<&Node> {
        let mut v: Vec<&Node> = self.nodes.values().filter(|n| pred(n)).collect();
        v.sort_by_key(|n| (n.module_path.clone(), n.span.start_line));
        v
    }

    /// BFS over out- or in-edges up to `depth`, optionally filtered by edge kind.
    /// Deterministic order (edges sorted by dst id, then site).
    pub fn neighbors(
        &self,
        start: SymbolId,
        dir: Dir,
        kinds: Option<&[EdgeKind]>,
        depth: usize,
    ) -> Vec<Hop> {
        let adj = match dir {
            Dir::Out => &self.out,
            Dir::In => &self.inc,
        };
        let mut seen = std::collections::HashSet::new();
        let mut frontier = vec![start];
        let mut hops = Vec::new();
        seen.insert(start);

        for d in 1..=depth {
            let mut next = Vec::new();
            for &cur in &frontier {
                let Some(es) = adj.get(&cur) else { continue };
                let mut es: Vec<&Edge> = es
                    .iter()
                    .filter(|e| kinds.is_none_or(|ks| ks.contains(&e.kind)))
                    .collect();
                es.sort_by(|a, b| {
                    pick(a, dir)
                        .0
                        .cmp(&pick(b, dir).0)
                        .then(a.site.start_line.cmp(&b.site.start_line))
                });
                for e in es {
                    let other = pick(e, dir);
                    if let Some(node) = self.nodes.get(&other) {
                        hops.push(Hop {
                            edge: e.clone(),
                            node: node.clone(),
                            depth: d,
                        });
                    }
                    if seen.insert(other) {
                        next.push(other);
                    }
                }
            }
            frontier = next;
            if frontier.is_empty() {
                break;
            }
        }
        hops
    }
}

fn pick(e: &Edge, dir: Dir) -> SymbolId {
    match dir {
        Dir::Out => e.dst,
        Dir::In => e.src,
    }
}
