//! Git overlay (v3): mine the repo's history for evolutionary risk signals —
//! churn, bug-density, ownership — and co-change coupling, then materialize them
//! onto IR nodes (`RiskScores`) and `ChangesWith` edges. Language-agnostic:
//! reads `git log`, not ASTs. See docs/06-risk-and-queries.md.
//!
//! File granularity, so it works for any language and even Tier-0 support.
//! Best-effort: no git / shallow clone → an empty overlay, never an error.

use ir::{Edge, EdgeKind, Node, RiskScores, Span, SymbolId};
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

// composite risk weights (git-only terms for now; churn + bugs carry most signal)
const W_CHURN: f32 = 0.4;
const W_BUG: f32 = 0.4;
const W_OWN: f32 = 0.2;

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

/// The changed file sets (index-root-relative) of the most recent `k` non-merge
/// commits, skipping bulk commits. For evaluating blast-radius prediction.
pub fn recent_commit_files(root: &Path, k: usize) -> Vec<Vec<String>> {
    try_recent(root, k).unwrap_or_default()
}

fn try_recent(root: &Path, k: usize) -> Result<Vec<Vec<String>>, git2::Error> {
    let repo = git2::Repository::discover(root)?;
    let workdir = repo
        .workdir()
        .and_then(|w| w.canonicalize().ok())
        .ok_or_else(|| git2::Error::from_str("no workdir"))?;
    let index_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut revwalk = repo.revwalk()?;
    revwalk.push_head()?;
    revwalk.set_sorting(git2::Sort::TIME)?;

    let mut out = Vec::new();
    for oid in revwalk {
        if out.len() >= k {
            break;
        }
        let Ok(commit) = repo.find_commit(oid?) else {
            continue;
        };
        if commit.parent_count() != 1 {
            continue; // skip merges and root
        }
        let files = changed_files(&repo, &commit, &workdir, &index_root);
        if files.len() >= 2 && files.len() <= MAX_FILES_PER_COMMIT {
            out.push(files);
        }
    }
    Ok(out)
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

    let mut raw: HashMap<String, RawMetrics> = HashMap::new();
    let mut pair_shared: HashMap<(String, String), u32> = HashMap::new();

    for oid in revwalk.take(COMMIT_WINDOW) {
        let Ok(oid) = oid else { continue };
        let Ok(commit) = repo.find_commit(oid) else {
            continue;
        };
        if commit.parent_count() != 1 {
            continue; // skip merges (noisy) and the root commit (diffs vs empty tree = all files)
        }
        let is_fix = is_fix(commit.message().unwrap_or(""));
        let author = commit.author().name().unwrap_or("?").to_owned();

        let files = changed_files(&repo, &commit, &workdir, &index_root);
        if files.is_empty() || files.len() > MAX_FILES_PER_COMMIT {
            continue;
        }
        for f in &files {
            let m = raw.entry(f.clone()).or_default();
            m.commits += 1;
            m.fix_commits += u32::from(is_fix);
            m.authors.insert(author.clone());
        }
        // co-change: every unordered pair in this commit
        for i in 0..files.len() {
            for j in (i + 1)..files.len() {
                let (a, b) = order(&files[i], &files[j]);
                *pair_shared.entry((a, b)).or_default() += 1;
            }
        }
    }

    Ok(finalize(raw, pair_shared))
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
        let composite = W_CHURN * churn + W_BUG * bug_density + W_OWN * ownership;
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
            is_exported: false,
            risk: RiskScores::default(),
        }
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
