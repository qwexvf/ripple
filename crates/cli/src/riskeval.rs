//! Does the risk score rank the files that actually break? — issue #19.
//!
//! `W_CHURN`/`W_BUG`/`W_OWN`/`W_FANOUT` are hand-set constants with no empirical
//! basis. Fitting them needs a label, and the label has to be independent of what
//! the score is made of, or the exercise just confirms itself.
//!
//! The label here is **which files a held-out fix commit touched**. Risk is computed
//! from training history only (commits older than the test window), so a file's score
//! cannot have seen the commit it is being judged against. Fanout comes from the
//! graph rather than history, so it leaks nothing either way.
//!
//! This reports; it does not tune. A weight change should be its own commit, with the
//! number that justified it.

use std::collections::{HashMap, HashSet};

/// One candidate scoring function, and how well it ranked.
pub struct Scored {
    name: &'static str,
    /// share of the top-decile files that a held-out fix commit touched
    precision_at_10: f32,
    precision_at_25: f32,
    /// precision@25 ÷ base rate: 1.0 means the score is no better than picking files
    /// at random, which is the result worth knowing
    lift_at_25: f32,
}

/// A file's terms, all from training data or the graph — never from the test window.
pub struct Terms {
    path: String,
    churn: f32,
    bug_density: f32,
    ownership: f32,
    fanout: f32,
    /// the git-only blend the overlay stores (churn/bug/ownership)
    composite: f32,
    /// the blend `ripple risk` actually prints — the git terms plus fanout, at the
    /// shipped weights. Measuring only the git-only composite would have judged a
    /// score nobody sees.
    shipped: f32,
}

/// Rank indexed files by each risk term and see which ones anticipate the fixes in a
/// held-out window.
fn report(terms: &[Terms], fixed: &HashSet<String>) -> Vec<Scored> {
    let base = if terms.is_empty() {
        0.0
    } else {
        fixed.len() as f32 / terms.len() as f32
    };
    let mut out = Vec::new();
    for (name, pick) in [
        ("churn", (|t: &Terms| t.churn) as fn(&Terms) -> f32),
        ("bug_density", |t: &Terms| t.bug_density),
        ("ownership", |t: &Terms| t.ownership),
        ("fanout", |t: &Terms| t.fanout),
        ("composite/git", |t: &Terms| t.composite),
        ("composite/shipped", |t: &Terms| t.shipped),
    ] {
        let precision = |frac: f32| precision_at(terms, fixed, pick, frac);
        let p25 = precision(0.25);
        out.push(Scored {
            name,
            precision_at_10: precision(0.10),
            precision_at_25: p25,
            lift_at_25: if base > 0.0 { p25 / base } else { 0.0 },
        });
    }
    out
}

/// Share of the top `frac` of files by `pick` that a held-out fix commit touched.
/// Ties break on path, so the number is reproducible.
fn precision_at(
    terms: &[Terms],
    fixed: &HashSet<String>,
    pick: fn(&Terms) -> f32,
    frac: f32,
) -> f32 {
    let mut ranked: Vec<&Terms> = terms.iter().collect();
    ranked.sort_by(|a, b| pick(b).total_cmp(&pick(a)).then(a.path.cmp(&b.path)));
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let k = ((terms.len() as f32 * frac).round() as usize).max(1);
    let top = &ranked[..k.min(ranked.len())];
    let hits = top.iter().filter(|t| fixed.contains(&t.path)).count();
    hits as f32 / top.len() as f32
}

/// The weighting that would have ranked best, by grid search over the four terms.
///
/// Reported next to the current weights so the gap is visible. Uses a plain weighted
/// mean rather than `overlay::blend`'s renormalisation, because every term here has
/// variance by construction — the files without history are excluded before this.
fn best_weights(terms: &[Terms], fixed: &HashSet<String>) -> (f32, [f32; 4]) {
    let steps = [0.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6];
    let mut best = (0.0, [0.0; 4]);
    for &wc in &steps {
        for &wb in &steps {
            for &wo in &steps {
                for &wf in &steps {
                    let sum = wc + wb + wo + wf;
                    if sum <= 0.0 {
                        continue;
                    }
                    let blended: Vec<Terms> = terms
                        .iter()
                        .map(|t| Terms {
                            path: t.path.clone(),
                            churn: t.churn,
                            bug_density: t.bug_density,
                            ownership: t.ownership,
                            fanout: t.fanout,
                            shipped: t.shipped,
                            composite: (wc * t.churn
                                + wb * t.bug_density
                                + wo * t.ownership
                                + wf * t.fanout)
                                / sum,
                        })
                        .collect();
                    let p = precision_at(&blended, fixed, |t| t.composite, 0.25);
                    if p > best.0 {
                        best = (p, [wc, wb, wo, wf]);
                    }
                }
            }
        }
    }
    best
}

/// Build the per-file terms from a training overlay plus graph fanout.
///
/// A file with no training history has no score to judge, so it is excluded and
/// counted — reporting a 0.0 for it would quietly reward whichever term happens to
/// treat "unknown" as "safe".
pub fn terms_for(
    risk: &HashMap<String, ir::RiskScores>,
    fanout: &HashMap<String, f32>,
) -> Vec<Terms> {
    let mut out: Vec<Terms> = risk
        .iter()
        .map(|(path, r)| Terms {
            path: path.clone(),
            churn: r.churn,
            bug_density: r.bug_density,
            ownership: r.ownership,
            fanout: fanout.get(path).copied().unwrap_or(0.0),
            composite: r.composite,
            shipped: shipped_blend(r, fanout.get(path).copied().unwrap_or(0.0)),
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// The composite as shipped: git terms plus structural fanout, at `overlay`'s weights.
fn shipped_blend(r: &ir::RiskScores, fanout: f32) -> f32 {
    let (wc, wb, wo, wf) = (
        overlay::W_CHURN,
        overlay::W_BUG,
        overlay::W_OWN,
        overlay::W_FANOUT,
    );
    (wc * r.churn + wb * r.bug_density + wo * r.ownership + wf * fanout) / (wc + wb + wo + wf)
}

/// Score one candidate weighting, so a fit found on one window can be graded on
/// another. Fitting and grading on the same window only measures the grid search.
pub fn score_weights(terms: &[Terms], fixed: &HashSet<String>, w: [f32; 4]) -> (f32, f32) {
    let sum: f32 = w.iter().sum();
    if sum <= 0.0 {
        return (0.0, 0.0);
    }
    let blended: Vec<Terms> = terms
        .iter()
        .map(|t| Terms {
            path: t.path.clone(),
            churn: t.churn,
            bug_density: t.bug_density,
            ownership: t.ownership,
            fanout: t.fanout,
            composite: t.composite,
            shipped: (w[0] * t.churn + w[1] * t.bug_density + w[2] * t.ownership + w[3] * t.fanout)
                / sum,
        })
        .collect();
    let base = if terms.is_empty() {
        0.0
    } else {
        fixed.len() as f32 / terms.len() as f32
    };
    let p = precision_at(&blended, fixed, |t| t.shipped, 0.25);
    (p, if base > 0.0 { p / base } else { 0.0 })
}

/// Print the whole comparison.
pub fn print(terms: &[Terms], fixed: &HashSet<String>, test_commits: usize, unscored: usize) {
    let base = if terms.is_empty() {
        0.0
    } else {
        fixed.len() as f32 / terms.len() as f32
    };
    println!(
        "risk vs held-out fixes ({test_commits} test commits, {} scorable files, \
         {} touched by a fix):",
        terms.len(),
        fixed.len()
    );
    println!("  base rate (pick a file at random) : {:.1}%", 100.0 * base);
    println!("  term               p@10%   p@25%   lift@25%");
    for s in report(terms, fixed) {
        println!(
            "  {:<17} {:>5.1}%  {:>5.1}%   {:>5.2}×",
            s.name,
            100.0 * s.precision_at_10,
            100.0 * s.precision_at_25,
            s.lift_at_25
        );
    }
    let (p, w) = best_weights(terms, fixed);
    println!(
        "  best grid weights: churn {:.1} bug {:.1} own {:.1} fanout {:.1} → p@25% {:.1}%",
        w[0],
        w[1],
        w[2],
        w[3],
        100.0 * p
    );
    if unscored > 0 {
        println!(
            "  excluded: {unscored} indexed files with no training history — unscorable, \
             not low-risk"
        );
    }
}
