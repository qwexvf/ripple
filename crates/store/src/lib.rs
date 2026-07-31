//! Persistence + in-memory graph. Per docs/04-architecture.md: the store is a
//! durable snapshot; query-time traversal runs over the in-RAM graph, never over
//! SQL/disk. v0/M1 backend: `redb` (pure-Rust KV). `SamyamaStore` lands in M3.

use anyhow::{Context, Result};
use ir::{Edge, EdgeKind, Node, SymbolId};
use parse::CachedFile;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

const NODES: TableDefinition<u64, &[u8]> = TableDefinition::new("nodes");
const EDGES: TableDefinition<u64, &[u8]> = TableDefinition::new("edges");
const EXTRACTS: TableDefinition<&str, &[u8]> = TableDefinition::new("extracts");
const ROOTS: TableDefinition<u64, &[u8]> = TableDefinition::new("roots");
/// Path + content hash per indexed file. The same facts as the head of an
/// `extracts` row, kept apart so a query can ask "is this answer still true?"
/// without deserializing every AST record in the repo.
const STAMPS: TableDefinition<&str, &[u8]> = TableDefinition::new("stamps");
/// Verified call verdicts, keyed by the *content hash* of the file they came from.
const VERIFIED: TableDefinition<&str, &[u8]> = TableDefinition::new("verified");
/// The shape of a `FileExtract` at the time the cache was written.
///
/// A cache row is keyed on the file's content hash, which does not change when the
/// *parser* does. A build whose extract gained a field therefore reads back rows the
/// current parser never produced, and a `#[serde(default)]` field makes that succeed
/// silently. Comparing the shape turns a wrong graph into a re-parse. See #56.
const SHAPE: TableDefinition<&str, &str> = TableDefinition::new("extract_shape");

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
    /// Where each indexed file was and what it hashed to, without paying for the
    /// extracts. A query that wants to know whether its answer is still true reads
    /// this; deserializing every AST record to compare two hashes is not worth it.
    fn read_file_stamps(&self) -> Result<HashMap<String, FileStamp>>;
    /// Persist the (tag, path) roots this index was built from. Commands that
    /// start from a filesystem path need the tag to namespace it the same way
    /// indexing did — without it, a multi-root graph looks empty to them.
    fn write_roots(&mut self, roots: &[(String, PathBuf)]) -> Result<()>;
    /// The roots of the last index; empty if none were recorded.
    fn read_roots(&self) -> Result<Vec<(String, PathBuf)>>;
    /// Merge verified verdicts into the cache, keyed by file content hash.
    fn write_verified(&mut self, by_hash: &HashMap<String, Vec<VerifiedCall>>) -> Result<()>;
    /// Every cached verdict set, keyed by file content hash.
    fn read_verified(&self) -> Result<HashMap<String, Vec<VerifiedCall>>>;
}

/// How long to wait out another process's index before giving up. Long enough to
/// cover a small repo's whole index, short enough that a stuck process is reported
/// rather than waited on forever.
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
const LOCK_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// redb reports the held lock as an I/O error whose message is the only detail.
fn is_locked(e: &redb::DatabaseError) -> bool {
    matches!(e, redb::DatabaseError::DatabaseAlreadyOpen)
        || e.to_string().contains("Cannot acquire lock")
}

/// The identifying part of a `CachedFile`: where the file was and what it hashed
/// to. Deserialized from the same rows, ignoring the extract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileStamp {
    pub canonical: PathBuf,
    pub module_path: String,
    pub hash: String,
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
    ///
    /// redb holds an exclusive lock, and two indexers is a normal thing to have —
    /// the MCP `reindex` tool and a CLI run, or two agents on one repo. Waiting
    /// briefly turns a race into a pause; past that, say who to blame rather than
    /// surfacing `Database already open. Cannot acquire lock.` (#38).
    fn db(&self) -> Result<Database> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        // A graph written by an older redb cannot be opened, and there is nothing in
        // it worth migrating: indexing rewrites every table anyway, and re-parsing a
        // repo costs seconds. Delete and rebuild rather than making the user do it.
        if let Err(redb::DatabaseError::UpgradeRequired(_)) = Database::create(&self.path) {
            std::fs::remove_file(&self.path).with_context(|| {
                format!(
                    "{} was written by an older ripple and must be rebuilt, \
                     but it could not be removed",
                    self.path.display()
                )
            })?;
        }
        self.wait_for_lock(|| Database::create(&self.path), "create redb")
    }

    /// Open an existing database for reading, waiting out a held lock the same way
    /// the write path does. `None` means there is no database yet.
    ///
    /// Reading is the *more* common collision — a query while an index runs — and
    /// it used to fail instantly with redb's raw message, wrapped in "run `ripple
    /// index` first" while an index was in fact running (#38).
    fn read_db(&self) -> Result<Option<Database>> {
        if !self.path.exists() {
            return Ok(None);
        }
        match self.wait_for_lock(|| Database::open(&self.path), "open redb") {
            Ok(db) => Ok(Some(db)),
            // a query can't rebuild the graph — say what to run, not what redb said
            Err(e) if needs_rebuild(&e) => Err(e).with_context(|| {
                format!(
                    "{} was written by an older ripple — re-run `ripple index`",
                    self.path.display()
                )
            }),
            Err(e) => Err(e),
        }
    }

    /// Like `read_db`, but an unreadable old format is "nothing cached" rather than
    /// an error: the caller is the indexer, and the write path rebuilds the file.
    fn cache_db(&self) -> Result<Option<Database>> {
        match self.read_db() {
            Err(e) if needs_rebuild(&e) => Ok(None),
            other => other,
        }
    }

    fn wait_for_lock(
        &self,
        mut attempt: impl FnMut() -> std::result::Result<Database, redb::DatabaseError>,
        what: &str,
    ) -> Result<Database> {
        let mut waited = std::time::Duration::ZERO;
        loop {
            match attempt() {
                Ok(db) => return Ok(db),
                Err(e) if is_locked(&e) && waited < LOCK_WAIT => {
                    std::thread::sleep(LOCK_POLL);
                    waited += LOCK_POLL;
                }
                Err(e) if is_locked(&e) => {
                    return Err(anyhow::Error::new(e).context(format!(
                        "another ripple is using {} — waited {}s. \
                         Re-run once it finishes, or point --root elsewhere",
                        self.path.display(),
                        LOCK_WAIT.as_secs()
                    )))
                }
                Err(e) => return Err(anyhow::Error::new(e).context(what.to_owned())),
            }
        }
    }
}

/// Was this database written by an older redb, whose format this build can't read?
fn needs_rebuild(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<redb::DatabaseError>(),
        Some(redb::DatabaseError::UpgradeRequired(_))
    )
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
    let _ = wtx.delete_table(STAMPS);
    let _ = wtx.delete_table(SHAPE);
    wtx.open_table(SHAPE)?
        .insert("extract", parse::extract_shape().as_str())?;
    let mut t = wtx.open_table(EXTRACTS)?;
    let mut s = wtx.open_table(STAMPS)?;
    for f in files {
        let bytes = serde_json::to_vec(f)?;
        t.insert(f.module_path.as_str(), bytes.as_slice())?;
        let stamp = serde_json::to_vec(&FileStamp {
            canonical: f.canonical.clone(),
            module_path: f.module_path.clone(),
            hash: f.hash.clone(),
        })?;
        s.insert(f.module_path.as_str(), stamp.as_slice())?;
    }
    Ok(())
}

/// Was this cache written by a build whose extract had the shape this one produces?
///
/// A cache with no shape recorded predates the check, so it was written by a build
/// whose extract certainly differs from today's — treated as a mismatch rather than
/// trusted.
fn shape_matches(rtx: &redb::ReadTransaction) -> bool {
    let Ok(t) = rtx.open_table(SHAPE) else {
        return false;
    };
    matches!(t.get("extract"), Ok(Some(v)) if v.value() == parse::extract_shape())
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
        // an old-format file is nothing this build can read, which for a cache is the
        // same as nothing cached — `index` deletes and rebuilds it on the write side
        let Some(db) = self.cache_db()? else {
            return Ok(HashMap::new());
        };
        let rtx = db.begin_read()?;
        let mut out = HashMap::new();
        // written by a build whose extract had a different shape: every row is
        // suspect, including the ones that still deserialize (#56)
        if !shape_matches(&rtx) {
            return Ok(out);
        }
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

    fn read_file_stamps(&self) -> Result<HashMap<String, FileStamp>> {
        // an old-format file is nothing this build can read, which for a cache is the
        // same as nothing cached — `index` deletes and rebuilds it on the write side
        let Some(db) = self.cache_db()? else {
            return Ok(HashMap::new());
        };
        let rtx = db.begin_read()?;
        let mut out = HashMap::new();
        // absent for an index written before stamps existed: unknown staleness,
        // which reports nothing rather than claiming everything is fresh
        if let Ok(t) = rtx.open_table(STAMPS) {
            for row in t.iter()? {
                let (_k, v) = row?;
                let Ok(f) = serde_json::from_slice::<FileStamp>(v.value()) else {
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

    /// Merges rather than replaces: verifying one query's neighbourhood must not
    /// discard what earlier queries learned about other files.
    fn write_verified(&mut self, by_hash: &HashMap<String, Vec<VerifiedCall>>) -> Result<()> {
        let db = self.db()?;
        let wtx = db.begin_write()?;
        {
            let mut t = wtx.open_table(VERIFIED)?;
            for (hash, calls) in by_hash {
                let bytes = serde_json::to_vec(calls)?;
                t.insert(hash.as_str(), bytes.as_slice())?;
            }
        }
        wtx.commit()?;
        Ok(())
    }

    fn read_verified(&self) -> Result<HashMap<String, Vec<VerifiedCall>>> {
        // an old-format file is nothing this build can read, which for a cache is the
        // same as nothing cached — `index` deletes and rebuilds it on the write side
        let Some(db) = self.cache_db()? else {
            return Ok(HashMap::new());
        };
        let rtx = db.begin_read()?;
        let mut out = HashMap::new();
        if let Ok(t) = rtx.open_table(VERIFIED) {
            for row in t.iter()? {
                let (k, v) = row?;
                // a row from an older verdict schema is a cache miss, not a failure
                if let Ok(calls) = serde_json::from_slice::<Vec<VerifiedCall>>(v.value()) {
                    out.insert(k.value().to_owned(), calls);
                }
            }
        }
        Ok(out)
    }

    fn read_roots(&self) -> Result<Vec<(String, PathBuf)>> {
        // an old-format file is nothing this build can read, which for a cache is the
        // same as nothing cached — `index` deletes and rebuilds it on the write side
        let Some(db) = self.cache_db()? else {
            return Ok(Vec::new());
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
        let db = self.read_db()?.with_context(|| {
            format!(
                "no index at {} — run `ripple index` first",
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

/// What a language server said about one call, recorded so the same question is not
/// asked twice about a file that has not changed.
///
/// Keyed by file content hash rather than path: a file that changes gets a new key and
/// its stale verdicts are simply never read again, and a file that is renamed but not
/// edited keeps its answers. Symbol ids are stored raw — replaying a verdict against a
/// graph that no longer holds the symbol is a miss, not a lie.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    /// Both ripple and the server found this call.
    Confirmed,
    /// Only the server found it.
    Added,
    /// Ripple has it; the server, which covers that file, does not report it.
    Contradicted,
}

/// One recorded verdict: the edge `src → dst`, and what the server said about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedCall {
    pub src: SymbolId,
    pub dst: SymbolId,
    pub verdict: Verdict,
    /// Which edge kind the verdict is about. A server with no call hierarchy can
    /// only supply `References`, and replaying that as a `Calls` edge would claim
    /// more than the server said. Rows written before this field existed are
    /// calls, which is what the default preserves.
    #[serde(default = "calls")]
    pub kind: EdgeKind,
}

fn calls() -> EdgeKind {
    EdgeKind::Calls
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A cache written by a build whose extract had a different shape must be
    /// ignored whole, not row by row. The rows still deserialize — that is the
    /// point: `#[serde(default)]` makes a missing field succeed as a zero, and the
    /// graph is then built from facts the parser never produced. Two measurement
    /// runs on #54 were wrong before this existed.
    #[test]
    fn a_cache_from_a_different_extract_shape_is_not_read() {
        let path = std::env::temp_dir().join(format!("ripple-shape-{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut store = RedbStore::open(&path);
        store
            .write_extracts(&[CachedFile {
                canonical: PathBuf::from("/p/a.ts"),
                module_path: "a.ts".to_owned(),
                hash: "h".to_owned(),
                extract: parse::FileExtract::default(),
            }])
            .expect("write");
        assert_eq!(store.read_extracts().expect("read").len(), 1);

        // exactly what a parser change does: the rows are untouched and still valid
        // JSON, only the shape they were written under has moved on
        {
            let db = store.db().expect("db");
            let wtx = db.begin_write().expect("write tx");
            wtx.open_table(SHAPE)
                .expect("shape")
                .insert("extract", "a shape from some other build")
                .expect("insert");
            wtx.commit().expect("commit");
        }
        assert!(
            store.read_extracts().expect("read").is_empty(),
            "a cache whose shape does not match must be a miss, not a silent zero"
        );

        let _ = std::fs::remove_file(&path);
    }
}
