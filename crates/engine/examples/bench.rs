//! Quick indexing benchmark: `cargo run --release -p engine --example bench -- <path>...`
//! Reports graph size and wall-time per root (median of a few runs).

use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let roots: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    if roots.is_empty() {
        eprintln!("usage: bench <path>...");
        std::process::exit(2);
    }

    for root in &roots {
        // warm once (page cache), then time 5 runs, keep the median.
        let _ = engine::index(root);
        let mut times = Vec::new();
        let mut last_nodes = 0usize;
        let mut last_edges = 0usize;
        for _ in 0..5 {
            let t = Instant::now();
            let g = engine::index(root).expect("index failed");
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            last_nodes = g.node_count();
            last_edges = g.edges().count();
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = times[times.len() / 2];
        println!(
            "{:<40} {:>7} nodes {:>7} edges   {:>8.1} ms (median of 5)",
            root.display(),
            last_nodes,
            last_edges,
            median,
        );
    }
}
