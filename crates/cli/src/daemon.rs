//! `ripple daemon` — a resident, file-watching index server.
//!
//! One daemon serves many projects. The expensive fixed cost of a CLI query is
//! *startup*: compiling every adapter's tree-sitter queries (~0.8s) and loading
//! the graph. The daemon pays that once and keeps the resident graph hot, so a
//! query becomes a socket round-trip instead of a cold build.
//!
//! Three properties keep it from inflating under a machine full of repos:
//!
//! * **Demand-load + LRU eviction** — a project's graph is built on first query
//!   and dropped when it is the least-recently-used past the cap. RAM is bounded
//!   by the cap, not by how many repos are registered.
//! * **A bounded, de-duplicating work queue** — every re-index goes through one
//!   worker thread, and a project already queued is not queued again. A burst of
//!   saves collapses to a single rebuild, so CPU stays near one core no matter
//!   how many editors are firing events.
//! * **Watches scoped and filtered** — `.ripple/` and `.git/` are ignored, so the
//!   daemon's own graph writes don't trigger a rebuild loop.
//!
//! Linux/systemd first: the socket defaults under `$XDG_RUNTIME_DIR`, which a
//! systemd unit provides via `RuntimeDirectory=`. Other platforms fall back to a
//! temp path and work the same; a proper service wrapper for them comes later.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use store::{Dir, GraphStore, InMemoryGraph, RedbStore};

/// Edge count for a resident graph (the store exposes an iterator, not a count).
fn edge_count(g: &InMemoryGraph) -> usize {
    g.edges().count()
}

/// How many project graphs stay resident before the least-recently-used is
/// evicted. Each is a few MB of nodes/edges; the cap is what bounds daemon RAM.
const DEFAULT_MAX_RESIDENT: usize = 8;

/// The socket the daemon listens on and clients connect to. `RIPPLE_SOCKET`
/// overrides; otherwise `$XDG_RUNTIME_DIR/ripple/daemon.sock` (what a systemd
/// `RuntimeDirectory=ripple` unit provides), falling back to a temp path.
pub fn socket_path() -> PathBuf {
    if let Some(explicit) = std::env::var_os("RIPPLE_SOCKET") {
        return PathBuf::from(explicit);
    }
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime).join("ripple").join("daemon.sock");
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "anon".to_owned());
    std::env::temp_dir()
        .join(format!("ripple-{user}"))
        .join("daemon.sock")
}

// ── wire protocol: newline-delimited JSON ──────────────────────────────────

/// One request from a client. `root` is the project directory; the daemon
/// demand-loads it if it is not already resident.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Build (if needed) and start watching a project, without querying it.
    Register { root: String },
    /// Blast radius of a symbol.
    Impact {
        root: String,
        symbol: String,
        #[serde(default)]
        budget: Option<usize>,
    },
    /// Callers/importers (`in`) or callees (`out`) of a symbol.
    Neighbors {
        root: String,
        symbol: String,
        #[serde(default)]
        dir: Option<String>,
        #[serde(default)]
        depth: Option<usize>,
    },
    /// Which projects are resident, and their node/edge counts.
    Status,
    /// Ask the daemon to exit.
    Stop,
}

/// The daemon's reply. `data` is op-specific JSON; `error` is set when `ok` is
/// false.
#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub data: serde_json::Value,
}

impl Response {
    fn ok(data: serde_json::Value) -> Self {
        Response {
            ok: true,
            error: None,
            data,
        }
    }
    fn err(msg: impl Into<String>) -> Self {
        Response {
            ok: false,
            error: Some(msg.into()),
            data: serde_json::Value::Null,
        }
    }
}

// ── resident project + registry ────────────────────────────────────────────

/// A resident project: its graph behind an `RwLock` so a re-index can swap it in
/// while reads continue, plus a last-access stamp for LRU and its live watcher.
struct Project {
    graph: RwLock<Arc<InMemoryGraph>>,
    last_access: Mutex<Instant>,
    /// Kept alive so the OS keeps delivering events; dropping it stops the watch.
    _watcher: notify::RecommendedWatcher,
}

/// The set of resident projects plus the reindex queue they feed.
pub struct Registry {
    projects: Mutex<HashMap<PathBuf, Arc<Project>>>,
    cap: usize,
    /// Reindex jobs drain to the single worker thread; `pending` dedups them so a
    /// burst of file events for one project collapses to one rebuild.
    queue: Sender<PathBuf>,
    pending: Mutex<HashSet<PathBuf>>,
}

impl Registry {
    /// Build the registry and spawn its single reindex worker.
    fn new(cap: usize) -> Arc<Self> {
        let (tx, rx) = std::sync::mpsc::channel();
        let registry = Arc::new(Registry {
            projects: Mutex::new(HashMap::new()),
            cap,
            queue: tx,
            pending: Mutex::new(HashSet::new()),
        });
        spawn_worker(Arc::clone(&registry), rx);
        registry
    }

    /// Queue a project for re-index unless one is already queued for it.
    fn enqueue(&self, root: PathBuf) {
        if self.pending.lock().unwrap().insert(root.clone()) {
            let _ = self.queue.send(root);
        }
    }

    /// The resident graph for `root`, building and registering it on first use.
    fn graph_for(self: &Arc<Self>, root: &Path) -> Result<Arc<InMemoryGraph>, String> {
        let root = canonical(root);
        // clone the project handle out from under the registry lock, then read its
        // graph — so the brief registry lock never spans a graph read
        let resident = self.projects.lock().unwrap().get(&root).map(Arc::clone);
        if let Some(project) = resident {
            *project.last_access.lock().unwrap() = Instant::now();
            let graph = Arc::clone(&project.graph.read().unwrap());
            return Ok(graph);
        }
        self.register(&root)?;
        let project = self
            .projects
            .lock()
            .unwrap()
            .get(&root)
            .map(Arc::clone)
            .ok_or("project vanished after register")?;
        let graph = Arc::clone(&project.graph.read().unwrap());
        Ok(graph)
    }

    /// Build a project's graph, start watching it, insert it, and evict the LRU
    /// project if that pushes the resident set over the cap.
    fn register(self: &Arc<Self>, root: &Path) -> Result<(), String> {
        let graph = build_graph(root).map_err(|e| e.to_string())?;
        let watcher = start_watcher(root, Arc::clone(self)).map_err(|e| e.to_string())?;
        let project = Arc::new(Project {
            graph: RwLock::new(Arc::new(graph)),
            last_access: Mutex::new(Instant::now()),
            _watcher: watcher,
        });
        let mut projects = self.projects.lock().unwrap();
        projects.insert(root.to_path_buf(), project);
        while projects.len() > self.cap {
            // evict the least-recently-accessed; dropping it stops its watcher
            let victim = projects
                .iter()
                .min_by_key(|(_, p)| *p.last_access.lock().unwrap())
                .map(|(k, _)| k.clone());
            match victim {
                Some(k) => {
                    projects.remove(&k);
                }
                None => break,
            }
        }
        Ok(())
    }

    /// Swap in a freshly built graph for an already-resident project. A re-index
    /// for a project that was evicted meanwhile is dropped — it will rebuild on
    /// its next query.
    fn replace_graph(&self, root: &Path, graph: InMemoryGraph) {
        if let Some(project) = self.projects.lock().unwrap().get(root) {
            *project.graph.write().unwrap() = Arc::new(graph);
        }
    }

    fn status(&self) -> serde_json::Value {
        let projects = self.projects.lock().unwrap();
        let rows: Vec<_> = projects
            .iter()
            .map(|(root, p)| {
                let g = p.graph.read().unwrap();
                serde_json::json!({
                    "root": root.to_string_lossy(),
                    "nodes": g.node_count(),
                    "edges": edge_count(&g),
                })
            })
            .collect();
        serde_json::json!({ "resident": rows.len(), "cap": self.cap, "projects": rows })
    }
}

/// The single reindex worker: one rebuild at a time, so a machine full of active
/// repos never spins up more than one indexing job's worth of CPU.
fn spawn_worker(registry: Arc<Registry>, rx: Receiver<PathBuf>) {
    std::thread::Builder::new()
        .name("ripple-reindex".to_owned())
        .spawn(move || {
            for root in rx {
                // clear pending first, so events arriving during the rebuild queue
                // a fresh job rather than being swallowed
                registry.pending.lock().unwrap().remove(&root);
                match build_graph(&root) {
                    Ok(graph) => registry.replace_graph(&root, graph),
                    Err(e) => eprintln!("ripple daemon: reindex {} failed: {e}", root.display()),
                }
            }
        })
        .expect("spawn reindex worker");
}

/// Watch a project tree and enqueue a re-index on any relevant change. `.ripple/`
/// and `.git/` are filtered so the daemon's own graph writes don't loop.
fn start_watcher(root: &Path, registry: Arc<Registry>) -> Result<notify::RecommendedWatcher> {
    use notify::{RecursiveMode, Watcher};
    let root_owned = root.to_path_buf();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        if event.paths.iter().any(|p| is_source_change(p)) {
            registry.enqueue(root_owned.clone());
        }
    })
    .context("create file watcher")?;
    watcher
        .watch(root, RecursiveMode::Recursive)
        .with_context(|| format!("watch {}", root.display()))?;
    Ok(watcher)
}

/// Is this path a source change worth rebuilding for? Ignores the daemon's own
/// index dir and VCS metadata (the loop guard), plus dependency/build dirs.
fn is_source_change(path: &Path) -> bool {
    let ignored = [
        ".ripple",
        ".git",
        "node_modules",
        "target",
        "_build",
        ".venv",
    ];
    !path
        .components()
        .any(|c| c.as_os_str().to_str().is_some_and(|s| ignored.contains(&s)))
}

fn canonical(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

// ── the build hook: injected so the daemon module stays independent of the CLI's
//    full index pipeline (which lives in `main`) ─────────────────────────────

type BuildFn = fn(&Path) -> Result<InMemoryGraph>;

use std::sync::OnceLock;
static BUILD: OnceLock<BuildFn> = OnceLock::new();

/// Register the full-index build the daemon calls to (re)build a project graph.
/// Called once from `main` before `run`, so this module doesn't have to depend on
/// the git-overlay/cross-service pipeline directly.
pub fn set_builder(f: BuildFn) {
    let _ = BUILD.set(f);
}

fn build_graph(root: &Path) -> Result<InMemoryGraph> {
    let build = BUILD.get().context("daemon builder not set")?;
    build(root)
}

/// Load the persisted graph a build wrote, so `main`'s builder can index-then-load.
pub fn load_persisted(db_path: PathBuf) -> Result<InMemoryGraph> {
    RedbStore::open(db_path).load()
}

// ── server ─────────────────────────────────────────────────────────────────

/// Run the daemon: bind the socket and serve until asked to stop. Blocks.
pub fn run(cap: Option<usize>) -> Result<()> {
    let sock = socket_path();
    if let Some(parent) = sock.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    // a stale socket file from a crashed daemon would make bind fail
    if sock.exists() {
        if UnixStream::connect(&sock).is_ok() {
            anyhow::bail!("a ripple daemon is already listening on {}", sock.display());
        }
        let _ = std::fs::remove_file(&sock);
    }
    let listener = UnixListener::bind(&sock).with_context(|| format!("bind {}", sock.display()))?;
    let registry = Registry::new(cap.unwrap_or(DEFAULT_MAX_RESIDENT));
    eprintln!("ripple daemon: listening on {}", sock.display());

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let registry = Arc::clone(&registry);
        // one short-lived thread per connection; requests are quick graph reads
        std::thread::spawn(move || {
            if handle(&registry, stream) == Control::Stop {
                // best-effort socket cleanup, then exit the process
                let _ = std::fs::remove_file(socket_path());
                std::process::exit(0);
            }
        });
    }
    Ok(())
}

#[derive(PartialEq)]
enum Control {
    Continue,
    Stop,
}

/// Read one request, answer it, and say whether the daemon should stop.
fn handle(registry: &Arc<Registry>, stream: UnixStream) -> Control {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return Control::Continue,
    });
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
        return Control::Continue;
    }
    let (response, control) = match serde_json::from_str::<Request>(line.trim()) {
        Ok(Request::Stop) => (
            Response::ok(serde_json::json!({"stopping": true})),
            Control::Stop,
        ),
        Ok(req) => (dispatch(registry, req), Control::Continue),
        Err(e) => (
            Response::err(format!("bad request: {e}")),
            Control::Continue,
        ),
    };
    let mut stream = stream;
    if let Ok(body) = serde_json::to_string(&response) {
        let _ = writeln!(stream, "{body}");
        let _ = stream.flush();
    }
    control
}

/// Answer one non-stop request against the resident graph.
fn dispatch(registry: &Arc<Registry>, req: Request) -> Response {
    match req {
        Request::Status => Response::ok(registry.status()),
        Request::Stop => Response::ok(serde_json::Value::Null), // handled in `handle`
        Request::Register { root } => match registry.graph_for(Path::new(&root)) {
            Ok(g) => Response::ok(serde_json::json!({
                "registered": root, "nodes": g.node_count(), "edges": edge_count(&g)
            })),
            Err(e) => Response::err(e),
        },
        Request::Impact {
            root,
            symbol,
            budget,
        } => {
            let graph = match registry.graph_for(Path::new(&root)) {
                Ok(g) => g,
                Err(e) => return Response::err(e),
            };
            let Some((seeds, _)) = graph.lookup(&symbol) else {
                return Response::err(format!("no symbol matched: {symbol}"));
            };
            let ids: Vec<_> = seeds.iter().map(|n| n.id).collect();
            let result = query::impact(&graph, &ids, budget.unwrap_or(20));
            let hits: Vec<_> = result
                .hits
                .iter()
                .map(|h| {
                    serde_json::json!({
                        "symbol": h.node.name,
                        "module": h.node.module_path,
                        "score": h.score,
                        "via": format!("{:?}", h.via),
                        "depth": h.depth,
                    })
                })
                .collect();
            Response::ok(serde_json::json!({ "total": result.hits.len(), "hits": hits }))
        }
        Request::Neighbors {
            root,
            symbol,
            dir,
            depth,
        } => {
            let graph = match registry.graph_for(Path::new(&root)) {
                Ok(g) => g,
                Err(e) => return Response::err(e),
            };
            let Some((seeds, _)) = graph.lookup(&symbol) else {
                return Response::err(format!("no symbol matched: {symbol}"));
            };
            let inbound = !matches!(dir.as_deref(), Some("out"));
            let direction = if inbound { Dir::In } else { Dir::Out };
            let mut rows = Vec::new();
            for start in seeds.iter().map(|n| n.id) {
                for hop in graph.neighbors(start, direction, None, depth.unwrap_or(1)) {
                    rows.push(serde_json::json!({
                        "symbol": hop.node.name,
                        "module": hop.node.module_path,
                        "kind": format!("{:?}", hop.edge.kind),
                        "confidence": hop.edge.confidence,
                    }));
                }
            }
            Response::ok(
                serde_json::json!({ "direction": if inbound {"in"} else {"out"}, "neighbors": rows }),
            )
        }
    }
}

// ── client: used by `ripple daemon status/stop/register` and query fallback ──

/// Send one request to a running daemon and return its response. `Err` means the
/// daemon is not reachable (not running), which callers use to fall back to the
/// cold path.
pub fn request(req: &Request) -> Result<Response> {
    let sock = socket_path();
    let mut stream = UnixStream::connect(&sock)
        .with_context(|| format!("no ripple daemon at {}", sock.display()))?;
    let body = serde_json::to_string(req)?;
    writeln!(stream, "{body}")?;
    stream.flush()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(serde_json::from_str(line.trim())?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_change_ignores_index_and_vcs_dirs() {
        assert!(is_source_change(&PathBuf::from("/repo/src/main.rs")));
        assert!(!is_source_change(&PathBuf::from(
            "/repo/.ripple/graph.redb"
        )));
        assert!(!is_source_change(&PathBuf::from("/repo/.git/index")));
        assert!(!is_source_change(&PathBuf::from(
            "/repo/node_modules/x/y.js"
        )));
        assert!(!is_source_change(&PathBuf::from("/repo/target/debug/foo")));
    }

    #[test]
    fn request_roundtrips_through_json() {
        let req = Request::Impact {
            root: "/r".to_owned(),
            symbol: "foo".to_owned(),
            budget: Some(5),
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"op\":\"impact\""));
        let back: Request = serde_json::from_str(&s).unwrap();
        assert!(matches!(
            back,
            Request::Impact {
                budget: Some(5),
                ..
            }
        ));
    }

    #[test]
    fn socket_path_honours_override() {
        // RIPPLE_SOCKET wins; use a value unlikely to collide
        std::env::set_var("RIPPLE_SOCKET", "/tmp/ripple-test-xyz.sock");
        assert_eq!(socket_path(), PathBuf::from("/tmp/ripple-test-xyz.sock"));
        std::env::remove_var("RIPPLE_SOCKET");
    }
}
