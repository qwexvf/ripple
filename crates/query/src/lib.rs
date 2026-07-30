//! Decision queries over the in-memory graph (v2). `impact` computes a
//! risk-ranked blast radius from a change, as a **bounded weighted diffusion**
//! over dependency edges (reverse direction) — the convergent alternative to the
//! naive path-sum, and the fix for `neighbors --in` fan-out. See docs/06-risk-and-queries.md.

use ir::{Edge, EdgeKind, Node, SymbolId};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use store::InMemoryGraph;

/// One node reached by a change, with how strongly it's impacted.
pub struct ImpactHit {
    pub node: Node,
    /// propagated reachability weight in (0,1]
    pub weight: f32,
    /// weight × (1 + risk.composite) — the ranking key
    pub score: f32,
    pub depth: usize,
    pub via: EdgeKind,
    /// The symbol this hit was reached *from* on its best path.
    ///
    /// Without it a result is a flat list: `depth` and `via` say how far and along
    /// what kind of edge, but not from where, so no client can draw the blast radius
    /// as a graph. Points at a seed when `depth == 1`.
    pub from: SymbolId,
}

/// A blast radius, and what it cost to report it.
pub struct Impact {
    /// Ranked hits, at most `budget` of them.
    pub hits: Vec<ImpactHit>,
    /// How many nodes the diffusion actually reached, before the budget cut.
    /// Reported so a truncated answer never looks like a complete one.
    pub reached: usize,
}

/// Per-hop decay: distant dependents matter less.
const DECAY: f32 = 0.85;
/// Below this propagated weight, stop expanding.
const EPSILON: f32 = 0.02;

/// How strongly a change propagates *backwards* across each edge kind
/// (dependents of the changed symbol). Co-change is the git signal.
fn kind_weight(kind: EdgeKind) -> f32 {
    match kind {
        EdgeKind::Calls | EdgeKind::GraphqlCall => 1.0,
        // a reference is a real dependency even when it is a type mention rather
        // than a call; what is uncertain about it lives in the edge's confidence
        // (servers that can only answer `references` supply it at 0.7), so it is
        // not discounted twice here
        EdgeKind::References => 0.9,
        EdgeKind::DbQuery | EdgeKind::Implements | EdgeKind::Extends => 0.9,
        EdgeKind::Imports => 0.7,
        EdgeKind::ChangesWith => 0.6,
        _ => 0.5,
    }
}

/// Risk-ranked blast radius of changing `seeds`. Returns up to `budget` hits,
/// highest `score` first, with a stable tie-break on SymbolId so the ranking is
/// deterministic. (A tie node's reported `depth`/`via` provenance may vary when
/// two equal-weight paths reach it; the score and order do not.)
pub fn impact(graph: &InMemoryGraph, seeds: &[SymbolId], budget: usize) -> Impact {
    // best propagated weight + (depth, via, from) provenance per reached node
    let mut best: HashMap<SymbolId, (f32, usize, EdgeKind, SymbolId)> = HashMap::new();
    let mut heap: BinaryHeap<QItem> = BinaryHeap::new();
    for &s in seeds {
        heap.push(QItem {
            weight: 1.0,
            id: s,
            depth: 0,
        });
    }

    while let Some(QItem { weight, id, depth }) = heap.pop() {
        // dependents = edges pointing INTO `id`
        for e in graph.in_edges(id) {
            let w = weight * e.confidence.max(0.0) * kind_weight(e.kind) * DECAY;
            if w < EPSILON {
                continue;
            }
            let improved = best.get(&e.src).is_none_or(|&(bw, ..)| w > bw);
            if improved {
                best.insert(e.src, (w, depth + 1, e.kind, id));
                heap.push(QItem {
                    weight: w,
                    id: e.src,
                    depth: depth + 1,
                });
            }
        }
    }

    // drop the seeds themselves, attach nodes + risk, rank
    let seed_set: std::collections::HashSet<SymbolId> = seeds.iter().copied().collect();
    let mut hits: Vec<ImpactHit> = best
        .into_iter()
        .filter(|(id, _)| !seed_set.contains(id))
        .filter_map(|(id, (weight, depth, via, from))| {
            let node = graph.get(id)?.clone();
            let score = weight * (1.0 + node.risk.composite);
            Some(ImpactHit {
                node,
                weight,
                score,
                depth,
                via,
                from,
            })
        })
        .collect();

    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.node.id.0.cmp(&b.node.id.0)) // stable tie-break
    });
    let reached = hits.len();
    hits.truncate(budget);
    Impact { hits, reached }
}

/// Does anything test this symbol? Either direction: which way a `Tests` edge
/// points is the linker's convention, not a fact about the symbol.
fn has_test_edge(graph: &InMemoryGraph, id: SymbolId) -> bool {
    graph
        .in_edges(id)
        .iter()
        .chain(graph.out_edges(id))
        .any(|e: &Edge| e.kind == EdgeKind::Tests)
}

/// The set of module paths reachable from `seeds` within `depth` over the given
/// edge kinds. Used to measure file-level coupling (a file's symbols reach
/// another file's symbols). Static edges link functions/modules, not module↔module.
pub fn reachable_modules(
    graph: &InMemoryGraph,
    seeds: &[SymbolId],
    kinds: &[EdgeKind],
    depth: usize,
) -> std::collections::HashSet<String> {
    use store::Dir;
    let mut set = std::collections::HashSet::new();
    for &s in seeds {
        for h in graph.neighbors(s, Dir::Out, Some(kinds), depth) {
            set.insert(h.node.module_path.clone());
        }
    }
    set
}

// ── path ──────────────────────────────────────────────────────────────────

/// One step of a path: the edge taken, and the node it lands on.
pub struct Step {
    pub edge: Edge,
    pub node: Node,
}

/// A route from one symbol to another, with how much of the claim survives it.
pub struct Route {
    pub steps: Vec<Step>,
    /// product of the edge confidences — a five-hop route of 0.9s is not a 0.9 claim
    pub confidence: f32,
}

/// Routes from `from` to `to` along dependency direction, shortest first.
///
/// This is the question the graph exists to answer and could not: "how does this page
/// reach that table?" needed one `neighbors` call per hop, and the answer had to be
/// assembled by hand — and where a name is ambiguous (`get` is seven resolvers) the
/// hops could not even be attributed to each other.
///
/// Bounded DFS rather than BFS: every route is wanted, not just one per node, and the
/// depth cap is what keeps that finite. Deterministic — edges are expanded in
/// (dst, site) order and routes are ranked by (length, −confidence, ids).
pub fn paths(
    graph: &InMemoryGraph,
    from: SymbolId,
    to: SymbolId,
    max_depth: usize,
    limit: usize,
) -> Vec<Route> {
    let mut found: Vec<Route> = Vec::new();
    let mut on_path: HashSet<SymbolId> = HashSet::new();
    on_path.insert(from);
    let mut steps: Vec<Step> = Vec::new();
    walk(
        graph,
        from,
        to,
        max_depth,
        &mut on_path,
        &mut steps,
        &mut found,
    );

    found.sort_by(|a, b| {
        a.steps
            .len()
            .cmp(&b.steps.len())
            .then(b.confidence.total_cmp(&a.confidence))
            .then_with(|| {
                let ids = |r: &Route| r.steps.iter().map(|s| s.node.id.0).collect::<Vec<_>>();
                ids(a).cmp(&ids(b))
            })
    });
    found.truncate(limit);
    found
}

fn walk(
    graph: &InMemoryGraph,
    at: SymbolId,
    to: SymbolId,
    left: usize,
    on_path: &mut HashSet<SymbolId>,
    steps: &mut Vec<Step>,
    found: &mut Vec<Route>,
) {
    if left == 0 || found.len() >= MAX_ROUTES {
        return;
    }
    let mut out: Vec<&Edge> = graph.out_edges(at).iter().collect();
    out.sort_by(|a, b| {
        a.dst
            .0
            .cmp(&b.dst.0)
            .then(a.site.start_line.cmp(&b.site.start_line))
    });
    for e in out {
        // a co-change edge is a statistical companion, not a route anything travels
        if e.kind == EdgeKind::ChangesWith || !on_path.insert(e.dst) {
            continue;
        }
        if let Some(node) = graph.get(e.dst) {
            steps.push(Step {
                edge: e.clone(),
                node: node.clone(),
            });
            if e.dst == to {
                found.push(Route {
                    confidence: steps.iter().map(|s| s.edge.confidence).product(),
                    steps: steps
                        .iter()
                        .map(|s| Step {
                            edge: s.edge.clone(),
                            node: s.node.clone(),
                        })
                        .collect(),
                });
            } else {
                walk(graph, e.dst, to, left - 1, on_path, steps, found);
            }
            steps.pop();
        }
        on_path.remove(&e.dst);
    }
}

/// Stop enumerating here. A hub symbol can sit on thousands of routes, and nobody
/// reads the four-thousandth — but a silent stop would look like "there are no more".
const MAX_ROUTES: usize = 2000;

// ── review_focus ──────────────────────────────────────────────────────────

/// A changed symbol worth reviewing, with why.
pub struct FocusItem {
    pub node: Node,
    pub review_priority: f32,
    pub downstream: usize, // impacted-node count
    pub reasons: Vec<String>,
}

pub struct ReviewResult {
    pub focus: Vec<FocusItem>,
    /// how many symbols the diff touched before `budget` truncated `focus`. A
    /// caller that doesn't say so reads as "the diff touched this many" (#41).
    pub total: usize,
    /// files that historically co-change with a changed file but are absent here
    /// (CodeScene's "absence of expected change" bug smell)
    pub missing_cochange: Vec<Node>,
    /// changed symbols with no test edge
    pub untested: Vec<Node>,
    /// whether this graph knows about tests at all. False means `untested` is
    /// empty because nothing could be determined, not because everything is
    /// covered — the two read identically otherwise (#36).
    pub tests_known: bool,
}

/// Rank the symbols touched by a diff (`changed`: file → changed line ranges,
/// keyed by module_path) for review, by risk × downstream blast radius. Also
/// surfaces missing-co-change and untested changes. See docs/06-risk-and-queries.md.
pub fn review_focus(
    graph: &InMemoryGraph,
    changed: &HashMap<String, Vec<(u32, u32)>>,
    budget: usize,
) -> ReviewResult {
    // changed symbols = def nodes whose span overlaps a changed range in its file
    let mut changed_syms: Vec<Node> = Vec::new();
    for node in graph.nodes() {
        if let Some(ranges) = changed.get(&node.module_path) {
            let (s, e) = (node.span.start_line, node.span.end_line);
            if ranges.iter().any(|&(rs, re)| s <= re && e >= rs) {
                changed_syms.push(node.clone());
            }
        }
    }

    // a graph with no Tests edge anywhere cannot tell tested from untested, and
    // flagging every row is the same as flagging none (#36)
    let tests_known = graph.edges().any(|e| e.kind == EdgeKind::Tests);

    let mut focus = Vec::new();
    let mut untested = Vec::new();
    for sym in &changed_syms {
        let downstream = impact(graph, &[sym.id], 200).hits;
        let down_weight: f32 = downstream.iter().map(|h| h.weight).sum();
        let review_priority = (1.0 + sym.risk.composite) * (1.0 + down_weight);

        let mut reasons = Vec::new();
        if sym.risk.bug_density > 0.6 {
            reasons.push(format!("high bug-density ({:.2})", sym.risk.bug_density));
        }
        if sym.risk.churn > 0.6 {
            reasons.push(format!("high churn ({:.2})", sym.risk.churn));
        }
        if !downstream.is_empty() {
            reasons.push(format!("{} downstream", downstream.len()));
        }
        if tests_known && !has_test_edge(graph, sym.id) {
            reasons.push("untested".into());
            untested.push(sym.clone());
        }

        focus.push(FocusItem {
            node: sym.clone(),
            review_priority,
            downstream: downstream.len(),
            reasons,
        });
    }
    focus.sort_by(|a, b| {
        b.review_priority
            .total_cmp(&a.review_priority)
            .then(a.node.id.0.cmp(&b.node.id.0))
    });
    let total = focus.len();
    focus.truncate(budget);

    // missing co-change: files co-changing with a changed file but not in the diff
    let mut missing = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for path in changed.keys() {
        let module = SymbolId::module(path);
        for e in graph.out_edges(module) {
            if e.kind == EdgeKind::ChangesWith && e.confidence >= 0.5 {
                if let Some(n) = graph.get(e.dst) {
                    if !changed.contains_key(&n.module_path) && seen.insert(e.dst) {
                        missing.push(n.clone());
                    }
                }
            }
        }
    }

    // deterministic order (both built from HashMap iteration)
    missing.sort_by(|a, b| a.module_path.cmp(&b.module_path));
    untested.sort_by_key(|n| n.id.0);

    ReviewResult {
        focus,
        total,
        missing_cochange: missing,
        untested,
        tests_known,
    }
}

struct QItem {
    weight: f32,
    id: SymbolId,
    depth: usize,
}
impl PartialEq for QItem {
    fn eq(&self, o: &Self) -> bool {
        self.weight == o.weight
    }
}
impl Eq for QItem {}
impl Ord for QItem {
    fn cmp(&self, o: &Self) -> Ordering {
        self.weight.total_cmp(&o.weight) // max-heap on weight
    }
}
impl PartialOrd for QItem {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ir::{NodeKind, Span};

    fn span() -> Span {
        Span {
            start_line: 1,
            start_col: 1,
            end_line: 1,
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
            span: span(),
            extra_spans: Vec::new(),
            is_exported: true,
            risk: ir::RiskScores::default(),
        }
    }

    fn calls(src: &Node, dst: &Node) -> Edge {
        Edge {
            src: src.id,
            dst: dst.id,
            kind: EdgeKind::Calls,
            confidence: 1.0,
            site: span(),
            source: ir::EdgeSource::Extracted,
        }
    }

    fn edge(src: &Node, dst: &Node, kind: EdgeKind, confidence: f32) -> Edge {
        Edge {
            src: src.id,
            dst: dst.id,
            kind,
            confidence,
            site: span(),
            source: ir::EdgeSource::Extracted,
        }
    }

    /// The front-to-DB question: a page reaches a table through a resolver and a
    /// context function, and the route has to come back as one answer with the
    /// confidence of the whole chain — not one `neighbors` call per hop.
    #[test]
    fn a_route_crosses_every_hop_and_multiplies_its_confidence() {
        let page = node("page.tsx", "Page");
        let resolver = node("resolver.ex", "get");
        let context = node("posts.ex", "get_post");
        let schema = node("post.ex", "Post");
        let cochange = node("unrelated.ex", "Unrelated");
        let graph = InMemoryGraph::from_parts(
            vec![
                page.clone(),
                resolver.clone(),
                context.clone(),
                schema.clone(),
                cochange.clone(),
            ],
            vec![
                edge(&page, &resolver, EdgeKind::GraphqlCall, 0.9),
                edge(&resolver, &context, EdgeKind::Calls, 0.9),
                edge(&context, &schema, EdgeKind::DbQuery, 0.85),
                // a companion, not a route: nothing travels a co-change edge
                edge(&page, &cochange, EdgeKind::ChangesWith, 1.0),
                edge(&cochange, &schema, EdgeKind::ChangesWith, 1.0),
            ],
        );

        let routes = paths(&graph, page.id, schema.id, 6, 3);
        assert_eq!(routes.len(), 1, "one real route, and not the co-change one");
        let hops: Vec<&str> = routes[0]
            .steps
            .iter()
            .map(|s| s.node.name.as_str())
            .collect();
        assert_eq!(hops, vec!["get", "get_post", "Post"]);
        assert!(
            (routes[0].confidence - 0.9 * 0.9 * 0.85).abs() < 1e-6,
            "a three-hop route of 0.9s is not a 0.9 claim: {}",
            routes[0].confidence
        );

        // the depth cap is what makes enumeration finite
        assert!(paths(&graph, page.id, schema.id, 2, 3).is_empty());
    }

    /// A hit has to say where it was reached from, and a truncated answer has to say
    /// how much it dropped — otherwise a budgeted result reads as a complete one.
    #[test]
    fn a_hit_names_its_parent_and_a_cut_answer_says_so() {
        let target = node("a.ts", "target");
        let mid = node("b.ts", "mid");
        let outer = node("c.ts", "outer");
        let other = node("d.ts", "other");
        let graph = InMemoryGraph::from_parts(
            vec![target.clone(), mid.clone(), outer.clone(), other.clone()],
            vec![
                calls(&mid, &target),
                calls(&outer, &mid),
                calls(&other, &target),
            ],
        );

        let all = impact(&graph, &[target.id], 20);
        assert_eq!(all.reached, 3);
        assert_eq!(all.hits.len(), 3);

        let parent_of = |name: &str| {
            all.hits
                .iter()
                .find(|h| h.node.name == name)
                .map(|h| h.from)
                .expect("hit")
        };
        assert_eq!(parent_of("mid"), target.id, "depth 1 points at the seed");
        assert_eq!(parent_of("other"), target.id);
        assert_eq!(
            parent_of("outer"),
            mid.id,
            "depth 2 points at the node it was reached through, not the seed"
        );

        let cut = impact(&graph, &[target.id], 1);
        assert_eq!(cut.hits.len(), 1);
        assert_eq!(cut.reached, 3, "the budget must not hide what was reached");
    }

    /// A truncated focus list that reports its own length reads as "the diff
    /// touched this much" — on a real release diff that hid 22 of 37 (#41).
    #[test]
    fn review_reports_how_many_changed_symbols_the_budget_cut() {
        let nodes: Vec<Node> = (0..5).map(|i| node("a.ts", &format!("f{i}"))).collect();
        let graph = InMemoryGraph::from_parts(nodes, Vec::new());
        let changed = HashMap::from([("a.ts".to_owned(), vec![(1, 1)])]);

        let r = review_focus(&graph, &changed, 2);
        assert_eq!(r.focus.len(), 2, "budget still truncates");
        assert_eq!(r.total, 5, "and the count survives the truncation");

        let all = review_focus(&graph, &changed, 20);
        assert_eq!(all.focus.len(), 5);
        assert_eq!(all.total, 5, "nothing cut, nothing to report");
    }
}
