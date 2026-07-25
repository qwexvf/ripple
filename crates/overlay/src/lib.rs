//! Git overlay (v3): mine the repo's history for evolutionary risk signals —
//! churn, bug-density, ownership — and co-change coupling, then materialize them
//! onto IR nodes (`RiskScores`) and `ChangesWith` edges. Language-agnostic:
//! reads `git log`, not ASTs. See docs/06-risk-and-queries.md.
//!
//! File granularity, so it works for any language and even Tier-0 support.
//! Best-effort: no git / shallow clone → an empty overlay, never an error.

use ir::{Edge, EdgeKind, EdgeSource, Node, RiskScores, Span, SymbolId};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Only mine this many most-recent commits (keeps big repos bounded).
const COMMIT_WINDOW: usize = 3000;
/// Skip commits touching more files than this (bulk refactors add co-change noise).
const MAX_FILES_PER_COMMIT: usize = 40;
/// Minimum shared commits before a co-change edge is considered.
const MIN_SHARED: u32 = 2;
/// Minimum coupling score to emit a co-change edge.
const MIN_COUPLING: f32 = 0.3;

// composite risk weights. Public so `eval --risk` can score the blend that actually
// ships rather than a copy of it that drifts.
pub const W_CHURN: f32 = 0.4;
pub const W_BUG: f32 = 0.4;
pub const W_OWN: f32 = 0.2;
/// Structural dependents. Weighted comparably to churn: a symbol many things
/// depend on is risky to change even with a calm history — the case the static
/// graph exists to catch.
pub const W_FANOUT: f32 = 0.4;

#[derive(Default)]
struct RawMetrics {
    commits: u32,
    fix_commits: u32,
    authors: HashSet<String>,
}

/// Mined, normalized signals keyed by index-root-relative module path.
#[derive(Default)]
pub struct GitOverlay {
    pub file_risk: HashMap<String, RiskScores>,
    pub cochange: Vec<(String, String, f32)>,
}

/// Mine `root`'s git history. Returns an empty overlay if there's no repo.
pub fn mine(root: &Path) -> GitOverlay {
    try_mine(root).unwrap_or_default()
}

/// Changed line ranges per file (index-root-relative) for a diff. `base = None`
/// diffs the working tree against HEAD ("what you're about to commit"); otherwise
/// against the given rev. Empty on any git error. Used by `review_focus`.
pub fn diff_lines(root: &Path, base: Option<&str>) -> HashMap<String, Vec<(u32, u32)>> {
    try_diff(root, base).unwrap_or_default()
}

/// One held-out commit: what it touched, and whether it looks like a fix.
pub struct TestCommit {
    /// Changed files, index-root-relative.
    pub files: Vec<String>,
    /// Message looks like a fix — the label risk scoring is judged against.
    pub is_fix: bool,
}

/// A train/test split of the repo's history, so co-change can be scored on
/// commits it was never mined from.
#[derive(Default)]
pub struct Holdout {
    /// The held-out commits, newest first.
    pub test: Vec<TestCommit>,
    /// Signals mined strictly from commits *older* than the held-out window.
    pub train: GitOverlay,
    /// How many commits fed `train` (0 = history too short to train on).
    pub train_commits: usize,
}

/// Split `root`'s history at the `k`th most recent eligible commit: those `k`
/// become the test set, everything older is mined into `train`.
///
/// Every commit newer than the split point is withheld from mining, including
/// ones ineligible as test cases (merges, single-file, bulk) — otherwise the
/// co-change score would be measured on commits it learned from, which is
/// exactly the leak this exists to close.
pub fn holdout(root: &Path, k: usize) -> Holdout {
    holdout_at(root, 0, k)
}

/// `holdout`, but skipping the newest `skip` eligible commits first.
///
/// Every "newest k" window contains every smaller one, so comparing k=30 against
/// k=50 compares a window with a subset of itself. `skip` makes two windows genuinely
/// disjoint, which is what fitting on one and grading on the other requires. Skipped
/// commits are excluded from training too — they are newer than the test window.
pub fn holdout_at(root: &Path, skip: usize, k: usize) -> Holdout {
    try_holdout(root, skip, k).unwrap_or_default()
}

fn try_holdout(root: &Path, skip: usize, k: usize) -> Result<Holdout, git2::Error> {
    let repo = git2::Repository::discover(root)?;
    let workdir = repo
        .workdir()
        .and_then(|w| w.canonicalize().ok())
        .ok_or_else(|| git2::Error::from_str("no workdir"))?;
    let index_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut revwalk = repo.revwalk()?;
    revwalk.push_head()?;
    revwalk.set_sorting(git2::Sort::TIME)?;

    let mut test = Vec::new();
    let mut acc = Accum::default();
    let mut train_commits = 0usize;
    let mut skipped = 0usize;
    for oid in revwalk {
        if train_commits >= COMMIT_WINDOW {
            break;
        }
        let Ok(commit) = repo.find_commit(oid?) else {
            continue;
        };
        if commit.parent_count() != 1 {
            continue; // skip merges and root
        }
        let files = changed_files(&repo, &commit, &workdir, &index_root);
        let eligible = files.len() >= 2 && files.len() <= MAX_FILES_PER_COMMIT;
        if skipped < skip {
            skipped += usize::from(eligible);
            continue; // newer than the test window: neither tested nor trained on
        }
        if test.len() < k {
            if eligible {
                test.push(TestCommit {
                    is_fix: is_fix(commit.message().unwrap_or("")),
                    files,
                });
            }
            continue; // inside the holdout window: trains nothing
        }
        if files.is_empty() || files.len() > MAX_FILES_PER_COMMIT {
            continue;
        }
        acc.add(&files, is_fix(commit.message().unwrap_or("")), &commit);
        train_commits += 1;
    }
    Ok(Holdout {
        test,
        train: acc.finalize(),
        train_commits,
    })
}

fn try_diff(
    root: &Path,
    base: Option<&str>,
) -> Result<HashMap<String, Vec<(u32, u32)>>, git2::Error> {
    let repo = git2::Repository::discover(root)?;
    let workdir = repo
        .workdir()
        .and_then(|w| w.canonicalize().ok())
        .ok_or_else(|| git2::Error::from_str("no workdir"))?;
    let index_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

    let old_tree = match base {
        Some(b) => repo.revparse_single(b)?.peel_to_tree()?,
        None => repo.head()?.peel_to_tree()?,
    };
    let mut opts = git2::DiffOptions::new();
    opts.context_lines(0); // hunk ranges = changed lines only, no surrounding context
    let diff = repo.diff_tree_to_workdir_with_index(Some(&old_tree), Some(&mut opts))?;

    let mut map: HashMap<String, Vec<(u32, u32)>> = HashMap::new();
    diff.foreach(
        &mut |_, _| true,
        None,
        Some(&mut |delta, hunk| {
            let added = hunk.new_lines();
            if added == 0 {
                return true; // pure deletion — no added lines to attribute
            }
            if let Some(p) = delta.new_file().path() {
                if let Ok(rel) = workdir.join(p).strip_prefix(&index_root) {
                    let key = rel.to_string_lossy().replace('\\', "/");
                    let start = hunk.new_start();
                    map.entry(key).or_default().push((start, start + added - 1));
                }
            }
            true
        }),
        None,
    )?;
    Ok(map)
}

fn try_mine(root: &Path) -> Result<GitOverlay, git2::Error> {
    let repo = git2::Repository::discover(root)?;
    let workdir = repo
        .workdir()
        .and_then(|w| w.canonicalize().ok())
        .ok_or_else(|| git2::Error::from_str("bare repo has no workdir"))?;
    let index_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

    let mut revwalk = repo.revwalk()?;
    revwalk.push_head()?;
    revwalk.set_sorting(git2::Sort::TIME)?;

    let mut acc = Accum::default();

    for oid in revwalk.take(COMMIT_WINDOW) {
        let Ok(oid) = oid else { continue };
        let Ok(commit) = repo.find_commit(oid) else {
            continue;
        };
        if commit.parent_count() != 1 {
            continue; // skip merges (noisy) and the root commit (diffs vs empty tree = all files)
        }
        let files = changed_files(&repo, &commit, &workdir, &index_root);
        if files.is_empty() || files.len() > MAX_FILES_PER_COMMIT {
            continue;
        }
        acc.add(&files, is_fix(commit.message().unwrap_or("")), &commit);
    }

    Ok(acc.finalize())
}

/// Per-file counters and co-change pair counts, accumulated commit by commit.
#[derive(Default)]
struct Accum {
    raw: HashMap<String, RawMetrics>,
    pair_shared: HashMap<(String, String), u32>,
}

impl Accum {
    fn add(&mut self, files: &[String], is_fix: bool, commit: &git2::Commit) {
        let author = commit.author().name().unwrap_or("?").to_owned();
        for f in files {
            let m = self.raw.entry(f.clone()).or_default();
            m.commits += 1;
            m.fix_commits += u32::from(is_fix);
            m.authors.insert(author.clone());
        }
        // co-change: every unordered pair in this commit
        for i in 0..files.len() {
            for j in (i + 1)..files.len() {
                let (a, b) = order(&files[i], &files[j]);
                *self.pair_shared.entry((a, b)).or_default() += 1;
            }
        }
    }

    fn finalize(self) -> GitOverlay {
        finalize(self.raw, self.pair_shared)
    }
}

/// Normalize raw metrics to [0,1] percentiles and build co-change edges.
fn finalize(
    raw: HashMap<String, RawMetrics>,
    pair_shared: HashMap<(String, String), u32>,
) -> GitOverlay {
    let churn_vals: Vec<f32> = raw.values().map(|m| m.commits as f32).collect();
    let bug_vals: Vec<f32> = raw
        .values()
        .map(|m| ratio(m.fix_commits, m.commits))
        .collect();
    let own_vals: Vec<f32> = raw
        .values()
        .map(|m| 1.0 / m.authors.len().max(1) as f32)
        .collect();

    let mut file_risk = HashMap::with_capacity(raw.len());
    for (path, m) in &raw {
        let churn = percentile(&churn_vals, m.commits as f32);
        let bug_density = percentile(&bug_vals, ratio(m.fix_commits, m.commits));
        let ownership = percentile(&own_vals, 1.0 / m.authors.len().max(1) as f32);
        let composite = blend(&[
            (W_CHURN, churn, informative(&churn_vals)),
            (W_BUG, bug_density, informative(&bug_vals)),
            (W_OWN, ownership, informative(&own_vals)),
        ]);
        file_risk.insert(
            path.clone(),
            RiskScores {
                churn,
                bug_density,
                ownership,
                composite,
                ..Default::default()
            },
        );
    }

    let commit_count = |p: &str| raw.get(p).map_or(0, |m| m.commits);
    let mut cochange = Vec::new();
    for ((a, b), shared) in pair_shared {
        if shared < MIN_SHARED {
            continue;
        }
        let denom = commit_count(&a).min(commit_count(&b)).max(1) as f32;
        let score = shared as f32 / denom;
        if score >= MIN_COUPLING {
            cochange.push((a, b, score.min(1.0)));
        }
    }
    // deterministic order (pair_shared is a HashMap)
    cochange.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    GitOverlay {
        file_risk,
        cochange,
    }
}

/// Write mined signals onto the graph: set each node's risk from its file, and
/// add symmetric `ChangesWith` edges between module nodes. Returns the number of
/// co-change pairs actually applied (both files present in the graph) — this can
/// be far below `overlay.cochange.len()` when indexed files live in a different
/// git repo than the one mined (e.g. a nested frontend repo).
pub fn apply(overlay: &GitOverlay, nodes: &mut [Node], edges: &mut Vec<Edge>) -> usize {
    for node in nodes.iter_mut() {
        if let Some(r) = overlay.file_risk.get(&node.module_path) {
            node.risk = *r;
        }
    }
    let indexed: HashSet<&str> = nodes.iter().map(|n| n.module_path.as_str()).collect();
    let zero = Span {
        start_line: 0,
        start_col: 0,
        end_line: 0,
        end_col: 0,
    };
    let mut applied = 0;
    for (a, b, score) in &overlay.cochange {
        if !indexed.contains(a.as_str()) || !indexed.contains(b.as_str()) {
            continue;
        }
        let (ma, mb) = (SymbolId::module(a), SymbolId::module(b));
        edges.push(edge(ma, mb, *score, zero));
        edges.push(edge(mb, ma, *score, zero));
        applied += 1;
    }
    applied
}

fn edge(src: SymbolId, dst: SymbolId, score: f32, site: Span) -> Edge {
    Edge {
        src,
        dst,
        kind: EdgeKind::ChangesWith,
        confidence: score,
        site,
        source: EdgeSource::CoChange,
    }
}

fn changed_files(
    repo: &git2::Repository,
    commit: &git2::Commit,
    workdir: &Path,
    index_root: &Path,
) -> Vec<String> {
    let Ok(tree) = commit.tree() else {
        return Vec::new();
    };
    let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());
    let Ok(diff) = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for delta in diff.deltas() {
        if let Some(p) = delta.new_file().path() {
            // git path is workdir-relative; re-key to index-root-relative
            if let Ok(rel) = workdir.join(p).strip_prefix(index_root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out
}

fn is_fix(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    ["fix", "bug", "revert", "hotfix", "patch", "regression"]
        .iter()
        .any(|kw| m.contains(kw))
}

fn ratio(num: u32, den: u32) -> f32 {
    if den == 0 {
        0.0
    } else {
        num as f32 / den as f32
    }
}

fn order(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_owned(), b.to_owned())
    } else {
        (b.to_owned(), a.to_owned())
    }
}

/// Percentile rank in [0,1], mapping the minimum value to 0. Uses `count(x < v)`
/// (not `<=`) so files at the low end — e.g. the many files with `bug_density = 0`
/// — score 0, not the fraction-of-ties. `<=` inflated every zero-bug file.
/// Does this signal distinguish anything? A metric with the same value everywhere
/// ranks nothing.
fn informative(values: &[f32]) -> bool {
    let mut it = values.iter();
    let Some(first) = it.next() else { return false };
    it.any(|v| (v - first).abs() > f32::EPSILON)
}

/// Weighted mean over the terms that carry signal, renormalized so the excluded
/// ones don't scale the result down.
///
/// Without this, a metric that is constant across the corpus silently caps the
/// composite: on a single-author repo every file has one author, so `ownership`
/// percentile-ranks to 0 everywhere and its 0.2 weight subtracted a flat 20% from
/// every score. Terms nothing populates yet (`complexity`, `test_proximity`) drop
/// out by the same rule rather than pretending to be a measured zero.
fn blend(terms: &[(f32, f32, bool)]) -> f32 {
    let (sum, weight) = terms
        .iter()
        .filter(|(.., informative)| *informative)
        .fold((0.0, 0.0), |(s, w), (weight, value, _)| {
            (s + weight * value, w + weight)
        });
    if weight == 0.0 {
        return 0.0;
    }
    sum / weight
}

/// Second risk pass, once every edge exists: how many things depend on each
/// symbol. Must run after cross-service linking — those edges are exactly the
/// dependents a purely local pass would miss.
///
/// Returns how many nodes got a non-zero fanout.
pub fn score_structure(nodes: &mut [Node], edges: &[Edge]) -> usize {
    let mut dependents: HashMap<SymbolId, HashSet<SymbolId>> = HashMap::new();
    for e in edges {
        dependents.entry(e.dst).or_default().insert(e.src);
    }
    let counts: Vec<f32> = nodes
        .iter()
        .map(|n| dependents.get(&n.id).map_or(0, HashSet::len) as f32)
        .collect();

    // whether a term carries signal is a property of the corpus, not of one node:
    // a percentile of 0.0 is a real measurement for the least-changed file, and
    // must not be mistaken for "this metric is missing"
    let (fanout_varies, churn_varies, bug_varies, own_varies) = {
        let column =
            |f: fn(&RiskScores) -> f32| -> Vec<f32> { nodes.iter().map(|n| f(&n.risk)).collect() };
        (
            informative(&counts),
            informative(&column(|r| r.churn)),
            informative(&column(|r| r.bug_density)),
            informative(&column(|r| r.ownership)),
        )
    };

    let mut scored = 0;
    for (node, count) in nodes.iter_mut().zip(&counts) {
        node.risk.fanout = percentile(&counts, *count);
        if *count > 0.0 {
            scored += 1;
        }
        let r = node.risk;
        node.risk.composite = blend(&[
            (W_CHURN, r.churn, churn_varies),
            (W_BUG, r.bug_density, bug_varies),
            (W_OWN, r.ownership, own_varies),
            (W_FANOUT, r.fanout, fanout_varies),
        ]);
    }
    scored
}

fn percentile(values: &[f32], v: f32) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let lt = values.iter().filter(|&&x| x < v).count();
    lt as f32 / values.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fix_detection() {
        assert!(is_fix("fix: null deref"));
        assert!(is_fix("Revert broken change"));
        assert!(!is_fix("add feature"));
    }

    #[test]
    fn blend_ignores_terms_that_rank_nothing() {
        // a signal identical everywhere carries no information: excluding it must
        // not scale the score down (single-author repos capped composite at 0.8)
        assert!(!informative(&[0.0, 0.0, 0.0]));
        assert!(!informative(&[]));
        assert!(informative(&[0.0, 0.5]));

        let both = blend(&[(0.4, 1.0, true), (0.2, 0.0, true)]);
        assert!((both - 0.666).abs() < 0.01, "got {both}");
        // same values, but the flat term drops out entirely
        let one = blend(&[(0.4, 1.0, true), (0.2, 0.0, false)]);
        assert!((one - 1.0).abs() < f32::EPSILON, "got {one}");
        assert_eq!(blend(&[(0.4, 1.0, false)]), 0.0, "nothing informative");
    }

    #[test]
    fn structure_pass_scores_dependents_and_reblends() {
        let mut nodes = vec![
            module_node("hub.ts"),
            module_node("leaf.ts"),
            module_node("caller_a.ts"),
            module_node("caller_b.ts"),
        ];
        let (hub, leaf) = (nodes[0].id, nodes[1].id);
        let (a, b) = (nodes[2].id, nodes[3].id);
        let e = |src, dst| Edge {
            src,
            dst,
            kind: EdgeKind::Calls,
            confidence: 1.0,
            site: Span {
                start_line: 1,
                start_col: 1,
                end_line: 1,
                end_col: 1,
            },
            source: EdgeSource::Extracted,
        };
        // two distinct dependents on hub, none on leaf; a duplicate edge must not
        // count twice
        let edges = vec![e(a, hub), e(b, hub), e(a, hub)];

        let scored = overlay_score(&mut nodes, &edges);
        assert_eq!(scored, 1, "only hub has dependents");

        let of = |id: SymbolId| nodes.iter().find(|n| n.id == id).expect("node").risk;
        assert!(of(hub).fanout > of(leaf).fanout, "hub must outrank leaf");
        assert_eq!(of(leaf).fanout, 0.0);
        // with no git signal at all, composite is carried entirely by fanout —
        // previously it would have been 0 for every node
        assert!(of(hub).composite > 0.0, "structural risk alone must score");
        assert_eq!(of(leaf).composite, 0.0);
    }

    fn overlay_score(nodes: &mut [Node], edges: &[Edge]) -> usize {
        score_structure(nodes, edges)
    }

    #[test]
    fn percentile_rank() {
        // count(x < v)/n → minimum maps to 0 (fixes bug-density inflation)
        let vals = vec![1.0, 2.0, 3.0, 4.0];
        assert_eq!(percentile(&vals, 1.0), 0.0); // minimum → 0
        assert_eq!(percentile(&vals, 4.0), 0.75); // 3 of 4 are smaller
                                                  // the acute case: most files have bug_density 0 → they score 0, not the tie fraction
        let bug = vec![0.0, 0.0, 0.0, 0.5];
        assert_eq!(percentile(&bug, 0.0), 0.0);
        assert_eq!(percentile(&[], 5.0), 0.0);
    }

    fn module_node(path: &str) -> Node {
        Node {
            id: SymbolId::module(path),
            kind: ir::NodeKind::Module,
            name: path.to_owned(),
            qualified_name: path.to_owned(),
            module_path: path.to_owned(),
            span: Span {
                start_line: 1,
                start_col: 1,
                end_line: 1,
                end_col: 1,
            },
            extra_spans: Vec::new(),
            is_exported: false,
            risk: RiskScores::default(),
        }
    }

    /// Commit `files` (each written with fresh content) on top of HEAD.
    fn commit(repo: &git2::Repository, n: u32, files: &[&str]) {
        let workdir = repo.workdir().expect("workdir").to_path_buf();
        let mut index = repo.index().expect("index");
        for f in files {
            std::fs::write(workdir.join(f), format!("v{n}\n")).expect("write");
            index.add_path(Path::new(f)).expect("add");
        }
        index.write().expect("write index");
        let tree = repo
            .find_tree(index.write_tree().expect("write_tree"))
            .expect("tree");
        // explicit, increasing timestamps: commits made in the same second sort
        // arbitrarily under Sort::TIME, which would make the split point random
        let sig = git2::Signature::new(
            "t",
            "t@e",
            &git2::Time::new(1_700_000_000 + i64::from(n) * 60, 0),
        )
        .expect("sig");
        let parents = repo
            .head()
            .ok()
            .and_then(|h| h.peel_to_commit().ok())
            .map(|c| vec![c])
            .unwrap_or_default();
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            &format!("c{n}"),
            &tree,
            &parents.iter().collect::<Vec<_>>(),
        )
        .expect("commit");
    }

    #[test]
    fn holdout_trains_only_on_commits_older_than_the_test_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = git2::Repository::init(dir.path()).expect("init");
        commit(&repo, 0, &["seed.ts"]); // root commit: no parent, never eligible
        for n in 1..=3 {
            commit(&repo, n, &["a.ts", "b.ts"]); // training-window coupling
        }
        for n in 4..=5 {
            commit(&repo, n, &["c.ts", "d.ts"]); // inside the holdout window
        }

        let split = holdout(dir.path(), 2);
        assert_eq!(split.test.len(), 2, "the two newest eligible commits");
        assert!(split
            .test
            .iter()
            .all(|c| c.files.contains(&"c.ts".to_owned())));
        assert_eq!(split.train_commits, 3);

        let pairs: Vec<(&str, &str)> = split
            .train
            .cochange
            .iter()
            .map(|(a, b, _)| (a.as_str(), b.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![("a.ts", "b.ts")],
            "c.ts/d.ts is what the test set is scored on — mining it is the leak"
        );
    }

    #[test]
    fn skip_makes_two_windows_disjoint() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = git2::Repository::init(dir.path()).expect("init");
        commit(&repo, 0, &["seed.ts"]);
        for n in 1..=6 {
            commit(
                &repo,
                n,
                &[if n % 2 == 0 { "even.ts" } else { "odd.ts" }, "shared.ts"],
            );
        }

        // every "newest k" window contains the smaller ones, so a fit graded against
        // one of its own subsets grades itself
        let newest = holdout_at(dir.path(), 0, 2);
        let older = holdout_at(dir.path(), 2, 2);
        assert_eq!(newest.test.len(), 2);
        assert_eq!(older.test.len(), 2);
        // c6,c5 vs c4,c3 — the parity of the non-shared file tells them apart
        assert!(newest.test[0].files.contains(&"even.ts".to_owned()));
        assert!(older.test[0].files.contains(&"even.ts".to_owned()));
        assert_eq!(
            older.train_commits, 2,
            "the skipped commits train nothing either — they are newer than the test window"
        );
    }

    #[test]
    fn holdout_reports_an_empty_training_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = git2::Repository::init(dir.path()).expect("init");
        commit(&repo, 0, &["a.ts"]);
        commit(&repo, 1, &["a.ts", "b.ts"]);

        // asking for more test commits than exist leaves nothing to train on;
        // callers need to tell that apart from a real 0%
        let split = holdout(dir.path(), 50);
        assert_eq!(split.test.len(), 1);
        assert_eq!(split.train_commits, 0);
        assert!(split.train.cochange.is_empty());
    }

    #[test]
    fn apply_sets_risk_and_filters_cochange() {
        let mut overlay = GitOverlay::default();
        overlay.file_risk.insert(
            "a.ts".into(),
            RiskScores {
                composite: 0.9,
                churn: 0.9,
                ..Default::default()
            },
        );
        // one pair fully in graph, one pointing at an unindexed file
        overlay.cochange.push(("a.ts".into(), "b.ts".into(), 0.7));
        overlay
            .cochange
            .push(("a.ts".into(), "external.ex".into(), 0.8));

        let mut nodes = vec![module_node("a.ts"), module_node("b.ts")];
        let mut edges = Vec::new();
        let applied = apply(&overlay, &mut nodes, &mut edges);

        // risk materialized onto the matching node
        let a = nodes.iter().find(|n| n.module_path == "a.ts").unwrap();
        assert!((a.risk.composite - 0.9).abs() < 1e-6);
        // only the in-graph pair applied (external.ex dropped), symmetric = 2 edges
        assert_eq!(applied, 1);
        assert_eq!(edges.len(), 2);
        assert!(edges.iter().all(|e| e.kind == EdgeKind::ChangesWith));
    }
}
