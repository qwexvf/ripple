//! Decision queries over the in-memory graph (v2). `impact` computes a
//! risk-ranked blast radius from a change, as a **bounded weighted diffusion**
//! over dependency edges (reverse direction) — the convergent alternative to the
//! naive path-sum, and the fix for `neighbors --in` fan-out. See docs/06-risk-and-queries.md.

use ir::{Edge, EdgeKind, Node, SymbolId};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
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

/// True if any impacted hit lacks a test edge (used by review to flag risk).
pub fn untested<'a>(graph: &InMemoryGraph, hits: &'a [ImpactHit]) -> Vec<&'a ImpactHit> {
    hits.iter()
        .filter(|h| {
            !graph
                .in_edges(h.node.id)
                .iter()
                .chain(graph.out_edges(h.node.id))
                .any(|e: &Edge| e.kind == EdgeKind::Tests)
        })
        .collect()
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
    /// files that historically co-change with a changed file but are absent here
    /// (CodeScene's "absence of expected change" bug smell)
    pub missing_cochange: Vec<Node>,
    /// changed symbols with no test edge
    pub untested: Vec<Node>,
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
        let has_test = graph
            .in_edges(sym.id)
            .iter()
            .chain(graph.out_edges(sym.id))
            .any(|e| e.kind == EdgeKind::Tests);
        if !has_test {
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
        missing_cochange: missing,
        untested,
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
}
