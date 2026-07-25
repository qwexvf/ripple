//! LSP-backed verification of call edges — docs/11-lsp-integration.md phase 3.
//!
//! A query never blocks on a server: verification runs *before* the traversal, on
//! a bounded neighborhood (the seed files plus one hop), inside a wall-clock
//! budget, and its results are written back to the store as edges carrying
//! `EdgeSource::LspVerified`. So the answer path stays the persisted graph, and a
//! missing or slow server changes freshness, never latency.
//!
//! Reconciliation follows the table in docs/11 with one measured deviation:
//! confirmed → 1.0, server-only → added at 1.0, unreachable → untouched, and a
//! contradiction is *reported* rather than acted on unless asked (see `OnDenial`).

use ir::{Edge, EdgeKind, EdgeSource, SymbolId};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use store::InMemoryGraph;

/// Confidence for a call both ripple and the server found. Two independent
/// extractions agreeing is the strongest evidence available here.
const CONF_VERIFIED: f32 = 1.0;
/// Confidence for a call only the server found.
///
/// docs/11 says add these at 1.0. Measured against dexter 0.7.1 that is too
/// generous: dexter attributes calls made inside an ExUnit `test` block to the
/// preceding `defp`, so 5 of the first 5 additions sampled on 5noobs claimed
/// `direct_messages_test.exs:create_player` called functions the test bodies call.
/// A single unconfirmed source is worth less than an agreement, and invariant 5
/// forbids emitting a fabricated edge as fact — so server-only edges land below the
/// extracted band and say `LspVerified` about where the claim came from.
const CONF_SERVER_ONLY: f32 = 0.7;
/// What an edge's confidence falls to under `--floor-contradicted`.
const CONF_CONTRADICTED: f32 = 0.4;

/// What to do when the server that owns an edge's language *and* workspace does
/// not report the call.
///
/// `Report` is the default, and it is a measurement, not caution. Every one of the
/// five contradictions sampled on the first real run (dexter 0.7.1, 5noobs) was the
/// server's miss, not ripple's: for a multi-clause Elixir function dexter attributes
/// callers to some clauses and not others, so `players.ex:player_in_discord?/1`
/// really does call `get_player` at line 1346 while dexter's caller list omits it.
/// Acting on that denial deletes true edges. Confirmations and additions are safe —
/// they don't depend on the server being complete — so those always apply.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OnDenial {
    /// Count it, show examples, change nothing.
    Report,
    /// Lower confidence to `CONF_CONTRADICTED`, keep the edge.
    Floor,
    /// Delete the edge.
    Drop,
}

/// What to verify, and how much time to spend doing it.
pub struct Plan<'a> {
    /// Module paths to verify: the query's seed files plus one hop.
    pub focus: BTreeSet<String>,
    /// `(tag, absolute path)` per index root, as recorded by indexing.
    pub roots: &'a [(String, PathBuf)],
    /// Wall-clock ceiling for the whole verification pass.
    pub budget: Duration,
    /// What a server's silence about one of our edges is allowed to do.
    pub on_denial: OnDenial,
}

/// What verification changed, and what it could not reach. Never silently partial:
/// every file that went unchecked is reported with the reason.
#[derive(Default)]
pub struct Outcome {
    pub confirmed: usize,
    pub added: usize,
    pub contradicted: usize,
    /// Contradicted edges whose confidence was actually lowered (`--floor-contradicted`).
    pub floored: usize,
    pub files_checked: usize,
    /// Files skipped because the budget ran out.
    pub out_of_budget: Vec<String>,
    /// Files skipped because no usable server covers their language.
    pub no_server: Vec<String>,
    /// Symbols the server could not resolve a call-hierarchy item for — neither
    /// confirmation nor denial, so nothing is changed for them.
    pub unresolved: usize,
    /// A few `target ← caller` pairs the server denied, so a floored edge can be
    /// checked by hand instead of taken on faith.
    pub contradicted_examples: Vec<String>,
    /// A few `target ← caller` pairs the server supplied and ripple lacked. Same
    /// reason: an added edge is a claim, and a claim should be checkable.
    pub added_examples: Vec<String>,
    /// `language: server` for each server that answered.
    pub servers: Vec<String>,
    /// The full edge list after reconciliation, ready to persist.
    pub edges: Vec<Edge>,
    /// Positions in `edges` that `--drop-contradicted` marked for removal. Applied
    /// by `finish` once every verdict is in, so index positions stay valid while
    /// verdicts are still being applied.
    to_drop: HashSet<usize>,
}

impl Outcome {
    /// Drop the edges `--drop-contradicted` marked, then put the list in a total
    /// order. Must run before `edges` is used: it is collected from a `HashMap`, so
    /// without the sort what gets persisted would vary run to run.
    fn finish(mut self) -> Outcome {
        if !self.to_drop.is_empty() {
            let keep: Vec<Edge> = std::mem::take(&mut self.edges)
                .into_iter()
                .enumerate()
                .filter(|(i, _)| !self.to_drop.contains(i))
                .map(|(_, e)| e)
                .collect();
            self.edges = keep;
        }
        self.edges
            .sort_by_key(|e| (e.src.0, e.dst.0, e.site.start_line, e.site.start_col));
        self
    }

    /// Whether anything was written. A contradiction alone changes nothing under
    /// the default `OnDenial::Report`, so `applied` is tracked separately.
    pub fn changed(&self) -> bool {
        self.confirmed + self.added > 0 || !self.to_drop.is_empty() || self.floored > 0
    }

    /// One line, always printed when `--verify` was asked for, so a query that
    /// verified nothing says so instead of looking verified.
    pub fn summary(&self) -> String {
        let mut s = format!(
            "verify lsp: {} files checked, {} confirmed, {} added, {} contradicted",
            self.files_checked, self.confirmed, self.added, self.contradicted
        );
        if !self.servers.is_empty() {
            s.push_str(&format!(" (via {})", self.servers.join(", ")));
        }
        if !self.out_of_budget.is_empty() {
            s.push_str(&format!(
                "\n  unverified — budget: {} files ({})",
                self.out_of_budget.len(),
                sample(&self.out_of_budget)
            ));
        }
        if !self.no_server.is_empty() {
            s.push_str(&format!(
                "\n  unverified — no usable server: {} files ({})",
                self.no_server.len(),
                sample(&self.no_server)
            ));
        }
        if !self.added_examples.is_empty() {
            s.push_str(&format!(
                "\n  added at {CONF_SERVER_ONLY} (server-only, unconfirmed by extraction):"
            ));
        }
        for e in &self.added_examples {
            s.push_str(&format!("\n  added: {e}"));
        }
        if self.contradicted > 0 && self.floored == 0 && self.to_drop.is_empty() {
            s.push_str(
                "\n  contradictions reported only (--floor-contradicted / --drop-contradicted to act):",
            );
        }
        for e in &self.contradicted_examples {
            s.push_str(&format!("\n  contradicted: {e}"));
        }
        if self.unresolved > 0 {
            s.push_str(&format!(
                "\n  {} symbols the server could not resolve — left as extracted",
                self.unresolved
            ));
        }
        s
    }
}

fn sample(files: &[String]) -> String {
    let head: Vec<&str> = files.iter().take(2).map(String::as_str).collect();
    if files.len() > head.len() {
        format!("{}, …", head.join(", "))
    } else {
        head.join(", ")
    }
}

/// Reduce a server's symbol name to the bare function name ripple stores.
///
/// Servers spell the same function several ways: `changeset/2` carries the arity
/// ripple doesn't distinguish, and a call-hierarchy caller comes back fully
/// qualified as `FiveNoobs.Players.PlayerReport.changeset`. Comparing raw names
/// reported every edge as a disagreement when both sides had found exactly the
/// same call.
pub fn bare_name(name: &str) -> &str {
    let no_arity = name.split('/').next().unwrap_or(name).trim();
    no_arity.rsplit('.').next().unwrap_or(no_arity)
}

/// A `documentSymbol` entry that isn't a callable function. dexter reports Ecto
/// `schema "players" do` blocks as function-kind symbols named `schema players`,
/// and asking for their callers is meaningless.
pub fn is_callable_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(char::is_whitespace)
}

/// The files worth verifying for a query: the seeds' own files plus the files of
/// their direct callers. One hop, because that's what fixes the first two levels
/// of a blast radius — the part a reviewer actually reads.
pub fn focus_files(graph: &InMemoryGraph, seeds: &[SymbolId]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for &seed in seeds {
        if let Some(n) = graph.get(seed) {
            out.insert(n.module_path.clone());
        }
        for e in graph.in_edges(seed) {
            if let Some(n) = graph.get(e.src) {
                out.insert(n.module_path.clone());
            }
        }
    }
    out
}

/// Run the pass. Returns the reconciled edge list plus a report; the caller
/// rebuilds the graph from it and persists it.
pub fn run(graph: &InMemoryGraph, plan: &Plan) -> Outcome {
    let deadline = Instant::now() + plan.budget;
    let registry = lang::registry();
    let mut out = Outcome {
        edges: graph.edges().cloned().collect(),
        ..Outcome::default()
    };
    let index: HashMap<(SymbolId, SymbolId), usize> = out
        .edges
        .iter()
        .enumerate()
        .filter(|(_, e)| e.kind == EdgeKind::Calls)
        .map(|(i, e)| ((e.dst, e.src), i))
        .collect();

    let mut work = plan.focus.clone();
    for (tag, root) in plan.roots {
        let specs = lsp::load(root).unwrap_or_else(|_| lsp::defaults());
        for spec in &specs {
            let mine: Vec<String> = work
                .iter()
                .filter(|m| in_root(m, tag))
                .filter(|m| language_of(&registry, m) == Some(spec.language.as_str()))
                .cloned()
                .collect();
            if mine.is_empty() || !lsp::applies(spec, root) {
                continue;
            }
            let Some((mut client, server)) = start(spec, root) else {
                continue;
            };
            out.servers.push(format!(
                "{}: {}",
                spec.language,
                server.as_deref().unwrap_or(&spec.command)
            ));
            let pass = Pass {
                graph,
                root,
                cov: Coverage {
                    tag,
                    language: &spec.language,
                    registry: &registry,
                },
                index: &index,
                plan,
            };
            for module in mine {
                work.remove(&module);
                if Instant::now() >= deadline {
                    out.out_of_budget.push(module);
                    continue;
                }
                verify_file(&pass, &mut client, &module, &mut out);
            }
            client.stop();
        }
    }
    // whatever no server claimed
    out.no_server.extend(work);
    out.finish()
}

/// Everything one server's pass reads but never mutates. Threading these as
/// separate arguments made `verify_file` an eight-argument function.
struct Pass<'a> {
    graph: &'a InMemoryGraph,
    /// Absolute path of the index root the files belong to.
    root: &'a Path,
    cov: Coverage<'a>,
    /// `(dst, src)` → position in `Outcome::edges`, so a verdict finds the edge it
    /// is about without rescanning the list per symbol.
    index: &'a HashMap<(SymbolId, SymbolId), usize>,
    plan: &'a Plan<'a>,
}

/// What the answering server's workspace actually covers. Silence from a server
/// only counts as denial inside this — an edge from another root or another
/// language (a GraphQL or Ecto join, say) was never part of the question.
struct Coverage<'a> {
    tag: &'a str,
    language: &'a str,
    registry: &'a [Box<dyn lang::LanguageAdapter>],
}

impl Coverage<'_> {
    fn covers(&self, module: &str) -> bool {
        in_root(module, self.tag) && language_of(self.registry, module) == Some(self.language)
    }
}

/// Start and hand-shake a server, or give up on it. A server that can't do
/// `callHierarchy` cannot verify calls, so it is treated as absent.
fn start(spec: &lsp::ServerSpec, root: &Path) -> Option<(lsp::Client, Option<String>)> {
    let mut client = lsp::Client::start(spec, root).ok()?;
    let (caps, server) = client.initialize(root, spec).ok()?;
    if !caps.call_hierarchy {
        client.stop();
        return None;
    }
    Some((client, server))
}

fn in_root(module: &str, tag: &str) -> bool {
    tag.is_empty() || module.starts_with(&format!("{tag}/"))
}

fn language_of<'a>(
    registry: &'a [Box<dyn lang::LanguageAdapter>],
    module: &str,
) -> Option<&'a str> {
    lang::adapter_for(registry, Path::new(module)).map(lang::LanguageAdapter::id)
}

/// Ask the server about every function in one file and reconcile each answer.
///
/// Answers are unioned per *name* before any verdict, because ripple collapses a
/// multi-clause or multi-arity function into one symbol while the server reports
/// one entry per clause. Reconciling clause-by-clause made every caller of
/// `get_player/1` look like a denial of `get_player/2` — 42 fabricated
/// contradictions on the first real run.
fn verify_file(pass: &Pass, client: &mut lsp::Client, module: &str, out: &mut Outcome) {
    let rel = module
        .strip_prefix(&format!("{}/", pass.cov.tag))
        .unwrap_or(module);
    let abs = pass.root.join(rel);
    if client.open(&abs).is_err() {
        out.no_server.push(module.to_owned());
        return;
    }
    let Ok(symbols) = client.functions(&abs) else {
        out.no_server.push(module.to_owned());
        return;
    };
    out.files_checked += 1;

    // name → (any clause resolved, unioned callers). BTreeMap so verdicts are
    // applied in a fixed order.
    let mut claims: std::collections::BTreeMap<SymbolId, (bool, HashSet<SymbolId>)> =
        std::collections::BTreeMap::new();
    for sym in symbols {
        let name = bare_name(&sym.name).to_owned();
        if !is_callable_name(&name) {
            continue;
        }
        let Some(target) = pass
            .graph
            .nodes_in_file(module)
            .into_iter()
            .find(|n| n.name == name)
        else {
            continue; // the server sees a symbol ripple has no node for
        };
        let target_id = target.id;
        let entry = claims.entry(target_id).or_default();
        let Ok(Some(sites)) = client.incoming_calls(&abs, sym.line, sym.character) else {
            continue; // this clause is unresolvable; another may not be
        };
        entry.0 = true;
        entry.1.extend(callers(pass, target_id, &sites));
    }
    for (target, (resolved, theirs)) in claims {
        if !resolved {
            out.unresolved += 1; // no clause resolved: neither confirmed nor denied
            continue;
        }
        reconcile(pass, target, &theirs, out);
    }
}

/// The server's caller set for one symbol, as ripple symbol ids.
///
/// Callers in files ripple doesn't index are dropped (a server that also indexes
/// dependencies would otherwise "add" edges to code nobody here will change), and
/// self-recursion is dropped because ripple excludes it by design.
fn callers(pass: &Pass, target: SymbolId, sites: &[lsp::CallSite]) -> HashSet<SymbolId> {
    let mut out = HashSet::new();
    for site in sites {
        let Ok(rel) = site.path.strip_prefix(pass.root) else {
            continue;
        };
        let module = resolve::namespace(pass.cov.tag, &rel.to_string_lossy());
        for attributed in attribute(pass.graph, &module, site) {
            match attributed {
                Attribution::Symbol(id) if id != target => {
                    out.insert(id);
                }
                // a call in the file but inside no indexed symbol (a module body, an
                // ExUnit `test` block) has no node to hang an edge on — issue #18
                Attribution::Symbol(_) | Attribution::OutsideAnySymbol => {}
            }
        }
    }
    out
}

/// Which ripple symbol a server-reported call belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribution {
    Symbol(SymbolId),
    /// The call's position falls inside the file but inside no indexed symbol.
    OutsideAnySymbol,
}

/// Attribute one reported call to ripple's symbols, by *position* rather than by
/// the name the server gave it.
///
/// The server's `name`/`line` describe the caller it decided on, and that decision
/// can be wrong: dexter credits a call inside an ExUnit `test` block to the
/// preceding `defp`, which really does not contain it. `fromRanges` carries the
/// call's own line, so the innermost enclosing ripple symbol is a fact we can check
/// instead of a claim we have to trust. A server that sends no `fromRanges` leaves
/// nothing to check, so its naming is used as before.
///
/// Returns one attribution per distinct call site (a caller can call the target
/// several times, and in Elixir those sites can be in different clauses).
pub fn attribute(graph: &InMemoryGraph, module: &str, site: &lsp::CallSite) -> Vec<Attribution> {
    if site.call_lines.is_empty() {
        let name = bare_name(&site.name);
        return graph
            .nodes_in_file(module)
            .into_iter()
            .find(|n| n.name == name)
            .map(|n| vec![Attribution::Symbol(n.id)])
            .unwrap_or_default();
    }
    // only a callable can contain a call. An Elixir module's own node spans the whole
    // file, so including it would turn every call sitting outside a function into
    // "the module called it" — precisely the case issue #18 is about, and the one
    // that has to stay visible.
    let nodes: Vec<&ir::Node> = graph
        .nodes_in_file(module)
        .into_iter()
        .filter(|n| matches!(n.kind, ir::NodeKind::Function | ir::NodeKind::Method))
        .collect();
    site.call_lines
        .iter()
        .map(|&line| {
            // innermost containing definition site, so a clause or nested definition
            // is credited over a wider one
            nodes
                .iter()
                .filter_map(|n| n.containing_span(line).map(|s| (n, s)))
                .min_by_key(|(_, s)| s.end_line - s.start_line)
                .map_or(Attribution::OutsideAnySymbol, |(n, _)| {
                    Attribution::Symbol(n.id)
                })
        })
        .collect()
}

/// `target ← caller`, for a report line a human can go and check.
fn describe(pass: &Pass, target: SymbolId, src: SymbolId) -> String {
    let name = |id| {
        pass.graph.get(id).map_or_else(
            || "?".to_owned(),
            |n: &ir::Node| format!("{}:{}", n.module_path, n.name),
        )
    };
    format!("{} ← {}", name(target), name(src))
}

/// Apply the docs/11 reconciliation table for one target symbol.
fn reconcile(pass: &Pass, target: SymbolId, theirs: &HashSet<SymbolId>, out: &mut Outcome) {
    let ours: HashSet<SymbolId> = pass
        .graph
        .in_edges(target)
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls)
        .map(|e| e.src)
        .collect();

    for &src in theirs.intersection(&ours) {
        if let Some(&i) = pass.index.get(&(target, src)) {
            out.edges[i].confidence = CONF_VERIFIED;
            out.edges[i].source = EdgeSource::LspVerified;
            out.confirmed += 1;
        }
    }
    for &src in theirs.difference(&ours) {
        let site = pass.graph.get(src).map_or(
            ir::Span {
                start_line: 0,
                start_col: 0,
                end_line: 0,
                end_col: 0,
            },
            |n| n.span,
        );
        out.edges.push(Edge {
            src,
            dst: target,
            kind: EdgeKind::Calls,
            confidence: CONF_SERVER_ONLY,
            site,
            source: EdgeSource::LspVerified,
        });
        out.added += 1;
        if out.added_examples.len() < 5 {
            out.added_examples.push(describe(pass, target, src));
        }
    }
    for &src in ours.difference(theirs) {
        let Some(&i) = pass.index.get(&(target, src)) else {
            continue;
        };
        if !pass
            .graph
            .get(src)
            .is_some_and(|n| pass.cov.covers(&n.module_path))
        {
            continue;
        }
        out.contradicted += 1;
        if out.contradicted_examples.len() < 5 {
            out.contradicted_examples.push(describe(pass, target, src));
        }
        match pass.plan.on_denial {
            OnDenial::Report => {}
            OnDenial::Floor => {
                out.edges[i].confidence = out.edges[i].confidence.min(CONF_CONTRADICTED);
                out.floored += 1;
            }
            OnDenial::Drop => {
                out.to_drop.insert(i);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ir::{Node, NodeKind, Span};

    fn span(line: u32) -> Span {
        Span {
            start_line: line,
            start_col: 1,
            end_line: line,
            end_col: 1,
        }
    }

    fn node(module: &str, name: &str) -> Node {
        Node {
            id: SymbolId::of(module, name),
            kind: NodeKind::Function,
            name: name.to_owned(),
            qualified_name: name.to_owned(),
            module_path: module.to_owned(),
            span: span(1),
            extra_spans: Vec::new(),
            is_exported: true,
            risk: ir::RiskScores::default(),
        }
    }

    fn call(src: &Node, dst: &Node, confidence: f32) -> Edge {
        Edge {
            src: src.id,
            dst: dst.id,
            kind: EdgeKind::Calls,
            confidence,
            site: span(7),
            source: EdgeSource::Extracted,
        }
    }

    struct Fixture {
        graph: InMemoryGraph,
        index: HashMap<(SymbolId, SymbolId), usize>,
        out: Outcome,
    }

    /// `caller.ex → target.ex` at 0.6, plus an unlinked `other.ex` caller and a
    /// `page.ts` caller from a different language.
    fn fixture() -> Fixture {
        let target = node("api/target.ex", "run");
        let caller = node("api/caller.ex", "call_it");
        let other = node("api/other.ex", "also_calls");
        let ts = node("web/page.ts", "fetchIt");
        let edges = vec![call(&caller, &target, 0.6), call(&ts, &target, 0.9)];
        let index = edges
            .iter()
            .enumerate()
            .map(|(i, e)| ((e.dst, e.src), i))
            .collect();
        let graph = InMemoryGraph::from_parts(vec![target, caller, other, ts], edges.to_vec());
        Fixture {
            graph,
            index,
            out: Outcome {
                edges,
                ..Outcome::default()
            },
        }
    }

    fn plan(on_denial: OnDenial) -> Plan<'static> {
        Plan {
            focus: BTreeSet::new(),
            roots: &[],
            budget: Duration::from_secs(1),
            on_denial,
        }
    }

    fn elixir_coverage(registry: &[Box<dyn lang::LanguageAdapter>]) -> Coverage<'_> {
        Coverage {
            tag: "api",
            language: "elixir",
            registry,
        }
    }

    fn run_reconcile(f: &mut Fixture, theirs: &[&str], on_denial: OnDenial) {
        let theirs: HashSet<SymbolId> = theirs
            .iter()
            .map(|m| {
                SymbolId::of(
                    m,
                    if m.ends_with(".ts") {
                        "fetchIt"
                    } else {
                        "call_it"
                    },
                )
            })
            .collect();
        reconcile_with(f, &theirs, on_denial);
    }

    /// Drive `reconcile` against the fixture as an Elixir server in root `api`.
    fn reconcile_with(f: &mut Fixture, theirs: &HashSet<SymbolId>, on_denial: OnDenial) {
        let registry = lang::registry();
        let plan = plan(on_denial);
        let pass = Pass {
            graph: &f.graph,
            root: Path::new("/repo"),
            cov: elixir_coverage(&registry),
            index: &f.index,
            plan: &plan,
        };
        reconcile(
            &pass,
            SymbolId::of("api/target.ex", "run"),
            theirs,
            &mut f.out,
        );
    }

    #[test]
    fn a_confirmed_edge_goes_to_full_confidence_and_records_the_server() {
        let mut f = fixture();
        run_reconcile(&mut f, &["api/caller.ex"], OnDenial::Report);
        assert_eq!(f.out.confirmed, 1);
        assert_eq!(f.out.added, 0);
        let e = &f.out.edges[0];
        assert_eq!(e.confidence, CONF_VERIFIED);
        assert_eq!(e.source, EdgeSource::LspVerified);
    }

    #[test]
    fn floor_contradicted_lowers_confidence_without_deleting() {
        let mut f = fixture();
        run_reconcile(&mut f, &[], OnDenial::Floor);
        assert_eq!(f.out.contradicted, 1, "only the Elixir caller is covered");
        let f = f.out.finish();
        assert_eq!(f.edges.len(), 2, "flooring must not delete");
        let denied = f
            .edges
            .iter()
            .find(|e| e.confidence < 0.5)
            .expect("floored");
        assert_eq!(denied.confidence, CONF_CONTRADICTED);
        assert_eq!(
            denied.source,
            EdgeSource::Extracted,
            "the server didn't produce this edge, so provenance stays"
        );
    }

    #[test]
    fn a_denial_changes_nothing_by_default() {
        let mut f = fixture();
        run_reconcile(&mut f, &[], OnDenial::Report);
        assert_eq!(f.out.contradicted, 1, "counted");
        assert_eq!(f.out.floored, 0);
        assert!(!f.out.changed(), "reporting alone must not trigger a write");
        let out = f.out.finish();
        assert_eq!(out.edges.len(), 2);
        assert_eq!(
            out.edges
                .iter()
                .find(|e| e.confidence < 0.7)
                .map(|e| e.confidence),
            Some(0.6),
            "the extracted confidence survives untouched"
        );
    }

    #[test]
    fn drop_contradicted_removes_the_edge() {
        let mut f = fixture();
        run_reconcile(&mut f, &[], OnDenial::Drop);
        let out = f.out.finish();
        assert_eq!(out.edges.len(), 1);
        assert_eq!(out.edges[0].src, SymbolId::of("web/page.ts", "fetchIt"));
    }

    #[test]
    fn an_edge_the_server_does_not_cover_is_never_contradicted() {
        // page.ts → target.ex is a cross-language edge (a GraphQL join, in practice).
        // The Elixir server was never asked about it, so its silence isn't denial.
        let mut f = fixture();
        run_reconcile(&mut f, &["api/caller.ex"], OnDenial::Drop);
        let ts_edge = f
            .out
            .edges
            .iter()
            .find(|e| e.src == SymbolId::of("web/page.ts", "fetchIt"))
            .expect("cross-language edge survives");
        assert_eq!(ts_edge.confidence, 0.9);
        assert_eq!(f.out.contradicted, 0);
    }

    #[test]
    fn a_server_only_edge_is_added_below_the_extracted_band() {
        let mut f = fixture();
        let theirs: HashSet<SymbolId> = [
            SymbolId::of("api/caller.ex", "call_it"),
            SymbolId::of("api/other.ex", "also_calls"),
        ]
        .into_iter()
        .collect();
        reconcile_with(&mut f, &theirs, OnDenial::Report);
        assert_eq!(
            (f.out.confirmed, f.out.added, f.out.contradicted),
            (1, 1, 0)
        );
        let added = f
            .out
            .edges
            .iter()
            .find(|e| e.src == SymbolId::of("api/other.ex", "also_calls"))
            .expect("added edge");
        assert_eq!(
            added.confidence, CONF_SERVER_ONLY,
            "a server-only claim is not as good as an agreement"
        );
        assert_eq!(added.source, EdgeSource::LspVerified);
    }

    fn site(name: &str, call_lines: &[u32]) -> lsp::CallSite {
        lsp::CallSite {
            path: std::path::PathBuf::from("/repo/api/caller.ex"),
            name: name.to_owned(),
            line: 10,
            call_lines: call_lines.to_vec(),
        }
    }

    /// A file with one two-clause function (20-24, 30-34) and one single-clause
    /// function (40-44), plus the module node that spans the whole file.
    fn spanned_graph() -> InMemoryGraph {
        let mut clauses = node("api/caller.ex", "call_it");
        clauses.span = Span {
            start_line: 20,
            start_col: 1,
            end_line: 24,
            end_col: 1,
        };
        clauses.extra_spans = vec![Span {
            start_line: 30,
            start_col: 1,
            end_line: 34,
            end_col: 1,
        }];
        let mut other = node("api/caller.ex", "also_calls");
        other.span = Span {
            start_line: 40,
            start_col: 1,
            end_line: 44,
            end_col: 1,
        };
        let mut module = node("api/caller.ex", "Api.Caller");
        module.kind = NodeKind::Class;
        module.span = Span {
            start_line: 1,
            start_col: 1,
            end_line: 99,
            end_col: 1,
        };
        InMemoryGraph::from_parts(vec![clauses, other, module], Vec::new())
    }

    #[test]
    fn a_call_is_attributed_by_position_including_later_clauses() {
        let g = spanned_graph();
        let call_it = SymbolId::of("api/caller.ex", "call_it");
        for line in [21, 32] {
            assert_eq!(
                attribute(
                    &g,
                    "api/caller.ex",
                    &site("whatever_the_server_said", &[line])
                ),
                vec![Attribution::Symbol(call_it)],
                "line {line} is inside a clause of call_it, whatever the server named"
            );
        }
    }

    #[test]
    fn a_call_outside_every_function_is_not_credited_to_the_module() {
        // the module node spans the whole file, so trusting containment alone would
        // report "the module called it" and hide the issue-#18 gap
        let g = spanned_graph();
        assert_eq!(
            attribute(&g, "api/caller.ex", &site("also_calls", &[7])),
            vec![Attribution::OutsideAnySymbol]
        );
    }

    #[test]
    fn each_call_site_is_attributed_separately() {
        let g = spanned_graph();
        let got = attribute(&g, "api/caller.ex", &site("call_it", &[21, 42, 7]));
        assert_eq!(
            got,
            vec![
                Attribution::Symbol(SymbolId::of("api/caller.ex", "call_it")),
                Attribution::Symbol(SymbolId::of("api/caller.ex", "also_calls")),
                Attribution::OutsideAnySymbol,
            ]
        );
    }

    #[test]
    fn without_call_positions_the_servers_naming_is_used() {
        // a server that sends no fromRanges leaves nothing to check
        let g = spanned_graph();
        assert_eq!(
            attribute(&g, "api/caller.ex", &site("call_it/2", &[])),
            vec![Attribution::Symbol(SymbolId::of(
                "api/caller.ex",
                "call_it"
            ))]
        );
        assert!(attribute(&g, "api/caller.ex", &site("nope", &[])).is_empty());
    }

    #[test]
    fn focus_is_the_seed_files_plus_their_callers() {
        let f = fixture();
        let seeds = vec![SymbolId::of("api/target.ex", "run")];
        let focus = focus_files(&f.graph, &seeds);
        assert_eq!(
            focus.iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["api/caller.ex", "api/target.ex", "web/page.ts"]
        );
    }

    #[test]
    fn server_names_reduce_to_ripples() {
        assert_eq!(bare_name("changeset/2"), "changeset");
        assert_eq!(
            bare_name("FiveNoobs.Players.PlayerReport.changeset"),
            "changeset"
        );
        assert!(!is_callable_name("schema players"));
        assert!(is_callable_name("changeset"));
    }
}
