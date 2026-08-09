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
        // a boundary call is a call: the process it crosses does not make the
        // dependency weaker, and the uncertainty about *which* handler already
        // lives in the edge's confidence
        EdgeKind::Calls | EdgeKind::GraphqlCall | EdgeKind::HttpCall => 1.0,
        // a reference is a real dependency even when it is a type mention rather
        // than a call; what is uncertain about it lives in the edge's confidence
        // (servers that can only answer `references` supply it at 0.7), so it is
        // not discounted twice here
        EdgeKind::References => 0.9,
        EdgeKind::DbQuery | EdgeKind::Implements | EdgeKind::Extends => 0.9,
        // a handler is unreachable if the declaration that mounts it changes; the
        // dependency is as real as a call, and what is uncertain lives in the
        // edge's confidence
        EdgeKind::Serves => 0.9,
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

/// Is this symbol part of the test side — something with a `Tests` edge going out?
fn is_test_side(graph: &InMemoryGraph, id: SymbolId) -> bool {
    graph
        .out_edges(id)
        .iter()
        .any(|e: &Edge| e.kind == EdgeKind::Tests)
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
    /// lines of this symbol the diff touched
    pub changed_lines: u32,
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

/// How many of a symbol's lines the diff touched, across every definition site.
fn changed_within(sym: &Node, ranges: &[(u32, u32)]) -> u32 {
    sym.definition_spans()
        .map(|span| {
            ranges
                .iter()
                .map(|&(rs, re)| {
                    let lo = rs.max(span.start_line);
                    let hi = re.min(span.end_line);
                    hi.saturating_sub(lo).saturating_add(u32::from(hi >= lo))
                })
                .sum::<u32>()
        })
        .sum()
}

/// What share of the symbol this diff rewrote, 0..1.
///
/// Every other term in the ranking looks backwards — dependents, churn,
/// bug-density — so a function *added* by the diff under review scores at the
/// floor no matter how much logic it carries. On ripple's own v0.1.2..v0.2.0 that
/// put the largest new function at rank 11 of 37, under a one-line registry entry
/// with a big blast radius (#42). This is the one term that reads the change
/// itself: rewriting a whole function is worth twice a one-line touch of it.
fn rewrite_share(sym: &Node, ranges: &[(u32, u32)]) -> f32 {
    let lines: u32 = sym
        .definition_spans()
        .map(|s| s.end_line.saturating_sub(s.start_line).saturating_add(1))
        .sum();
    (changed_within(sym, ranges) as f32 / lines.max(1) as f32).min(1.0)
}

/// Rank the symbols touched by a diff (`changed`: file → changed line ranges,
/// keyed by module_path) for review, by risk × downstream blast radius × how much
/// of the symbol the diff rewrote. Also surfaces missing-co-change and untested
/// changes. See docs/06-risk-and-queries.md.
pub fn review_focus(
    graph: &InMemoryGraph,
    changed: &HashMap<String, Vec<(u32, u32)>>,
    budget: usize,
    scope: &str,
) -> ReviewResult {
    // changed symbols = def nodes any of whose definition sites overlaps a changed
    // range. Several sites is ordinary code — Elixir's multi-clause functions,
    // reopened classes — and reading only the primary span made editing the second
    // clause drop the function from the review entirely.
    let mut changed_syms: Vec<Node> = Vec::new();
    for node in graph.nodes() {
        if let Some(ranges) = changed.get(&node.module_path) {
            let overlaps = node.definition_spans().any(|sp| {
                ranges
                    .iter()
                    .any(|&(rs, re)| sp.start_line <= re && sp.end_line >= rs)
            });
            if overlaps {
                changed_syms.push(node.clone());
            }
        }
    }

    // a graph with no Tests edge cannot tell tested from untested, and flagging
    // every row is the same as flagging none (#36). Asked per root: in a multi-root
    // index one repo's tests used to answer for a repo that has none.
    let tests_known = graph
        .edges()
        .filter(|e| e.kind == EdgeKind::Tests)
        .any(|e| {
            graph
                .get(e.src)
                .is_some_and(|n| n.module_path.starts_with(scope))
        });

    let mut focus = Vec::new();
    let mut untested = Vec::new();
    for sym in &changed_syms {
        let downstream = impact(graph, &[sym.id], 200).hits;
        // a test that breaks is how you find out, not damage — the same rule
        // `overlay::score_structure` applies to fanout, so the two agree (#42).
        // The hit stays in `impact`'s own answer: "your test will break" is worth
        // knowing, it just isn't reach.
        let down_weight: f32 = downstream
            .iter()
            .filter(|h| !is_test_side(graph, h.node.id))
            .map(|h| h.weight)
            .sum();
        let empty = Vec::new();
        let ranges = changed.get(&sym.module_path).unwrap_or(&empty);
        let changed_lines = changed_within(sym, ranges);
        let review_priority = (1.0 + sym.risk.composite)
            * (1.0 + down_weight.ln_1p())
            * (1.0 + (changed_lines as f32).ln_1p() * (0.5 + 0.5 * rewrite_share(sym, ranges)));

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
        if changed_lines > 0 {
            reasons.push(format!("{changed_lines} lines changed"));
        }
        if tests_known && !has_test_edge(graph, sym.id) {
            reasons.push("untested".into());
            untested.push(sym.clone());
        }

        focus.push(FocusItem {
            node: sym.clone(),
            review_priority,
            downstream: downstream.len(),
            changed_lines,
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

// ── locate: natural-language task → entrypoint seeds ───────────────────────

/// One place to start work, with why it surfaced and what it touches.
pub struct Seed {
    pub node: Node,
    /// Which field carried each matched task word (`route:login`, `name:verify`),
    /// so the agent gets the reason, not just the rank.
    pub why: Vec<String>,
    /// Dependents (in-degree) — a cheap, stable proxy for how central the symbol is.
    pub centrality: usize,
    /// Summed lexical match strength across the task words.
    pub lexical: u32,
    /// One-hop blast-radius preview: the top dependents of this seed, so an agent
    /// sees "start here, and this is what it touches" without a second call.
    pub touches: Vec<Touch>,
}

/// A dependent shown in a seed's blast preview.
pub struct Touch {
    pub node: Node,
    pub via: EdgeKind,
}

/// Ranked entrypoints for a task, and how many candidates the ranking chose among.
pub struct Located {
    pub seeds: Vec<Seed>,
    /// Candidates with a non-zero text match, before the budget cut.
    pub total: usize,
    /// The budget cut fell inside a run of equally-ranked candidates: the tail is
    /// arbitrary among them, so an agent should widen rather than trust the order.
    pub ambiguous: bool,
}

/// Split identifiers and prose into lowercase word tokens — on non-alphanumerics
/// (`review_focus`, `auth/login`) and on camelCase humps (`getUserId` →
/// `get user id`) — then strip a trailing plural `s` so "tokens" reaches `token`.
pub fn tokenize(s: &str) -> Vec<String> {
    split_words(s).iter().map(|w| singular(w)).collect()
}

/// The word split without stemming — on non-alphanumerics and camelCase humps,
/// lowercased. `tokenize` stems each of these; `locate` keeps the unstemmed form
/// to name the reason (`name:focus`, not the clipped `name:focu`).
fn split_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            if ch.is_uppercase() && prev_lower && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            cur.extend(ch.to_lowercase());
            prev_lower = ch.is_lowercase();
        } else {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Grammatical filler and the canonical "implement X" verb — words a task sentence
/// carries that name no code. Left in, `a`/`the`/`into` substring-match almost every
/// symbol, drowning the ranking and inflating the candidate count. Code verbs
/// (`get`, `set`, `add`, `create`) are deliberately *not* here: they are real names.
const STOPWORDS: &[&str] = &[
    "a",
    "an",
    "the",
    "to",
    "of",
    "in",
    "on",
    "at",
    "by",
    "for",
    "and",
    "or",
    "as",
    "is",
    "are",
    "be",
    "it",
    "its",
    "this",
    "that",
    "with",
    "into",
    "from",
    "after",
    "before",
    "when",
    "where",
    "how",
    "what",
    "which",
    "then",
    "than",
    "so",
    "if",
    "no",
    "not",
    "implement",
    "please",
];

/// Is this a task word worth matching? Drops stopwords and one-character tokens
/// (a lone `a`/`s` substring-matches everything and means nothing).
fn is_signal(word: &str) -> bool {
    word.len() >= 2 && !STOPWORDS.contains(&word)
}

/// Trivial de-pluralization: drop a trailing `s` on a word long enough not to be
/// an initialism, but keep `ss` (class, address). Deterministic, no model.
fn singular(word: &str) -> String {
    if word.len() > 3 && word.ends_with('s') && !word.ends_with("ss") {
        word[..word.len() - 1].to_owned()
    } else {
        word.to_owned()
    }
}

/// Score `node` against task `terms`, recording which field carried each hit.
/// Zero with no reasons means drop it. A route word weighs most (a URL is a
/// deliberate name for a feature), then the symbol name, then module/qualified,
/// then prose in the doc comment.
fn locate_score(node: &Node, terms: &[(String, String)]) -> (u32, Vec<String>) {
    let name_tokens = tokenize(&node.name);
    let module_tokens = tokenize(&node.module_path);
    let route_tokens = node.route_path.as_deref().map(tokenize).unwrap_or_default();
    let doc_tokens = node.doc.as_deref().map(tokenize).unwrap_or_default();
    let name_lower = node.name.to_lowercase();
    let qualified_lower = node.qualified_name.to_lowercase();

    let mut score = 0u32;
    let mut why: Vec<String> = Vec::new();
    for (t, raw) in terms {
        let (pts, field) = if route_tokens.iter().any(|w| w == t) {
            (5, "route")
        } else if name_tokens.iter().any(|w| w == t) {
            (4, "name")
        } else if name_lower.contains(t.as_str()) {
            (2, "name")
        } else if module_tokens.iter().any(|w| w == t) || qualified_lower.contains(t.as_str()) {
            (2, "module")
        } else if doc_tokens.iter().any(|w| w == t) {
            (1, "doc")
        } else {
            (0, "")
        };
        if pts > 0 {
            score += pts;
            why.push(format!("{field}:{raw}"));
        }
    }
    (score, why)
}

/// Rank starting points for a natural-language task across the whole graph. Text
/// recall is fused with graph centrality and risk by Reciprocal Rank Fusion, so a
/// word landing on a central, risky symbol outranks a bare substring hit; the top
/// `budget` seeds each carry a one-hop blast-radius preview. See docs/07.
pub fn locate(graph: &InMemoryGraph, task: &str, budget: usize) -> Located {
    // stemmed for matching, raw kept alongside so a reason reads `name:focus`;
    // stopwords and lone letters are dropped so the sentence's grammar is not matched,
    // and a repeated word is counted once so "login login" does not inflate the score
    let mut seen_terms: HashSet<String> = HashSet::new();
    let terms: Vec<(String, String)> = split_words(task)
        .into_iter()
        .filter(|raw| is_signal(raw))
        .map(|raw| (singular(&raw), raw))
        .filter(|(stem, _)| seen_terms.insert(stem.clone()))
        .collect();
    if terms.is_empty() {
        return Located {
            seeds: Vec::new(),
            total: 0,
            ambiguous: false,
        };
    }

    struct Cand<'a> {
        node: &'a Node,
        lexical: u32,
        why: Vec<String>,
        centrality: usize,
    }
    let cands: Vec<Cand> = graph
        .nodes()
        .filter_map(|n| match locate_score(n, &terms) {
            (0, _) => None,
            (lexical, why) => Some(Cand {
                node: n,
                lexical,
                why,
                centrality: graph.in_edges(n.id).len(),
            }),
        })
        .collect();
    let total = cands.len();
    if cands.is_empty() {
        return Located {
            seeds: Vec::new(),
            total: 0,
            ambiguous: false,
        };
    }

    // RRF: each signal contributes 1/(K + rank). K dampens how much the top of any
    // one ranking dominates. Ties are broken by SymbolId inside every sort so the
    // ranks — and therefore the fused order — are reproducible run to run.
    const K: f32 = 60.0;
    let n = cands.len();
    let mut rrf = vec![0f32; n];
    let tie = |a: &Cand, b: &Cand| a.node.id.0.cmp(&b.node.id.0);

    // Competition ranking, not ordinal: candidates equal on a signal share the same
    // rank (the position where their run begins), so two indistinguishable matches
    // get an identical fused score — which is what makes a tie detectable instead of
    // silently broken by the sort's own arbitrary order. `cmp` orders (descending),
    // `eq` decides who ties.
    let mut fuse = |cmp: &dyn Fn(&Cand, &Cand) -> Ordering, eq: &dyn Fn(&Cand, &Cand) -> bool| {
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&a, &b| cmp(&cands[a], &cands[b]).then_with(|| tie(&cands[a], &cands[b])));
        let mut rank = 0usize;
        for pos in 0..n {
            if pos > 0 && !eq(&cands[order[pos]], &cands[order[pos - 1]]) {
                rank = pos;
            }
            rrf[order[pos]] += 1.0 / (K + rank as f32);
        }
    };
    fuse(&|a, b| b.lexical.cmp(&a.lexical), &|a, b| {
        a.lexical == b.lexical
    });
    fuse(&|a, b| b.centrality.cmp(&a.centrality), &|a, b| {
        a.centrality == b.centrality
    });
    fuse(
        &|a, b| b.node.risk.composite.total_cmp(&a.node.risk.composite),
        &|a, b| a.node.risk.composite.total_cmp(&b.node.risk.composite) == Ordering::Equal,
    );

    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        rrf[b]
            .total_cmp(&rrf[a])
            .then_with(|| tie(&cands[a], &cands[b]))
    });

    let ambiguous = budget >= 1
        && n > budget
        && (rrf[order[budget - 1]] - rrf[order[budget]]).abs() < f32::EPSILON;

    let seeds = order
        .iter()
        .take(budget)
        .map(|&i| {
            let c = &cands[i];
            // reuse the blast-radius engine, tiny budget — the preview is a teaser,
            // not the full impact an agent runs once it has picked a seed
            let touches = impact(graph, &[c.node.id], 3)
                .hits
                .into_iter()
                .map(|h| Touch {
                    node: h.node,
                    via: h.via,
                })
                .collect();
            Seed {
                node: c.node.clone(),
                why: c.why.clone(),
                centrality: c.centrality,
                lexical: c.lexical,
                touches,
            }
        })
        .collect();

    Located {
        seeds,
        total,
        ambiguous,
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
            doc: None,
            route_path: None,
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

    /// Every other term looks backwards, so a function the diff *adds* scored at
    /// the floor: on ripple's own v0.1.2..v0.2.0 the largest new function ranked
    /// 11 of 37, under a one-line registry entry (#42).
    #[test]
    fn a_rewritten_symbol_outranks_a_one_line_touch_of_its_neighbour() {
        let mut big = node("a.ts", "rewritten");
        big.span.end_line = 60;
        let mut small = node("a.ts", "grazed");
        small.span.start_line = 100;
        small.span.end_line = 160;

        let graph = InMemoryGraph::from_parts(vec![big.clone(), small.clone()], Vec::new());
        // the whole of one, a single line of the other
        let changed = HashMap::from([("a.ts".to_owned(), vec![(1, 60), (100, 100)])]);

        let r = review_focus(&graph, &changed, 10, "");
        let names: Vec<&str> = r.focus.iter().map(|f| f.node.name.as_str()).collect();
        assert_eq!(names, vec!["rewritten", "grazed"]);

        let lines: Vec<u32> = r.focus.iter().map(|f| f.changed_lines).collect();
        assert_eq!(lines, vec![60, 1], "and the count is reported, not implied");
    }

    /// A symbol with several definition sites is ordinary code — Elixir's
    /// multi-clause functions, a reopened class. Reading only the primary span made
    /// editing the second clause drop the function from the review entirely.
    #[test]
    fn a_second_definition_site_is_still_the_same_changed_symbol() {
        let mut multi = node("players.ex", "kind");
        multi.span = ir::Span {
            start_line: 10,
            start_col: 1,
            end_line: 10,
            end_col: 40,
        };
        multi.extra_spans = vec![ir::Span {
            start_line: 20,
            start_col: 1,
            end_line: 20,
            end_col: 40,
        }];
        let graph = InMemoryGraph::from_parts(vec![multi], Vec::new());

        for line in [10, 20] {
            let changed = HashMap::from([("players.ex".to_owned(), vec![(line, line)])]);
            let r = review_focus(&graph, &changed, 10, "");
            assert_eq!(
                r.focus.len(),
                1,
                "editing the clause at line {line} must still name the function"
            );
            assert_eq!(r.focus[0].changed_lines, 1);
        }
    }

    /// One repository's tests used to answer for another's: `tests_known` scanned
    /// the whole multi-root graph, so a repo with no tests at all still had every
    /// row flagged `untested` as if that meant something (#36).
    #[test]
    fn tests_are_known_per_root_not_per_index() {
        let tested = node("web/src/util.ts", "getPath");
        let test_fn = node("web/src/util.test.ts", "runs");
        let untested = node("api/src/client.ts", "send");
        let tests_edge = edge(&test_fn, &tested, EdgeKind::Tests, 0.8);
        let graph = InMemoryGraph::from_parts(vec![tested, test_fn, untested], vec![tests_edge]);

        let web = HashMap::from([("web/src/util.ts".to_owned(), vec![(1, 1)])]);
        assert!(review_focus(&graph, &web, 10, "web/").tests_known);

        let api = HashMap::from([("api/src/client.ts".to_owned(), vec![(1, 1)])]);
        let r = review_focus(&graph, &api, 10, "api/");
        assert!(
            !r.tests_known,
            "the other repo's tests say nothing about this one"
        );
        assert!(
            r.untested.is_empty() && !r.focus[0].reasons.iter().any(|s| s == "untested"),
            "and nothing is flagged on a judgement that cannot be made"
        );
    }

    /// A truncated focus list that reports its own length reads as "the diff
    /// touched this much" — on a real release diff that hid 22 of 37 (#41).
    #[test]
    fn review_reports_how_many_changed_symbols_the_budget_cut() {
        let nodes: Vec<Node> = (0..5).map(|i| node("a.ts", &format!("f{i}"))).collect();
        let graph = InMemoryGraph::from_parts(nodes, Vec::new());
        let changed = HashMap::from([("a.ts".to_owned(), vec![(1, 1)])]);

        let r = review_focus(&graph, &changed, 2, "");
        assert_eq!(r.focus.len(), 2, "budget still truncates");
        assert_eq!(r.total, 5, "and the count survives the truncation");

        let all = review_focus(&graph, &changed, 20, "");
        assert_eq!(all.focus.len(), 5);
        assert_eq!(all.total, 5, "nothing cut, nothing to report");
    }

    #[test]
    fn tokenize_splits_camel_snake_and_strips_plurals() {
        assert_eq!(tokenize("getUserId"), ["get", "user", "id"]);
        assert_eq!(tokenize("auth/login.ts"), ["auth", "login", "ts"]);
        // trailing plural dropped, but `ss` kept
        assert_eq!(tokenize("tokens class"), ["token", "class"]);
        // de-pluralization is crude (it also clips "focus" → "focu"), but it runs
        // on both the query and the field, so an exact token still meets its match
        assert_eq!(tokenize("review_focus"), tokenize("reviewFocus"));
    }

    /// A task word that names a URL must reach the handler even though its function
    /// name says none of the word — that is the whole point of stamping route_path.
    #[test]
    fn locate_reaches_a_handler_through_its_route() {
        let mut handler = node("api/auth.ex", "handle");
        handler.route_path = Some("auth login".to_owned());
        let unrelated = node("api/other.ex", "handle");
        let graph = InMemoryGraph::from_parts(vec![handler.clone(), unrelated], Vec::new());

        let r = locate(&graph, "implement login", 10);
        assert_eq!(r.seeds.len(), 1, "only the routed handler matches 'login'");
        assert_eq!(r.seeds[0].node.id, handler.id);
        assert!(r.seeds[0].why.iter().any(|w| w == "route:login"));
    }

    /// Prose in a doc comment is the weakest but real signal — a symbol named
    /// nothing like the task is still reachable through what it documents.
    #[test]
    fn locate_matches_doc_comment_text() {
        let mut guard = node("api/mw.ts", "mw");
        guard.doc = Some("limits repeated login attempts".to_owned());
        let graph = InMemoryGraph::from_parts(vec![guard.clone()], Vec::new());

        let r = locate(&graph, "login attempts", 10);
        assert_eq!(r.seeds.len(), 1);
        assert_eq!(r.seeds[0].node.id, guard.id);
        assert!(r.seeds[0].why.iter().any(|w| w == "doc:login"));
    }

    /// A task sentence's grammar must not be matched: `a`/`the`/`into` would
    /// substring-hit nearly every symbol and drown the ranking.
    #[test]
    fn locate_ignores_stopwords_and_lone_letters() {
        let real = node("a.ts", "parse");
        // named only with words a task sentence carries; nothing real to match
        let noise = node("a.ts", "the");
        let graph = InMemoryGraph::from_parts(vec![real.clone(), noise.clone()], Vec::new());

        // only "parse" is signal; "the"/"a"/"into" and the lone "x" are dropped
        let r = locate(&graph, "parse the a file into x", 10);
        assert_eq!(r.seeds.len(), 1, "only the real word matched");
        assert_eq!(r.seeds[0].node.id, real.id);

        // a task made entirely of stopwords matches nothing at all
        assert!(locate(&graph, "the a of into", 10).seeds.is_empty());
    }

    /// The determinism invariant: two runs over the same graph agree exactly, so a
    /// diff of two `locate` outputs means something.
    #[test]
    fn locate_ranking_is_deterministic() {
        let nodes: Vec<Node> = (0..20)
            .map(|i| node("a.ts", &format!("login{i}")))
            .collect();
        let graph = InMemoryGraph::from_parts(nodes, Vec::new());

        let a = locate(&graph, "login", 8);
        let b = locate(&graph, "login", 8);
        let ids = |r: &Located| r.seeds.iter().map(|s| s.node.id.0).collect::<Vec<_>>();
        assert_eq!(ids(&a), ids(&b));
    }

    /// A budget that cuts through a run of identical candidates is flagged, and the
    /// total is reported so a thin ranking never reads as a confident one.
    #[test]
    fn locate_declares_a_tied_truncation() {
        // 10 indistinguishable matches: same name shape, no edges, no risk
        let nodes: Vec<Node> = (0..10)
            .map(|i| node("a.ts", &format!("login{i}")))
            .collect();
        let graph = InMemoryGraph::from_parts(nodes, Vec::new());

        let r = locate(&graph, "login", 3);
        assert_eq!(r.seeds.len(), 3);
        assert_eq!(r.total, 10);
        assert!(r.ambiguous, "the cut fell inside a tie");
    }

    /// Centrality breaks a lexical tie: two equally-named matches, the one more
    /// things depend on ranks first, because that is where work usually starts.
    #[test]
    fn locate_prefers_the_more_central_of_two_equal_matches() {
        let hub = node("a.ts", "login");
        let leaf = node("b.ts", "login");
        let d1 = node("a.ts", "d1");
        let d2 = node("a.ts", "d2");
        // d1, d2 depend on hub; nothing depends on leaf
        let graph = InMemoryGraph::from_parts(
            vec![hub.clone(), leaf.clone(), d1.clone(), d2.clone()],
            vec![calls(&d1, &hub), calls(&d2, &hub)],
        );

        let r = locate(&graph, "login", 2);
        assert_eq!(r.seeds[0].node.id, hub.id, "the hub outranks the leaf");
        assert_eq!(r.seeds[0].centrality, 2);
    }
}
