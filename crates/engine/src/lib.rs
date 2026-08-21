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

// ── external-dependency reachability ───────────────────────────────────────
//
// The binding pass in `resolve` mints `NodeKind::External` nodes for symbols
// that live outside the indexed roots (`urql`, `urql.useQuery`). These three
// queries read that enrichment directly — the engine seeds itself, with no
// external symbol list handed in from outside.

/// Call edges — the only edge kinds a reachability walk should follow. `Imports`
/// makes module nodes look like callers; `References` is a use, not a call.
const CALL_KINDS: &[EdgeKind] = &[EdgeKind::Calls, EdgeKind::AsyncCall];

/// The external `dep.symbol` node, if the binding pass created one.
fn external_symbol<'g>(graph: &'g InMemoryGraph, dep: &str, symbol: &str) -> Option<&'g Node> {
    let qn = format!("{dep}.{symbol}");
    graph
        .nodes()
        .find(|n| n.kind == NodeKind::External && n.module_path == dep && n.qualified_name == qn)
}

/// Import-level floor: does the project import `dep` at all? True when any
/// `Imports` edge lands on an external node keyed to `dep`. This is the soundest
/// signal — it holds even when call resolution can't prove the symbol is used.
pub fn imports(graph: &InMemoryGraph, dep: &str) -> bool {
    graph
        .nodes()
        .filter(|n| n.kind == NodeKind::External && n.module_path == dep)
        .any(|n| {
            graph
                .in_edges(n.id)
                .iter()
                .any(|e| e.kind == EdgeKind::Imports)
        })
}

/// Project symbols with a `Calls`/`References` edge to the external `dep.symbol`
/// node — the symbol is referenced somewhere, whether or not a call chain into
/// it can be proven. Deduped, in a stable (module path, line) order.
pub fn uses(graph: &InMemoryGraph, dep: &str, symbol: &str) -> Vec<Node> {
    let Some(node) = external_symbol(graph, dep, symbol) else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<Node> = Vec::new();
    for e in graph.in_edges(node.id) {
        if !matches!(
            e.kind,
            EdgeKind::Calls | EdgeKind::AsyncCall | EdgeKind::References
        ) {
            continue;
        }
        if let Some(src) = graph.get(e.src) {
            // an external node never "uses" another — only project symbols count
            if src.kind != NodeKind::External && seen.insert(src.id) {
                out.push(src.clone());
            }
        }
    }
    out.sort_by(|a, b| {
        a.module_path
            .cmp(&b.module_path)
            .then(a.span.start_line.cmp(&b.span.start_line))
    });
    out
}

/// Transitive call routes *into* the external `dep.symbol` node — one shortest
/// call-only route per distinct reaching project symbol, shortest first. Seeds
/// itself from the external node's incoming call edges and walks callers; no
/// external seed list required. Empty when nothing reaches it (or it has no
/// node — never imported).
pub fn reaches(graph: &InMemoryGraph, dep: &str, symbol: &str) -> Vec<Route> {
    let Some(node) = external_symbol(graph, dep, symbol) else {
        return Vec::new();
    };
    // every symbol that transitively reaches the external node along call edges
    let callers = graph.neighbors(node.id, Dir::In, Some(CALL_KINDS), DEFAULT_DEPTH);
    let mut routes: Vec<Route> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for hop in callers {
        if !seen.insert(hop.node.id) {
            continue;
        }
        // one shortest route per caller; keep call-edge-only routes so an import
        // hop can never masquerade as a call path
        for r in paths(graph, hop.node.id, node.id, DEFAULT_DEPTH, 1) {
            if r.steps.iter().all(|s| CALL_KINDS.contains(&s.edge.kind)) {
                routes.push(r);
            }
        }
    }
    routes.sort_by(|a, b| {
        a.steps
            .len()
            .cmp(&b.steps.len())
            .then(b.confidence.total_cmp(&a.confidence))
    });
    routes
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

    // ── external-import binding ────────────────────────────────────────────

    /// A component imports `useQuery` from the external package `urql` and calls
    /// it directly; another reaches it transitively through a local hook.
    fn urql_fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        // direct external caller
        fs::write(
            dir.path().join("FindingDetailPane.tsx"),
            "import { useQuery } from 'urql';\n\
             export function FindingDetailPane() { return useQuery(); }\n",
        )
        .unwrap();
        // local hook that wraps the external symbol
        fs::write(
            dir.path().join("useOrgMembers.ts"),
            "import { useQuery } from 'urql';\n\
             export function useOrgMembers() { return useQuery(); }\n",
        )
        .unwrap();
        // reaches the external symbol only through the local hook
        fs::write(
            dir.path().join("MembersSection.tsx"),
            "import { useOrgMembers } from './useOrgMembers';\n\
             export function MembersSection() { return useOrgMembers(); }\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn ts_external_import_is_imported_used_and_reached() {
        let dir = urql_fixture();
        let graph = index(dir.path()).unwrap();

        assert!(imports(&graph, "urql"), "urql is imported");

        let users = uses(&graph, "urql", "useQuery");
        assert!(!users.is_empty(), "useQuery is called directly");
        let names: Vec<&str> = users.iter().map(|n| n.name.as_str()).collect();
        assert!(
            names.contains(&"FindingDetailPane") && names.contains(&"useOrgMembers"),
            "direct callers of useQuery show up in `uses`: {names:?}"
        );

        let routes = reaches(&graph, "urql", "useQuery");
        assert!(!routes.is_empty(), "something reaches urql.useQuery");
        // the transitive reacher: MembersSection → useOrgMembers → urql.useQuery
        let membersection_id = graph.find_by_name("MembersSection")[0].id;
        let ext = external_symbol(&graph, "urql", "useQuery").unwrap().id;
        let route = paths(&graph, membersection_id, ext, DEFAULT_DEPTH, 1);
        assert!(
            !route.is_empty(),
            "MembersSection reaches urql.useQuery transitively through useOrgMembers"
        );
        assert!(
            route[0].steps.len() >= 2,
            "MembersSection's route to useQuery is transitive (through the hook), not direct"
        );
    }

    #[test]
    fn ts_path_alias_import_stays_local_not_external() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("tsconfig.json"),
            "{ \"compilerOptions\": { \"baseUrl\": \".\", \"paths\": { \"~/*\": [\"src/*\"] } } }\n",
        )
        .unwrap();
        fs::create_dir_all(dir.path().join("src/hooks")).unwrap();
        fs::write(
            dir.path().join("src/hooks/useThing.ts"),
            "export function useThing() {}\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/Comp.tsx"),
            "import { useThing } from '~/hooks/useThing';\n\
             export function Comp() { return useThing(); }\n",
        )
        .unwrap();

        let graph = index(dir.path()).unwrap();

        // the aliased import resolved locally: Comp's call reaches the real def
        assert!(
            is_reachable(&graph, "Comp", "useThing"),
            "an aliased local import must resolve as local so its callers are found"
        );
        // and it was NOT treated as an external package
        assert!(
            !graph
                .nodes()
                .any(|n| n.kind == NodeKind::External && n.module_path.starts_with('~')),
            "a tsconfig path alias is local, not an external dep"
        );
        assert!(!imports(&graph, "~/hooks/useThing"));
    }

    #[test]
    fn py_external_import_is_used() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("runner.py"),
            "from subprocess import run\n\n\
             def launch():\n    return run(['ls'])\n",
        )
        .unwrap();
        let graph = index(dir.path()).unwrap();

        assert!(imports(&graph, "subprocess"), "subprocess is imported");
        let users = uses(&graph, "subprocess", "run");
        assert!(
            users.iter().any(|n| n.name == "launch"),
            "launch uses subprocess.run: {:?}",
            users.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
        assert!(
            !reaches(&graph, "subprocess", "run").is_empty(),
            "launch reaches subprocess.run"
        );
    }

    #[test]
    fn ts_namespace_member_call_binds_external_symbol() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Counter.tsx"),
            "import * as React from 'react';\n\
             export function Counter() { return React.useState(0); }\n",
        )
        .unwrap();
        let graph = index(dir.path()).unwrap();

        assert!(imports(&graph, "react"), "react is imported");
        let users = uses(&graph, "react", "useState");
        assert!(
            users.iter().any(|n| n.name == "Counter"),
            "Counter uses react.useState via namespace member call: {:?}",
            users.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
        assert!(
            !reaches(&graph, "react", "useState").is_empty(),
            "Counter reaches react.useState"
        );
    }

    #[test]
    fn py_namespace_member_calls_bind_external_symbols() {
        let dir = tempfile::tempdir().unwrap();
        // plain `import os` + os.system(...)
        fs::write(
            dir.path().join("shell.py"),
            "import os\n\n\
             def wipe():\n    return os.system('rm -rf /tmp/x')\n",
        )
        .unwrap();
        // aliased `import numpy as np` + np.load()
        fs::write(
            dir.path().join("model.py"),
            "import numpy as np\n\n\
             def load_weights():\n    return np.load('w.npy')\n",
        )
        .unwrap();
        let graph = index(dir.path()).unwrap();

        assert!(imports(&graph, "os"), "os is imported");
        let os_users = uses(&graph, "os", "system");
        assert!(
            os_users.iter().any(|n| n.name == "wipe"),
            "wipe uses os.system: {:?}",
            os_users.iter().map(|n| &n.name).collect::<Vec<_>>()
        );

        assert!(imports(&graph, "numpy"), "numpy is imported (aliased)");
        let np_users = uses(&graph, "numpy", "load");
        assert!(
            np_users.iter().any(|n| n.name == "load_weights"),
            "load_weights uses numpy.load via alias: {:?}",
            np_users.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn ts_side_effect_import_is_imported() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("entry.ts"),
            "import 'polyfill';\nexport function boot() {}\n",
        )
        .unwrap();
        let graph = index(dir.path()).unwrap();
        assert!(
            imports(&graph, "polyfill"),
            "a side-effect-only import still registers the dep"
        );
        // it binds no symbol, so there is nothing used or reached
        assert!(uses(&graph, "polyfill", "anything").is_empty());
    }

    #[test]
    fn a_never_imported_symbol_is_absent() {
        let dir = urql_fixture();
        let graph = index(dir.path()).unwrap();
        assert!(!imports(&graph, "left-pad"), "left-pad is never imported");
        assert!(uses(&graph, "left-pad", "leftPad").is_empty());
        assert!(reaches(&graph, "left-pad", "leftPad").is_empty());
        // and a real dep queried for a symbol it never exposed here
        assert!(uses(&graph, "urql", "gql").is_empty());
        assert!(reaches(&graph, "urql", "gql").is_empty());
    }
}
