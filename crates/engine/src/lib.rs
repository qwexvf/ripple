//! Reusable code-graph engine — the language-agnostic core of ripple.
//!
//! Builds a symbol graph from source with tree-sitter (no build step, no LSP,
//! no git), then answers reachability / blast-radius queries over it. This is
//! everything ripple's own graph is made of, minus the git overlay, risk
//! scoring, review targeting and MCP surface that live in `ripple-cli` — so a
//! tool that only wants "is B reachable from A?" can depend on this one crate
//! instead of wiring up six.
//!
//! ```no_run
//! use std::path::Path;
//! let graph = engine::index(Path::new("."))?;
//! if engine::is_reachable(&graph, "handleRequest", "child_process.exec") {
//!     println!("the vulnerable sink is reachable from the entrypoint");
//! }
//! # Ok::<(), anyhow::Error>(())
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

// ── graph model ──────────────────────────────────────────────────────────
pub use ir::{Edge, EdgeKind, Node, NodeKind, SymbolId};
pub use store::{Dir, InMemoryGraph, Match};

// ── queries ──────────────────────────────────────────────────────────────
pub use query::{
    impact, locate, paths, reachable_modules, Impact, ImpactHit, Located, Route, Step,
};

/// Default reachability depth — deep enough for real call chains, bounded so a
/// dense graph stays finite. Override with [`reachable_within`].
pub const DEFAULT_DEPTH: usize = 6;

/// Default number of distinct routes to return, shortest first.
pub const DEFAULT_LIMIT: usize = 3;

/// Build an in-memory graph from a single root. No cache, no persistence — one
/// shot, then drop it. For a persistent/incremental graph use the `store` and
/// `resolve` crates directly the way `ripple-cli` does.
pub fn index(root: &Path) -> Result<InMemoryGraph> {
    let br = resolve::build(root)?;
    Ok(InMemoryGraph::from_parts(br.nodes, br.edges))
}

/// Build one graph spanning several roots — e.g. a project plus its vendored
/// dependency sources (`node_modules`, `site-packages`) — so an import in one
/// root can resolve to a def in another. That cross-root pass is what lets a
/// reachability query cross from your code into a dependency's vulnerable
/// function. See [`resolve::build_incremental`].
pub fn index_roots(roots: &[PathBuf]) -> Result<InMemoryGraph> {
    let indexed = resolve::build_incremental(roots, &HashMap::new())?;
    Ok(InMemoryGraph::from_parts(
        indexed.result.nodes,
        indexed.result.edges,
    ))
}

/// Every route from any symbol named `from` to any symbol named `to`, along
/// dependency direction, shortest first. Empty = not reachable in the graph.
///
/// Both names go through the graph's own [`InMemoryGraph::lookup`] widening
/// (exact, then qualified-name suffix, then substring), so `"child_process.exec"`
/// and a bare `"exec"` both resolve — an ambiguous name widens the seed set
/// rather than failing.
pub fn reachable(graph: &InMemoryGraph, from: &str, to: &str) -> Vec<Route> {
    reachable_within(graph, from, to, DEFAULT_DEPTH, DEFAULT_LIMIT)
}

/// [`reachable`] with an explicit depth cap and route limit.
pub fn reachable_within(
    graph: &InMemoryGraph,
    from: &str,
    to: &str,
    max_depth: usize,
    limit: usize,
) -> Vec<Route> {
    let (Some((froms, _)), Some((tos, _))) = (graph.lookup(from), graph.lookup(to)) else {
        return Vec::new();
    };
    let from_ids: Vec<SymbolId> = froms.iter().map(|n| n.id).collect();
    let to_ids: Vec<SymbolId> = tos.iter().map(|n| n.id).collect();

    let mut routes: Vec<Route> = Vec::new();
    for &f in &from_ids {
        for &t in &to_ids {
            routes.extend(paths(graph, f, t, max_depth, limit));
        }
    }
    // fold the per-(from,to) results back into one shortest-first ranking
    routes.sort_by(|a, b| {
        a.steps.len().cmp(&b.steps.len()).then(
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    routes.truncate(limit);
    routes
}

/// True if any symbol named `to` is reachable from any symbol named `from`.
/// The yes/no answer a CVE-reachability gate wants: is the vulnerable function
/// actually called (transitively) from the project's own code?
pub fn is_reachable(graph: &InMemoryGraph, from: &str, to: &str) -> bool {
    let (Some((froms, _)), Some((tos, _))) = (graph.lookup(from), graph.lookup(to)) else {
        return false;
    };
    let to_ids: Vec<SymbolId> = tos.iter().map(|n| n.id).collect();
    for f in froms {
        for &t in &to_ids {
            if !paths(graph, f.id, t, DEFAULT_DEPTH, 1).is_empty() {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // app.ts calls a helper that calls the sink; danger.ts is defined but never
    // reached from the entrypoint.
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("app.ts"),
            "import { runJob } from './job';\nexport function handleRequest() { runJob(); }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("job.ts"),
            "import { exec } from './sink';\nexport function runJob() { exec(); }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("sink.ts"),
            "export function exec() {}\nexport function unusedSink() {}\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn indexes_and_reaches_a_transitive_sink() {
        let dir = fixture();
        let graph = index(dir.path()).unwrap();
        assert!(graph.node_count() > 0, "graph should have symbols");
        assert!(
            is_reachable(&graph, "handleRequest", "exec"),
            "exec is called two hops from handleRequest"
        );
    }

    #[test]
    fn unreachable_and_unknown_names_are_false() {
        let dir = fixture();
        let graph = index(dir.path()).unwrap();
        assert!(
            !is_reachable(&graph, "handleRequest", "unusedSink"),
            "unusedSink is never called"
        );
        assert!(
            !is_reachable(&graph, "handleRequest", "noSuchSymbol"),
            "an unknown target is not reachable"
        );
        assert!(reachable(&graph, "noSuchCaller", "exec").is_empty());
    }
}
