//! ripple CLI.
//!   ripple parse <file>            M0: dump extracted symbols
//!   ripple index <path>            M1: build graph → .ripple/graph.redb
//!   ripple neighbors <symbol>      M1: traverse the persisted graph

mod daemon;
mod riskeval;
mod verify;

use anyhow::{bail, Context, Result};
use ir::EdgeKind;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use store::{Dir, GraphStore, InMemoryGraph, RedbStore};
use verify::{bare_name, is_callable_name};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("parse") => cmd_parse(&args[1..]),
        Some("index") => cmd_index(&args[1..]),
        Some("neighbors") => cmd_neighbors(&args[1..]),
        Some("locate") => cmd_locate(&args[1..]),
        Some("impact") => cmd_impact(&args[1..]),
        Some("review") => cmd_review(&args[1..]),
        Some("path") => cmd_path(&args[1..]),
        Some("risk") => cmd_risk(&args[1..]),
        Some("mcp") => cmd_mcp(&args[1..]),
        Some("daemon") => cmd_daemon(&args[1..]),
        Some("eval") => cmd_eval(&args[1..]),
        Some("lsp") => cmd_lsp(&args[1..]),
        Some(other) => bail!("unknown command: {other}\n{USAGE}"),
        None => bail!("{USAGE}"),
    }
}

const USAGE: &str = "usage:\n  ripple parse <file> [--json]\n  ripple index <path>... [--calls lsp [--calls-budget 120s]] [--debug]   (--calls lsp: call edges from a language server; --debug: per-phase timings to stderr)\n  ripple neighbors <symbol> [--in|--out] [--depth N] [--in-file <substr>] [--root <path>] [--json]\n  ripple locate <task words...> [--budget N] [--root <path>] [--json] [--debug]   (where do I start for this task?)\n  ripple impact <symbol>... [--budget N] [--in-file <substr>] [--root <path>] [--json] [--verify lsp] [--sync]\n  ripple review [<base>] [--budget N] [--root <path>] [--json] [--verify lsp] [--sync]\n    --sync   (rebuild from the working tree in memory before answering, so edits since the last index are reflected — no re-index, nothing persisted)\n    --verify lsp [--verify-budget 2s] [--floor-contradicted|--drop-contradicted]  (upgrade call edges from a language server)\n  ripple path <from> <to> [--depth 6] [--limit 3] [--root <path>] [--json]   (how does A reach B?)\n  ripple risk <symbol|file> [--root <path>] [--json]\n  ripple mcp [--root <path>]   (MCP server over stdio for AI agents)\n  ripple daemon [run] [--max-resident 8]   (resident, file-watching index server; systemd-friendly)\n    ripple daemon register <path> | status | stop   (talk to a running daemon over its socket)\n  ripple eval [--commits N] [--skip N] [--weights <spec>] [--root <path>]   (held-out co-change recall)\n    --risk                                        (do the risk terms rank the files a later fix touched?)\n    --review [--budget N] [--cases N] [--converge 0.6] [--escape-days 7] [--max-introducer-files 40]   (does review rank the defective symbol within the change that introduced it? bulk introducers dropped)\n    --vs-grep [--budget N] [--commits N]   (does the blast radius beat grep at predicting co-change?)\n    --oracle lsp [--sample N] [--granularity function|file]   (agree with a language server?)\n  ripple lsp doctor [--root <path>] [--budget 10s] [--json]   (are language servers usable here?)\n  ripple lsp trust [--root <path>]   (allow this repo's own .ripple/lsp.json to launch servers)";

/// Where `root`'s own database would live.
fn own_db_path(root: &Path) -> PathBuf {
    root.join(".ripple").join("graph.redb")
}

/// The name of the pointer a secondary root carries to the shared index.
const INDEX_POINTER: &str = "index-root";

/// The database that answers for `root`.
///
/// `ripple index A B` writes one graph, under A. Standing in B — which is the
/// normal thing to do, since B is a repository you work in — the old rule
/// (`<root>/.ripple/graph.redb`) pointed at a path redb would happily *create*,
/// so every query answered "nothing" from an empty database. Indexing therefore
/// leaves a pointer in each secondary root, and a dangling pointer is an error
/// naming the index that vanished, never a silent fall-through.
fn db_path(root: &Path) -> Result<PathBuf> {
    let own = own_db_path(root);
    let pointer = root.join(".ripple").join(INDEX_POINTER);
    let Ok(text) = std::fs::read_to_string(&pointer) else {
        return Ok(own); // never part of a shared index: answer from its own graph
    };
    // the pointer wins over a local database. A leftover single-root graph next to
    // a pointer is the stale one, and preferring it silently answered from a graph
    // that knows nothing about the other repo
    if own.exists() {
        eprintln!(
            "⚠ {} has its own index as well as a pointer to the shared one — using the shared index. \
             Delete {} if it is stale",
            root.display(),
            own.display()
        );
    }
    let primary = PathBuf::from(text.trim());
    if !primary.is_absolute() {
        bail!(
            "{} holds a relative index pointer ({}), which names a different database \
             from every directory — re-run `ripple index` for both roots",
            pointer.display(),
            primary.display()
        );
    }
    let shared = own_db_path(&primary);
    if !shared.exists() {
        bail!(
            "{} says its index lives at {}, and that database is gone — \
             re-run `ripple index` for both roots",
            root.display(),
            shared.display()
        );
    }
    Ok(shared)
}

/// Edge kinds surfaced by `neighbors` (call/import/co-change/cross-service).
const NEIGHBOR_KINDS: [EdgeKind; 9] = [
    EdgeKind::Calls,
    EdgeKind::References,
    EdgeKind::Imports,
    EdgeKind::ChangesWith,
    EdgeKind::GraphqlCall,
    // a call across an HTTP or pub/sub boundary is a caller like any other; leaving
    // them out made the first HttpCall edges invisible to `neighbors`
    EdgeKind::HttpCall,
    EdgeKind::AsyncCall,
    EdgeKind::DbQuery,
    // a router's handlers are its neighbors even though it calls none of them (#54)
    EdgeKind::Serves,
];

/// Flags that consume the following token as their value, read off `USAGE`.
///
/// Derived rather than declared: the list used to be a second copy that had to be
/// kept in sync by hand, and every value-taking flag added without touching it
/// reintroduced the bug where `--root <path>` leaked its value as a positional
/// (#24). `USAGE` is the one place a flag is written down now.
fn value_flags() -> &'static HashSet<&'static str> {
    static FLAGS: std::sync::OnceLock<HashSet<&'static str>> = std::sync::OnceLock::new();
    FLAGS.get_or_init(|| {
        let mut out = HashSet::new();
        for line in USAGE.lines() {
            let mut tokens = line.split_whitespace().peekable();
            while let Some(tok) = tokens.next() {
                let flag = tok.trim_start_matches('[').trim_end_matches([']', '|']);
                if !flag.starts_with("--") {
                    continue;
                }
                // `[--depth N]` / `[--root <path>]` take a value; `[--json]` does not
                let takes_value = tokens.peek().is_some_and(|next| {
                    let n = next.trim_start_matches('[').trim_end_matches([']', '|']);
                    !n.starts_with("--") && !n.starts_with('(')
                });
                if takes_value {
                    out.insert(flag);
                }
            }
        }
        out
    })
}

/// Positional args, correctly skipping `--flag value` pairs (so `--root X` never
/// leaks `X` as a positional).
fn positionals(args: &[String]) -> Vec<&String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a.starts_with("--") {
            i += if value_flags().contains(a.as_str()) {
                2
            } else {
                1
            };
        } else {
            out.push(a);
            i += 1;
        }
    }
    out
}

fn positional(args: &[String]) -> Option<&String> {
    positionals(args).into_iter().next()
}

fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn cmd_parse(args: &[String]) -> Result<()> {
    let json = args.iter().any(|a| a == "--json");
    let file: PathBuf = positional(args).context(USAGE)?.into();
    let adapter = lang::for_path(&file)
        .with_context(|| format!("no language adapter for {}", file.display()))?;
    let source = std::fs::read_to_string(&file)
        .with_context(|| format!("cannot read {}", file.display()))?;
    let nodes = parse::extract(&source, adapter.as_ref())?;

    if json {
        println!("{}", serde_json::to_string_pretty(&nodes)?);
    } else {
        println!("{} ({} symbols)", file.display(), nodes.len());
        for n in &nodes {
            let exp = if n.is_exported { "export " } else { "" };
            println!(
                "  {exp}{:?} {} @ {}:{}",
                n.kind, n.name, n.span.start_line, n.span.start_col
            );
        }
    }
    Ok(())
}

fn cmd_index(args: &[String]) -> Result<()> {
    if args.iter().any(|a| a == "--debug") {
        ir::timing::force_on();
    }
    let roots: Vec<PathBuf> = {
        let r: Vec<PathBuf> = positionals(args).into_iter().map(PathBuf::from).collect();
        if r.is_empty() {
            vec![".".into()]
        } else {
            r
        }
    };
    let calls = match flag_value(args, "--calls") {
        None => None,
        Some("lsp") => Some(
            parse_duration(flag_value(args, "--calls-budget"))
                .unwrap_or_else(|| std::time::Duration::from_secs(120)),
        ),
        Some(other) => bail!("--calls takes lsp, not {other}"),
    };
    println!("{}", index_project(&roots, calls)?);
    Ok(())
}

/// Build the graph for `roots` (parse + git overlay + cross-service) and persist
/// it. Returns a one-line summary. Shared by `index` and the MCP `reindex` tool.
fn index_project(roots: &[PathBuf], lsp_calls: Option<std::time::Duration>) -> Result<String> {
    let total = ir::timing::start("index total");
    let mut store = RedbStore::open(own_db_path(&roots[0])); // db + cache live under root[0]

    let cached = ir::timing::step("read_cache", || store.read_extracts())?;
    let indexed = ir::timing::step("build_incremental", || {
        resolve::build_incremental(roots, &cached)
    })?;
    let mut nodes = indexed.result.nodes;
    let mut edges = indexed.result.edges;

    // git overlay per root (best-effort), namespaced to match module paths
    let git_span = ir::timing::start("git_overlay_mine");
    let mut git = overlay::GitOverlay::default();
    for (tag, root) in &indexed.roots {
        let o = overlay::mine_cached(root);
        for (k, v) in o.file_risk {
            git.file_risk.insert(resolve::namespace(tag, &k), v);
        }
        for (a, b, s) in o.cochange {
            git.cochange
                .push((resolve::namespace(tag, &a), resolve::namespace(tag, &b), s));
        }
    }
    git_span.stop(git.cochange.len());
    let cochange_span = ir::timing::start("cochange_apply");
    let cochange_applied = overlay::apply(&git, &mut nodes, &mut edges);
    cochange_span.stop(cochange_applied);

    // cross-service: TS→resolver (GraphqlCall), resolver→context (Calls), fn→schema (DbQuery)
    let mut cross = ir::timing::step("cross_service", || {
        resolve::link_cross_service(&indexed.files, &nodes)
    });
    let (graphql, db, imported) = (cross.graphql, cross.db, cross.imported);
    let (unmatched, unused) = (cross.unmatched_consumers, cross.unused_providers);
    let (endpoints, mounted) = (cross.endpoints, cross.mounted);
    let file_granular = cross.file_granular;
    edges.append(&mut cross.edges);

    // stamp each handler with the route it serves, so `locate("login")` reaches the
    // handler through its URL and not only through a call that happens to say "login"
    if !cross.route_paths.is_empty() {
        let mut by_id: HashMap<ir::SymbolId, Vec<String>> = HashMap::new();
        for (id, text) in cross.route_paths.drain(..) {
            let routes = by_id.entry(id).or_default();
            if !routes.contains(&text) {
                routes.push(text);
            }
        }
        for n in &mut nodes {
            if let Some(routes) = by_id.get(&n.id) {
                n.route_path = Some(routes.join(" "));
            }
        }
    }

    // tests last: an Elixir call edge only exists after the cross-service pass, and
    // a test that calls nothing ripple resolved is a test ripple can't see (#36)
    let scopes = resolve::TestScopes::of(&indexed.files, &indexed.roots, &lang::registry());
    let tests_span = ir::timing::start("link_tests");
    let mut test_edges = resolve::link_tests(&scopes, &edges);
    tests_span.stop(test_edges.len());
    let tests = test_edges.len();
    edges.append(&mut test_edges);

    // structural risk needs every edge, including the cross-service ones
    let struct_span = ir::timing::start("score_structure");
    let with_dependents = overlay::score_structure(&mut nodes, &edges);
    struct_span.stop(with_dependents);
    // several call sites between the same pair are several edges. Counted rather than
    // merged: a site is real information (`path` prints its line), and the count is the
    // number that decides whether merging would be worth the loss (#28)
    let repeated = {
        let mut seen: HashSet<(u64, u64, u8)> = HashSet::new();
        edges
            .iter()
            .filter(|e| !seen.insert((e.src.0, e.dst.0, e.kind as u8)))
            .count()
    };

    // which roots the previous index covered, read before it is overwritten: a
    // root dropped from the set keeps a pointer to a graph that no longer knows
    // it, and every query from there fails blaming the wrong thing (#43)
    let dropped: Vec<PathBuf> = {
        let now: HashSet<&PathBuf> = indexed.roots.iter().map(|(_, p)| p).collect();
        store
            .read_roots()?
            .into_iter()
            .map(|(_, p)| p)
            .filter(|p| !now.contains(p))
            .collect()
    };

    // one transaction: a crash between these three would leave the graph, the
    // extract cache and the roots describing different indexes
    ir::timing::step("write_index", || {
        store.write_index(&nodes, &edges, &indexed.files, &indexed.roots)
    })?;
    write_index_pointers(&roots[0], &indexed.roots)?;
    for root in &dropped {
        let _ = std::fs::remove_file(root.join(".ripple").join(INDEX_POINTER));
    }
    total.stop(nodes.len());

    let edge_count = edges.len();
    let calls_report = match lsp_calls {
        Some(budget) => Some(lsp_calls_pass(
            &mut store, &mut nodes, edges, budget, &scopes,
        )?),
        None => None,
    };

    let s = indexed.stats;
    let mut summary = format!(
        "indexed {} files across {} root(s) ({} added, {} changed, {} unchanged, {} removed) → {} nodes, {} edges ({} co-change, {} graphql, {} db, {} imported, {} endpoint, {} mounted, {} tests, {} file-granular, {} repeated, {} with dependents) ({})",
        indexed.result.files_indexed, indexed.roots.len(),
        s.added, s.changed, s.unchanged, s.removed,
        nodes.len(), edge_count, cochange_applied, graphql, db, imported, endpoints, mounted, tests, file_granular, repeated, with_dependents,
        own_db_path(&roots[0]).display()
    );
    // the cross-service diagnostics: a boundary convention nobody taught a detector
    // is invisible in the edge count but loud here (#32)
    if unmatched > 0 || unused > 0 {
        summary.push_str(&format!(
            "\n  cross-service: {unmatched} consumer selection(s) matched no provider, \
             {unused} provider key(s) nothing consumes"
        ));
    }
    if !dropped.is_empty() {
        summary.push_str(&format!(
            "\n⚠ dropped {} root(s) no longer indexed ({}) — their symbols are gone from this graph",
            dropped.len(),
            dropped
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(report) = calls_report {
        summary.push('\n');
        summary.push_str(&report);
    }
    Ok(summary)
}

/// `index --calls lsp`: ask each root's language server for the call edges of
/// files whose language has no refs query, and persist what comes back.
///
/// This is the producer half of docs/11 — everywhere else a server only *grades*
/// edges tree-sitter already found. A Tier-0 language has none to grade, so the
/// pass runs over the whole root at index time rather than over a query's
/// neighborhood, and its answers are stored with `EdgeSource::LspVerified` at the
/// server-only confidence. Storing them is what keeps queries deterministic: the
/// graph a later query reads is a fact on disk, not a re-derivation from a server
/// whose answer moves with its version.
fn lsp_calls_pass(
    store: &mut RedbStore,
    nodes: &mut [ir::Node],
    edges: Vec<ir::Edge>,
    budget: std::time::Duration,
    scopes: &resolve::TestScopes,
) -> Result<String> {
    let graph = InMemoryGraph::from_parts(nodes.to_vec(), edges);
    let focus = verify::server_sourced_files(&graph);
    if focus.is_empty() {
        return Ok(
            "calls lsp: no tier-0 files in this index — nothing a server is the only source for"
                .to_owned(),
        );
    }
    let roots = store.read_roots()?;
    let hashes: std::collections::HashMap<String, String> = store
        .read_extracts()?
        .into_iter()
        .map(|(module, f)| (module, f.hash))
        .collect();
    let cached = store.read_verified()?;
    let plan = verify::Plan {
        focus,
        roots: &roots,
        budget,
        hashes: &hashes,
        cached: &cached,
        // nothing to contradict: a Tier-0 file has no extracted edge for the server
        // to disagree with, so silence can only mean "no callers"
        on_denial: verify::OnDenial::Report,
    };
    let outcome = verify::run(&graph, &plan);
    let report = outcome.summary();
    if !outcome.learned.is_empty() {
        store
            .write_verified(&outcome.learned)
            .context("persisting verified verdicts")?;
    }
    if outcome.changed() {
        // Go and Gleam have no refs query, so their call edges exist only here —
        // without a second pass every symbol in those repos stays "untested" (#36).
        // Re-derive rather than add: the pass carries the first round's Tests edges.
        let mut edges = outcome.edges;
        edges.retain(|e| e.kind != EdgeKind::Tests);
        let fresh = resolve::link_tests(scopes, &edges);
        edges.extend(fresh);
        // and re-score: for a Tier-0 language every call edge in the graph was
        // produced right here, so the fanout computed before this pass was
        // computed without them — the risk terms and the edges disagreed on disk
        overlay::score_structure(nodes, &edges);
        store
            .write(nodes, &edges)
            .context("persisting server-sourced call edges")?;
    }
    Ok(report)
}

/// The tag `index` used for `root`, so a filesystem path can be turned into the
/// module path the graph actually stores. Empty for a single-root index (paths
/// are stored bare) and for an index built before roots were recorded.
fn root_tag(store: &RedbStore, root: &Path) -> Result<String> {
    let roots = store.read_roots()?;
    if roots.is_empty() {
        return Ok(String::new());
    }
    let want = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if let Some((tag, _)) = roots.iter().find(|(_, p)| *p == want) {
        return Ok(tag.clone());
    }
    bail!(
        "{} is not one of the indexed roots ({}) — run `ripple index` for it first",
        want.display(),
        roots
            .iter()
            .map(|(_, p)| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// `ripple lsp <subcommand>`. Only `doctor` for now: report whether this project
/// has usable language servers, so the Tier-2 accuracy layer is never a mystery.
/// See docs/11-lsp-integration.md.
/// Where the user's own server table lives, for messages. Falls back to the
/// documented path when no config directory can be determined.
fn user_config_hint() -> String {
    lsp::user_config_path().map_or_else(
        || "~/.config/ripple/lsp.json".to_owned(),
        |p| p.display().to_string(),
    )
}

/// `ripple lsp trust [--root <path>]`: record a root as one whose own
/// `.ripple/lsp.json` may be obeyed.
///
/// Trust is stored in the user's config directory, never in the repository — a
/// repository that could mark itself trusted would be no protection at all.
fn cmd_lsp_trust(args: &[String]) -> Result<()> {
    let root: PathBuf = flag_value(args, "--root").map_or_else(|| ".".into(), PathBuf::from);
    let (file, already) = lsp::trust(&root)?;
    let canonical = root.canonicalize().unwrap_or(root);
    if already {
        println!(
            "{} is already trusted ({})",
            canonical.display(),
            file.display()
        );
        return Ok(());
    }
    println!("trusted {}", canonical.display());
    println!("  recorded in {}", file.display());
    println!("  its .ripple/lsp.json will now be used, including any command it names");
    Ok(())
}

fn cmd_lsp(args: &[String]) -> Result<()> {
    match positional(args).map(String::as_str) {
        Some("doctor") => cmd_lsp_doctor(&args[1..]),
        Some("trust") => cmd_lsp_trust(&args[1..]),
        Some(other) => bail!("unknown lsp subcommand: {other}\n{USAGE}"),
        None => bail!("{USAGE}"),
    }
}

fn cmd_lsp_doctor(args: &[String]) -> Result<()> {
    let json = args.iter().any(|a| a == "--json");
    let root: PathBuf = flag_value(args, "--root").map_or_else(|| ".".into(), PathBuf::from);
    let root = root
        .canonicalize()
        .with_context(|| format!("cannot access {}", root.display()))?;

    let store = RedbStore::open(db_path(&root)?);
    let graph = store.load().ok();
    // A cross-repo index spans several roots, and each root has its own language
    // mix — the Elixir server belongs to the repo that has mix.exs, not to the one
    // that happens to hold the database.
    let mut roots = store.read_roots().unwrap_or_default();
    if roots.is_empty() {
        roots.push((String::new(), root.clone()));
    }
    let adapters: Vec<String> = lang::registry().iter().map(|a| a.id().to_owned()).collect();
    let config = lsp::load(&root)?;
    let specs = config.specs;

    // one budget for the whole command, split across roots: a hung server must not
    // be able to hold the output back, and every probe is clamped so no handshake
    // outlives the call
    let budget = parse_duration(flag_value(args, "--budget"))
        .unwrap_or_else(|| std::time::Duration::from_secs(10));
    let deadline = std::time::Instant::now() + budget;
    let mut checked = Vec::new();
    for (tag, path) in &roots {
        let indexed = indexed_languages(graph.as_ref(), tag);
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let reports = lsp::probe_all(&specs, path, left);
        checked.push((path.clone(), indexed, reports));
    }

    if json {
        let out: Vec<Value> = checked
            .iter()
            .map(|(path, indexed, reports)| {
                json!({
                    "root": path.display().to_string(),
                    "indexed_languages": indexed,
                    "servers": reports,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "ripple_adapters": adapters,
                "roots": out,
                "untrusted_config": config.untrusted.as_ref().map(|u| json!({
                    "path": u.path.display().to_string(),
                    "declares": u.declares.iter()
                        .map(|(l, c)| json!({"language": l, "command": c}))
                        .collect::<Vec<_>>(),
                })),
            }))?
        );
        return Ok(());
    }

    if let Some(u) = &config.untrusted {
        println!(
            "⚠ ignored {} — a config inside the repository names commands to run,",
            u.path.display()
        );
        println!("  and this root is not trusted. Nothing from it was used:");
        for (language, command) in &u.declares {
            println!("    {language}: {command}");
        }
        println!(
            "  Move it to {} to apply it everywhere,",
            user_config_hint()
        );
        println!("  or run `ripple lsp trust` if you wrote this repository's copy yourself.\n");
    }

    for (path, indexed, reports) in &checked {
        println!("language servers for {}", path.display());
        if graph.is_none() {
            println!("  (no index yet — run `ripple index` to see which languages matter here)");
        } else if indexed.is_empty() {
            println!("  (nothing indexed under this root)");
        } else {
            println!("  indexed languages: {}", indexed.join(", "));
        }

        for r in reports {
            let no_adapter = !adapters.contains(&r.language);
            let matters = indexed.contains(&r.language);
            // a server for a language ripple can't index, in a project that
            // doesn't use it either, is pure noise — skip it
            if no_adapter && matches!(r.health, lsp::Health::NotApplicable) {
                continue;
            }
            println!("\n  {} ({})", r.language, r.command);
            if no_adapter {
                println!(
                    "    note     ripple has no adapter for {} yet, so this server adds nothing today",
                    r.language
                );
            }
            match &r.health {
                lsp::Health::NotApplicable => {
                    let note = if matters {
                        "no root marker here — this language is indexed from another root"
                    } else {
                        "no root marker in this project"
                    };
                    println!("    n/a      {note}");
                }
                lsp::Health::BinaryMissing => {
                    let note = if matters {
                        "not installed — this language is indexed, so its edges stay tree-sitter only"
                    } else {
                        "not installed"
                    };
                    println!("    missing  {note}");
                }
                lsp::Health::Failed { error, log } => {
                    println!("    broken   {error}");
                    for l in log.iter().rev().take(3) {
                        println!("             {l}");
                    }
                }
                lsp::Health::Ready {
                    init_ms,
                    caps,
                    server,
                } => {
                    let name = server.as_deref().unwrap_or("(unnamed)");
                    println!("    ready    {name}, handshake {init_ms}ms");
                    println!(
                        "             callHierarchy={} references={} documentSymbol={} workspaceSymbol={}",
                        caps.call_hierarchy,
                        caps.references,
                        caps.document_symbol,
                        caps.workspace_symbol
                    );
                    let verdict = if !caps.usable_for_calls() {
                        "cannot supply call edges (no callHierarchy or references)"
                    } else if r.inline {
                        "usable inline"
                    } else {
                        "usable, background-warm only"
                    };
                    println!("             {verdict}");
                }
            }
        }
        println!();
    }
    Ok(())
}

/// Languages indexed under one root. Module paths are namespaced by root tag in a
/// multi-root index, so the tag selects the root's own files.
fn indexed_languages(graph: Option<&InMemoryGraph>, tag: &str) -> Vec<String> {
    let Some(graph) = graph else {
        return Vec::new();
    };
    let prefix = if tag.is_empty() {
        String::new()
    } else {
        format!("{tag}/")
    };
    // one registry for the whole scan: `for_path` rebuilds it per call, which on a
    // 44k-node graph is 44k throwaway registries
    let registry = lang::registry();
    let mut ids: Vec<String> = graph
        .nodes()
        .filter(|n| n.module_path.starts_with(&prefix))
        .filter_map(|n| lang::adapter_for(&registry, Path::new(&n.module_path)))
        .map(|a| a.id().to_owned())
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

fn cmd_impact(args: &[String]) -> Result<()> {
    let json = args.iter().any(|a| a == "--json");
    let root: PathBuf = flag_value(args, "--root").map_or_else(|| ".".into(), PathBuf::from);
    let budget: usize = flag_value(args, "--budget")
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let symbols: Vec<&str> = positionals(args).into_iter().map(String::as_str).collect();
    if symbols.is_empty() {
        bail!("{USAGE}");
    }

    let mut store = RedbStore::open(db_path(&root)?);
    let mut graph = load_current(&store, &root, args.iter().any(|a| a == "--sync"))?;
    let mut seeds: Vec<ir::SymbolId> = Vec::new();
    for s in &symbols {
        let matches = lookup_or_bail(&graph, s, json, args)?;
        seeds.extend(matches.iter().map(|n| n.id));
    }
    graph = verify_upgrade(&mut store, graph, &root, args, &seeds, json)?;

    let result = query::impact(&graph, &seeds, budget);
    let hits = &result.hits;
    let mut touched: HashSet<&str> = hits.iter().map(|h| h.node.module_path.as_str()).collect();
    touched.extend(
        seeds
            .iter()
            .filter_map(|id| graph.get(*id))
            .map(|n| n.module_path.as_str()),
    );
    // --sync already rebuilt from the working tree, so the answer is current — the
    // "re-run index" warning would contradict it
    if !args.iter().any(|a| a == "--sync") {
        warn_if_stale(&store, &touched);
    }
    if json {
        let out: Vec<_> = hits
            .iter()
            .map(|h| {
                // `from` makes the result a graph rather than a list: depth and via
                // say how far and along what kind of edge, never from where
                let parent = graph.get(h.from);
                serde_json::json!({
                    "symbol": h.node.name, "module": h.node.module_path,
                    "kind": format!("{:?}", h.node.kind),
                    "score": h.score, "weight": h.weight, "depth": h.depth,
                    "via": format!("{:?}", h.via), "risk": h.node.risk.composite,
                    "from": parent.map(|n| n.name.clone()),
                    "from_module": parent.map(|n| n.module_path.clone()),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "shown": hits.len(),
                "reached": result.reached,
                "hits": out,
            }))?
        );
    } else {
        let cut = result.reached.saturating_sub(hits.len());
        let note = if cut > 0 {
            format!(" — {cut} more cut by --budget {budget}")
        } else {
            String::new()
        };
        println!(
            "blast radius of {} — {} of {} hits (ranked){note}:",
            symbols.join(", "),
            hits.len(),
            result.reached
        );
        for h in hits {
            println!(
                "  {:.2}  {:?}<{:.2}> {}",
                h.score,
                h.via,
                h.weight,
                hit_name(&h.node)
            );
        }
    }
    Ok(())
}

fn cmd_locate(args: &[String]) -> Result<()> {
    if args.iter().any(|a| a == "--debug") {
        ir::timing::force_on();
    }
    let json = args.iter().any(|a| a == "--json");
    let root: PathBuf = flag_value(args, "--root").map_or_else(|| ".".into(), PathBuf::from);
    let budget: usize = flag_value(args, "--budget")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    let task = positionals(args)
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    if task.trim().is_empty() {
        bail!("{USAGE}");
    }

    let store = RedbStore::open(db_path(&root)?);
    let load_span = ir::timing::start("graph_load");
    let graph = store.load()?;
    load_span.stop(graph.node_count());
    let tags = store.read_roots()?;
    let locate_span = ir::timing::start("locate");
    let located = query::locate(&graph, &task, budget);
    locate_span.stop(located.total);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&locate_payload(&located, &tags))?
        );
        return Ok(());
    }

    if located.seeds.is_empty() {
        if located.total > 0 {
            // matches exist; the budget just hid all of them — never let that read
            // as "nothing matched"
            println!(
                "{} candidate(s) for \"{task}\", all cut by --budget {budget}",
                located.total
            );
        } else {
            println!("no starting point matched: {task}");
        }
        return Ok(());
    }
    let cut = located.total.saturating_sub(located.seeds.len());
    let note = if cut > 0 {
        format!(" — {cut} more cut by --budget {budget}")
    } else {
        String::new()
    };
    println!(
        "start here for \"{task}\" — {} of {} candidates{note}:",
        located.seeds.len(),
        located.total
    );
    if located.ambiguous {
        println!("  (many candidates tied at the cut — widen --budget or the task words)");
    }
    for s in &located.seeds {
        println!(
            "  {:?} {}  [{}]  {} dependents",
            s.node.kind,
            hit_name(&s.node),
            s.why.join(", "),
            s.centrality
        );
        for t in &s.touches {
            println!("      ← {:?} {}", t.via, hit_name(&t.node));
        }
    }
    Ok(())
}

/// The repo tag a module path belongs to (`web/src/...` → `web`), for grouping
/// cross-repo results. Empty when the graph is single-root and unnamespaced.
fn repo_of(module_path: &str, tags: &[(String, PathBuf)]) -> String {
    tags.iter()
        .map(|(t, _)| t)
        .find(|t| module_path.starts_with(&format!("{t}/")))
        .cloned()
        .unwrap_or_default()
}

/// Shared JSON for `locate`, over CLI and MCP. Flat ranked list (a global order is
/// more useful than per-repo buckets), each seed tagged with its repo, its reasons,
/// its centrality, and a one-hop blast preview. Truncation is declared.
fn locate_payload(located: &query::Located, tags: &[(String, PathBuf)]) -> serde_json::Value {
    let seeds: Vec<_> = located
        .seeds
        .iter()
        .map(|s| {
            serde_json::json!({
                "symbol": s.node.name,
                "module": s.node.module_path,
                "repo": repo_of(&s.node.module_path, tags),
                "kind": format!("{:?}", s.node.kind),
                "why": s.why,
                "centrality": s.centrality,
                "lexical": s.lexical,
                "touches": s.touches.iter().map(|t| serde_json::json!({
                    "symbol": t.node.name,
                    "module": t.node.module_path,
                    "via": format!("{:?}", t.via),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({
        "shown": located.seeds.len(),
        "total": located.total,
        "ambiguous": located.ambiguous,
        "reason": if located.total > located.seeds.len() { "budget" } else { "" },
        "seeds": seeds,
    })
}

/// Look a symbol up, widening the rule if needed, and say so whenever the answer
/// is about more or other symbols than the caller asked for.
///
/// Exact-only lookup made every Elixir module unfindable by the name a human types:
/// a module's name *is* its qualified name, so `impact LfgPost` failed and only
/// `impact FiveNoobs.Lfgs.LfgPost` worked. The note goes to stderr under `--json` so
/// machine output stays clean.
///
/// An *exact* name matching several symbols is the more dangerous case and used to
/// pass silently: six unrelated `run`s across three languages were seeded as one,
/// and the union reads exactly like one symbol's blast radius (#37).
fn lookup_or_bail<'a>(
    graph: &'a InMemoryGraph,
    query: &str,
    json: bool,
    args: &[String],
) -> Result<Vec<&'a ir::Node>> {
    let Some((nodes, how)) = graph.lookup(query) else {
        bail!("no symbol matched: {query}");
    };
    // narrow before saying anything: `--in-file` is the answer to the ambiguity,
    // so a note about symbols the user already excluded is noise
    let nodes = narrow_to_file(nodes, query, args)?;
    if let Some(note) = lookup_note(query, &nodes, how) {
        if json {
            eprintln!("{note}");
        } else {
            println!("{note}");
        }
    }
    Ok(nodes)
}

/// What to say about a lookup whose answer isn't one exactly-named symbol.
/// `None` when the query pinned a single symbol and there is nothing to warn about.
fn lookup_note(query: &str, nodes: &[&ir::Node], how: store::Match) -> Option<String> {
    const SHOWN: usize = 5;
    match how {
        store::Match::Exact if nodes.len() > 1 => {
            let files: HashSet<&str> = nodes.iter().map(|n| n.module_path.as_str()).collect();
            let shown: Vec<&str> = nodes
                .iter()
                .take(SHOWN)
                .map(|n| n.module_path.as_str())
                .collect();
            Some(format!(
                "'{query}' matches {} symbols in {} files ({}{}); answering about all of them — narrow with --in-file",
                nodes.len(),
                files.len(),
                shown.join(", "),
                if nodes.len() > shown.len() { ", …" } else { "" },
            ))
        }
        store::Match::Exact => None,
        store::Match::QualifiedSuffix | store::Match::Substring => {
            let rule = if how == store::Match::QualifiedSuffix {
                "qualified-name suffix"
            } else {
                "substring"
            };
            let names: Vec<&str> = nodes.iter().take(SHOWN).map(|n| n.name.as_str()).collect();
            let more = nodes.len().saturating_sub(names.len());
            let tail = if more > 0 {
                format!(", … {more} more")
            } else {
                String::new()
            };
            Some(format!(
                "no exact match for '{query}'; matched {} symbol(s) by {rule}: {}{tail}",
                nodes.len(),
                names.join(", ")
            ))
        }
    }
}

/// Warn when the files an answer is built from have changed since indexing.
///
/// There is no watcher and no automatic re-index, so a renamed function keeps
/// answering with a 0.95 next to it — a fabricated fact presented as a measured
/// one. The extract cache already stores a content hash per file, so checking the
/// handful of files in *this* answer costs one read each and no parse (#38).
fn warn_if_stale(store: &RedbStore, modules: &HashSet<&str>) {
    if modules.is_empty() {
        return;
    }
    let Ok(stamps) = store.read_file_stamps() else {
        return; // the answer is worth more than a diagnostic that failed
    };
    let stale = stale_modules(&stamps, modules);
    if stale.is_empty() {
        return;
    }
    let shown: Vec<&str> = stale.iter().take(3).map(String::as_str).collect();
    let more = stale.len().saturating_sub(shown.len());
    eprintln!(
        "⚠ {} of {} files in this answer changed since indexing ({}{}) — re-run `ripple index`",
        stale.len(),
        modules.len(),
        shown.join(", "),
        if more > 0 {
            format!(", … {more} more")
        } else {
            String::new()
        }
    );
}

/// Leave every secondary root a note saying where the shared index lives, so
/// `cd api && ripple review` answers from the graph that actually knows about
/// `api` rather than creating an empty database next to it.
///
/// Best-effort per root: a read-only checkout is a reason to lose the shortcut,
/// not a reason to fail an index that already succeeded.
fn write_index_pointers(primary: &Path, roots: &[(String, PathBuf)]) -> Result<()> {
    let primary = primary
        .canonicalize()
        .unwrap_or_else(|_| primary.to_owned());
    for (_, root) in roots {
        if *root == primary {
            continue;
        }
        let dir = root.join(".ripple");
        if std::fs::create_dir_all(&dir).is_err() {
            continue;
        }
        let _ = std::fs::write(
            dir.join(INDEX_POINTER),
            primary.to_string_lossy().as_bytes(),
        );
    }
    Ok(())
}

/// The machine-readable form of a review, shared by `--json` and the MCP tool.
///
/// One function on purpose: the two surfaces had drifted, and the MCP one — the
/// surface an agent actually reads — was missing every fix the CLI had gained
/// (namespacing, `total`, `changed_lines`, `untested_known`).
fn review_payload(r: &query::ReviewResult) -> Value {
    json!({
        "focus": r.focus.iter().map(|f| json!({
            "symbol": f.node.name, "module": f.node.module_path,
            "priority": f.review_priority, "downstream": f.downstream,
            "changed_lines": f.changed_lines,
            "reasons": f.reasons,
        })).collect::<Vec<_>>(),
        // without `total` a caller reads focus.len() as the size of the diff (#41)
        "total": r.total,
        "missing_cochange": r.missing_cochange.iter().map(|n| &n.module_path).collect::<Vec<_>>(),
        "untested": r.untested.iter().map(|n| &n.name).collect::<Vec<_>>(),
        // and without this it cannot tell "nothing untested" from "cannot tell" (#36)
        "untested_known": r.tests_known,
    })
}

/// Re-key a diff's repo-relative paths as the module paths the graph stores.
/// A single-root index has an empty tag and this is the identity.
fn namespaced(
    tag: &str,
    changed: HashMap<String, Vec<(u32, u32)>>,
) -> HashMap<String, Vec<(u32, u32)>> {
    if tag.is_empty() {
        return changed;
    }
    changed
        .into_iter()
        .map(|(path, ranges)| (resolve::namespace(tag, &path), ranges))
        .collect()
}

/// Which of `modules` no longer hash to what they were indexed as, sorted.
/// A module the index has never heard of is not a staleness claim we can make;
/// a file that has since disappeared is.
fn stale_modules(
    stamps: &HashMap<String, store::FileStamp>,
    modules: &HashSet<&str>,
) -> Vec<String> {
    let mut stale: Vec<String> = modules
        .iter()
        .filter(|m| {
            stamps.get(**m).is_some_and(|stamp| {
                std::fs::read_to_string(&stamp.canonical)
                    .map_or(true, |text| parse::content_hash(&text) != stamp.hash)
            })
        })
        .map(|m| (*m).to_owned())
        .collect();
    stale.sort_unstable();
    stale
}

/// `--in-file <substr>`: narrow a name that matched several symbols to the ones in
/// a matching file. One name is routinely several symbols (`get` is seven
/// resolvers), and without this the only way to read the right one was `awk`.
fn narrow_to_file<'a>(
    matches: Vec<&'a ir::Node>,
    symbol: &str,
    args: &[String],
) -> Result<Vec<&'a ir::Node>> {
    let Some(want) = flag_value(args, "--in-file") else {
        return Ok(matches);
    };
    let narrowed: Vec<&ir::Node> = matches
        .into_iter()
        .filter(|n| n.module_path.contains(want))
        .collect();
    if narrowed.is_empty() {
        bail!("no symbol '{symbol}' in a file matching '{want}'");
    }
    Ok(narrowed)
}

/// `ripple path <from> <to>`: how does one symbol reach another?
///
/// Answering this used to take one `neighbors` call per hop, assembled by hand — and
/// where a name is ambiguous (`get` is seven different resolvers) the hops could not
/// be attributed to each other at all.
fn cmd_path(args: &[String]) -> Result<()> {
    let json = args.iter().any(|a| a == "--json");
    let root: PathBuf = flag_value(args, "--root").map_or_else(|| ".".into(), PathBuf::from);
    let depth: usize = flag_value(args, "--depth")
        .and_then(|s| s.parse().ok())
        .unwrap_or(6);
    let limit: usize = flag_value(args, "--limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let ends = positionals(args);
    let [from, to] = ends.as_slice() else {
        bail!("path wants two symbols: ripple path <from> <to>\n{USAGE}");
    };

    let graph = RedbStore::open(db_path(&root)?).load()?;
    // no --in-file here: one filter over two endpoints has no unambiguous meaning
    let starts = lookup_or_bail(&graph, from, json, &[])?;
    let targets = lookup_or_bail(&graph, to, json, &[])?;

    let mut routes: Vec<(String, String, query::Route)> = Vec::new();
    for s in &starts {
        for t in &targets {
            if s.id == t.id {
                continue;
            }
            for r in query::paths(&graph, s.id, t.id, depth, limit) {
                routes.push((hit_name(s), hit_name(t), r));
            }
        }
    }
    routes.sort_by(|a, b| {
        a.2.steps
            .len()
            .cmp(&b.2.steps.len())
            .then(b.2.confidence.total_cmp(&a.2.confidence))
    });
    routes.truncate(limit);

    if json {
        let out: Vec<Value> = routes
            .iter()
            .map(|(from, to, r)| {
                json!({
                    "from": from, "to": to,
                    "hops": r.steps.len(),
                    "confidence": r.confidence,
                    "steps": r.steps.iter().map(|s| json!({
                        "edge": format!("{:?}", s.edge.kind),
                        "edge_confidence": s.edge.confidence,
                        "source": format!("{:?}", s.edge.source),
                        "site_line": s.edge.site.start_line,
                        "symbol": s.node.name, "module": s.node.module_path,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "routes": out }))?
        );
        return Ok(());
    }
    if routes.is_empty() {
        println!(
            "no route from {from} to {to} within {depth} hops \
             (co-change edges are excluded — they are companions, not routes)"
        );
        return Ok(());
    }
    for (i, (start, end, r)) in routes.iter().enumerate() {
        println!(
            "route {} — {} hops, confidence {:.2}",
            i + 1,
            r.steps.len(),
            r.confidence
        );
        println!("  {start}");
        for s in &r.steps {
            println!(
                "    │ {:?}<{:.2}> {}",
                s.edge.kind,
                s.edge.confidence,
                if s.edge.site.start_line > 0 {
                    format!("line {}", s.edge.site.start_line)
                } else {
                    "—".to_owned()
                }
            );
            println!("  ▼ {}", hit_name(&s.node));
        }
        if end != &hit_name(&r.steps[r.steps.len() - 1].node) {
            println!("  (ends at {end})");
        }
    }
    Ok(())
}

/// The node a hop hangs off: for an outward walk that's the edge's source, for an
/// inward one its destination.
fn parent_of(hop: &store::Hop, dir: Dir) -> Option<ir::SymbolId> {
    Some(match dir {
        Dir::Out => hop.edge.src,
        Dir::In => hop.edge.dst,
    })
}

/// Re-order hops so each one follows its parent, deepest-last within a branch.
///
/// `neighbors --depth 3` printed hops sorted by depth, so every second-level hop sat
/// at the same indentation regardless of which first-level hop it came from — the walk
/// was unreadable as a chain, which is what a walk is for.
fn tree_order(root: ir::SymbolId, hops: &[store::Hop], dir: Dir) -> Vec<Row> {
    let mut children: HashMap<ir::SymbolId, Vec<&store::Hop>> = HashMap::new();
    for h in hops {
        if let Some(p) = parent_of(h, dir) {
            children.entry(p).or_default().push(h);
        }
    }
    // how many sites back each (neighbour, edge kind): three `<Input />` in one
    // component are three edges, and collapsing them without saying so hides that (#28)
    let mut sites: HashMap<(ir::SymbolId, u8), usize> = HashMap::new();
    for h in hops {
        *sites.entry((h.node.id, h.edge.kind as u8)).or_default() += 1;
    }

    let mut walk = Walk {
        children: &children,
        sites: &sites,
        // one row per (neighbour, kind): keying on the node alone dropped the fact that
        // a caller can both import *and* call the same target
        emitted: HashSet::new(),
        // recursion is per node — expanding one twice would duplicate a whole subtree
        expanded: HashSet::new(),
        out: Vec::with_capacity(hops.len()),
    };
    walk.descend(root);
    // a hop whose parent was never itself reached (a cycle entered mid-way) still has
    // to be printed — dropping it would silently shrink the answer
    for h in hops {
        walk.push(h);
    }
    walk.out
}

/// One line of a traversal: a neighbour, and how many sites back it.
struct Row {
    hop: store::Hop,
    sites: usize,
}

struct Walk<'a> {
    children: &'a HashMap<ir::SymbolId, Vec<&'a store::Hop>>,
    sites: &'a HashMap<(ir::SymbolId, u8), usize>,
    emitted: HashSet<(ir::SymbolId, u8)>,
    expanded: HashSet<ir::SymbolId>,
    out: Vec<Row>,
}

impl Walk<'_> {
    /// Emit a row unless this (neighbour, kind) already has one. Returns whether it did.
    fn push(&mut self, h: &store::Hop) -> bool {
        let key = (h.node.id, h.edge.kind as u8);
        if !self.emitted.insert(key) {
            return false;
        }
        self.out.push(Row {
            hop: clone_hop(h),
            sites: self.sites.get(&key).copied().unwrap_or(1),
        });
        true
    }

    fn descend(&mut self, at: ir::SymbolId) {
        let Some(kids) = self.children.get(&at).cloned() else {
            return;
        };
        for h in kids {
            let fresh = self.push(h);
            if fresh && self.expanded.insert(h.node.id) {
                self.descend(h.node.id);
            }
        }
    }
}

fn clone_hop(h: &store::Hop) -> store::Hop {
    store::Hop {
        edge: h.edge.clone(),
        node: h.node.clone(),
        depth: h.depth,
    }
}

/// How a blast-radius hit is named in human output.
///
/// A file-level hit (`NodeKind::Module`) has no symbol name of its own — its name
/// *is* the path, and printing `path (path)` read like a bug. Say what it is instead:
/// these appear because a call can sit outside every function (issue #18).
///
/// Everything else carries its line, because two same-named symbols in one file
/// otherwise print as byte-identical blocks with different contents (#37).
fn hit_name(node: &ir::Node) -> String {
    if node.kind == ir::NodeKind::Module {
        format!("[file] {}", node.module_path)
    } else {
        format!(
            "{} ({}:{})",
            node.name, node.module_path, node.span.start_line
        )
    }
}

/// `--verify lsp`: before answering, ask the language servers about the query's
/// neighborhood, persist whatever they confirm/add/contradict, and answer from the
/// upgraded graph. Without the flag this returns the graph untouched.
///
/// Persisting rather than applying in-memory is what keeps determinism: LSP answers
/// move with server version and index state, so they become stored data with a
/// `source`, not something a later query re-derives. See docs/11.
fn verify_upgrade(
    store: &mut RedbStore,
    graph: InMemoryGraph,
    root: &Path,
    args: &[String],
    seeds: &[ir::SymbolId],
    json: bool,
) -> Result<InMemoryGraph> {
    if flag_value(args, "--verify") != Some("lsp") {
        return Ok(graph);
    }
    let budget = parse_duration(flag_value(args, "--verify-budget"))
        .unwrap_or_else(|| std::time::Duration::from_secs(2));
    let mut roots = store.read_roots()?;
    if roots.is_empty() {
        roots.push((String::new(), root.canonicalize().unwrap_or(root.into())));
    }
    // the extract cache already hashes every file for incremental indexing; the
    // verdict cache reuses those hashes as its keys
    let hashes: std::collections::HashMap<String, String> = store
        .read_extracts()?
        .into_iter()
        .map(|(module, f)| (module, f.hash))
        .collect();
    let cached = store.read_verified()?;
    let plan = verify::Plan {
        focus: verify::focus_files(&graph, seeds),
        roots: &roots,
        budget,
        hashes: &hashes,
        cached: &cached,
        on_denial: if args.iter().any(|a| a == "--drop-contradicted") {
            verify::OnDenial::Drop
        } else if args.iter().any(|a| a == "--floor-contradicted") {
            verify::OnDenial::Floor
        } else {
            verify::OnDenial::Report
        },
    };
    let outcome = verify::run(&graph, &plan);
    // json output is a bare array clients parse; the report goes to stderr so it
    // stays visible without breaking them
    if json {
        eprintln!("{}", outcome.summary());
    } else {
        println!("{}", outcome.summary());
    }
    if !outcome.learned.is_empty() {
        store
            .write_verified(&outcome.learned)
            .context("persisting verified verdicts")?;
    }
    if !outcome.changed() {
        return Ok(graph);
    }
    let nodes: Vec<ir::Node> = graph.nodes().cloned().collect();
    store
        .write(&nodes, &outcome.edges)
        .context("persisting verified edges")?;
    Ok(InMemoryGraph::from_parts(nodes, outcome.edges))
}

/// `churn,bug,ownership,fanout` — four numbers, for grading a candidate weighting.
fn parse_weights(spec: &str) -> Option<[f32; 4]> {
    let parts: Vec<f32> = spec
        .split(',')
        .filter_map(|p| p.trim().parse().ok())
        .collect();
    <[f32; 4]>::try_from(parts.as_slice()).ok()
}

/// `2s`, `500ms`, or a bare number of milliseconds.
fn parse_duration(v: Option<&str>) -> Option<std::time::Duration> {
    let v = v?.trim();
    let (num, mult) = if let Some(n) = v.strip_suffix("ms") {
        (n, 1)
    } else if let Some(n) = v.strip_suffix('s') {
        (n, 1000)
    } else {
        (v, 1)
    };
    let ms: u64 = num.trim().parse().ok()?;
    Some(std::time::Duration::from_millis(ms * mult))
}

/// Historical validation: over recent commits, how many same-commit file pairs
/// does the graph link? Static edges are leakage-free (independent of commit
/// history); the static-vs-co-change gap shows why co-change is needed.
/// `eval --oracle lsp`: compare ripple's call edges against a language server's,
/// on a sample. The number this produces is the only precision measurement the
/// tree-sitter call graph has — see docs/11-lsp-integration.md phase 2.
///
/// Deliberately a *comparison*, not a scoring: a tree-sitter-based server like
/// dexter is a peer, so a disagreement localises a bug in one of the two, and
/// agreement proves neither correct.
fn cmd_eval_oracle(args: &[String]) -> Result<()> {
    let root: PathBuf = flag_value(args, "--root").map_or_else(|| ".".into(), PathBuf::from);
    let root = root
        .canonicalize()
        .with_context(|| format!("cannot access {}", root.display()))?;
    let sample: usize = flag_value(args, "--sample")
        .and_then(|s| s.parse().ok())
        .unwrap_or(25);
    let grain = match flag_value(args, "--granularity") {
        Some("file") => Granularity::File,
        Some("function") | None => Granularity::Function,
        Some(other) => bail!("--granularity takes function or file, not {other}"),
    };

    let store = RedbStore::open(db_path(&root)?);
    let graph = store.load()?;
    let mut roots = store.read_roots()?;
    if roots.is_empty() {
        roots.push((String::new(), root.clone()));
    }
    let specs = lsp::load(&root)?.specs;
    let registry = lang::registry();

    let mut any = false;
    for (tag, path) in &roots {
        let indexed = indexed_languages(Some(&graph), tag);
        for spec in specs.iter().filter(|s| indexed.contains(&s.language)) {
            if !lsp::applies(spec, path) {
                continue;
            }
            let mut client = match lsp::Client::start(spec, path) {
                Ok(c) => c,
                Err(e) => {
                    println!("{}: cannot start {} ({e:#})", spec.language, spec.command);
                    continue;
                }
            };
            let (caps, server) = match client.initialize(path, spec) {
                Ok(v) => v,
                Err(e) => {
                    println!("{}: handshake failed ({e:#})", spec.language);
                    continue;
                }
            };
            if !caps.call_hierarchy {
                println!(
                    "{}: {} has no callHierarchy — cannot compare",
                    spec.language,
                    server.as_deref().unwrap_or(&spec.command)
                );
                continue;
            }
            any = true;
            let files = sample_files(&graph, tag, &spec.language, &registry, sample);
            let cmp = compare_calls(&graph, &mut client, path, tag, &files, grain);
            report_oracle(spec, server.as_deref(), &files, &cmp, grain);
            client.stop();
        }
    }
    if !any {
        println!("no usable server for any indexed language — `ripple lsp doctor` explains why");
    }
    Ok(())
}

/// Evenly spread `n` files of one language across the root, so the sample isn't
/// all of one directory. Deterministic: module paths are sorted first.
fn sample_files(
    graph: &InMemoryGraph,
    tag: &str,
    language: &str,
    registry: &[Box<dyn lang::LanguageAdapter>],
    n: usize,
) -> Vec<String> {
    let prefix = if tag.is_empty() {
        String::new()
    } else {
        format!("{tag}/")
    };
    let mut modules: Vec<&str> = graph
        .nodes()
        .filter(|node| node.module_path.starts_with(&prefix))
        .filter(|node| {
            lang::adapter_for(registry, Path::new(&node.module_path))
                .is_some_and(|a| a.id() == language)
        })
        .map(|node| node.module_path.as_str())
        .collect();
    modules.sort_unstable();
    modules.dedup();
    if modules.len() <= n {
        return modules.into_iter().map(str::to_owned).collect();
    }
    let step = modules.len() / n;
    modules
        .into_iter()
        .step_by(step.max(1))
        .take(n)
        .map(str::to_owned)
        .collect()
}

/// What a caller identity means when the two sides are compared.
///
/// The server names functions; ripple sometimes only knows the file (a call in a
/// module body or an ExUnit `test` block belongs to no indexed symbol — issue #18).
/// Scoring a file-granular edge against a function-granular oracle counted a
/// correct-but-coarser answer as a disagreement, so the granularity is now stated
/// rather than assumed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Granularity {
    Function,
    File,
}

#[derive(Default)]
struct Comparison {
    /// symbols the server could resolve, so a verdict is possible
    judged: usize,
    /// symbols the server couldn't resolve a call-hierarchy item for
    server_unknown: usize,
    /// symbols the server reported but ripple has no node for
    ripple_unknown: usize,
    /// a few names from each side, to diagnose a mismatch in naming convention
    server_names: Vec<String>,
    /// self-recursion the server reports and ripple drops on purpose
    self_edges: usize,
    /// call sites the server credited to a function that does not contain them
    misattributed: usize,
    /// call sites inside no indexed ripple symbol — the size of the issue-#18 gap,
    /// measured from the server's side
    outside_any_symbol: usize,
    /// a few `module:line` positions of those, so the gap can be characterised
    /// rather than guessed at
    outside_examples: Vec<String>,
    agreed: usize,
    /// ripple has an edge the server doesn't
    ripple_only: Vec<String>,
    /// the server has an edge ripple lacks
    server_only: Vec<String>,
}

/// For each function the server finds in each sampled file, compare its callers
/// with ripple's. Callers in files ripple doesn't index are dropped: a server that
/// also indexes stdlib and dependencies would otherwise look infinitely better.
fn compare_calls(
    graph: &InMemoryGraph,
    client: &mut lsp::Client,
    root: &Path,
    tag: &str,
    files: &[String],
    grain: Granularity,
) -> Comparison {
    let mut cmp = Comparison::default();
    for module in files {
        let rel = module.strip_prefix(&format!("{tag}/")).unwrap_or(module);
        let abs = root.join(rel);
        if client.open(&abs).is_err() {
            continue;
        }
        let Ok(symbols) = client.functions(&abs) else {
            continue;
        };
        // Union the server's answers per ripple symbol before judging any of them.
        // One ripple node routinely corresponds to several server symbols — eight
        // overload declarations of `getFragmentData`, an Elixir function's clauses —
        // and comparing each against the same node counted the same caller set as a
        // disagreement once per declaration (45 phantom false positives on 5noobs).
        let mut claims: std::collections::BTreeMap<ir::SymbolId, (bool, Vec<lsp::CallSite>)> =
            std::collections::BTreeMap::new();
        for sym in symbols {
            let name = bare_name(&sym.name).to_owned();
            if !is_callable_name(&name) {
                continue;
            }
            let Ok(found) = client.incoming_calls(&abs, sym.line, sym.character) else {
                continue;
            };
            if cmp.server_names.len() < 6 {
                cmp.server_names.push(name.clone());
            }
            let Some(target) = graph
                .nodes_in_file(module)
                .into_iter()
                .find(|n| n.name == name)
            else {
                cmp.ripple_unknown += 1;
                continue;
            };
            let entry = claims.entry(target.id).or_default();
            // a declaration the server can't resolve tells us nothing; another
            // declaration of the same symbol may still answer
            if let Some(sites) = found {
                entry.0 = true;
                entry.1.extend(sites);
            }
        }

        for (target_id, (resolved, sites)) in claims {
            let Some(target) = graph.get(target_id) else {
                continue;
            };
            if !resolved {
                cmp.server_unknown += 1;
                continue;
            }

            let key = |module: &str, name: &str| match grain {
                Granularity::Function => (module.to_owned(), name.to_owned()),
                Granularity::File => (module.to_owned(), String::new()),
            };
            // A caller that is a module or a file is a file-granular claim: the call
            // is real but sits outside every function (issue #18). At function
            // granularity there is nothing for the server to agree or disagree with,
            // so scoring it there measures the granularity mismatch, not the edge.
            let comparable = |n: &ir::Node| match grain {
                Granularity::Function => {
                    matches!(n.kind, ir::NodeKind::Function | ir::NodeKind::Method)
                }
                Granularity::File => true,
            };
            let ours: HashSet<(String, String)> = graph
                .in_edges(target.id)
                .iter()
                .filter(|e| e.kind == EdgeKind::Calls)
                .filter_map(|e| graph.get(e.src))
                .filter(|n| comparable(n))
                .map(|n| key(&n.module_path, &n.name))
                .collect();
            let mut theirs: HashSet<(String, String)> = HashSet::new();

            for site in sites {
                let Ok(rel) = site.path.strip_prefix(root) else {
                    continue;
                };
                let module = resolve::namespace(tag, &rel.to_string_lossy());
                if graph.nodes_in_file(&module).is_empty() {
                    continue; // not indexed by ripple; not a fair miss
                }
                let named = bare_name(&site.name).to_owned();
                for attributed in verify::attribute(graph, &module, &site) {
                    let caller = match attributed {
                        verify::Attribution::Symbol(id) => {
                            let Some(n) = graph.get(id) else { continue };
                            if n.name != named {
                                // the server credited a function that does not
                                // contain the call: judging ripple against that
                                // manufactures disagreements in both directions
                                cmp.misattributed += 1;
                            }
                            n.name.clone()
                        }
                        verify::Attribution::OutsideAnySymbol => {
                            cmp.outside_any_symbol += 1;
                            if cmp.outside_examples.len() < 8 {
                                let lines = site
                                    .call_lines
                                    .iter()
                                    .map(u32::to_string)
                                    .collect::<Vec<_>>()
                                    .join(",");
                                cmp.outside_examples
                                    .push(format!("{module}:{lines} (server said {named})"));
                            }
                            // ripple has no symbol here, so at function granularity
                            // there is nothing to compare; at file granularity the
                            // file itself is the answer
                            match grain {
                                Granularity::Function => continue,
                                Granularity::File => String::new(),
                            }
                        }
                    };
                    // ripple drops X → X deliberately (a blast radius from a symbol
                    // to itself says nothing), so counting it as a miss measures a
                    // documented choice rather than a defect
                    if module == target.module_path && caller == target.name {
                        cmp.self_edges += 1;
                        continue;
                    }
                    theirs.insert(key(&module, &caller));
                }
            }

            cmp.judged += 1;
            if ours == theirs {
                cmp.agreed += 1;
            }
            let describe = |(module, name): &(String, String)| {
                format!("{}:{} ← {module}:{name}", target.module_path, target.name)
            };
            cmp.ripple_only
                .extend(ours.difference(&theirs).map(describe));
            cmp.server_only
                .extend(theirs.difference(&ours).map(describe));
        }
    }
    cmp
}

fn report_oracle(
    spec: &lsp::ServerSpec,
    server: Option<&str>,
    files: &[String],
    cmp: &Comparison,
    grain: Granularity,
) {
    let pct = |n: usize, d: usize| {
        if d == 0 {
            0.0
        } else {
            100.0 * n as f32 / d as f32
        }
    };
    println!(
        "{} vs {} — {} files, {} symbols judged ({} unresolved by server, {} unknown to ripple)",
        spec.language,
        server.unwrap_or(&spec.command),
        files.len(),
        cmp.judged,
        cmp.server_unknown,
        cmp.ripple_unknown
    );
    if cmp.judged == 0 && !cmp.server_names.is_empty() {
        println!(
            "  server named: {} — if these don't look like ripple's symbol names, the two sides disagree on naming, not on edges",
            cmp.server_names.join(", ")
        );
    }
    println!(
        "  compared at           : {} granularity",
        match grain {
            Granularity::Function => "function",
            Granularity::File => "file",
        }
    );
    println!(
        "  identical caller sets : {}/{} ({:.1}%)",
        cmp.agreed,
        cmp.judged,
        pct(cmp.agreed, cmp.judged)
    );
    println!(
        "  ripple-only edges     : {} (possible false positives)",
        cmp.ripple_only.len()
    );
    println!(
        "  server-only edges     : {} (possible missed edges)",
        cmp.server_only.len()
    );
    if cmp.self_edges > 0 {
        println!(
            "  excluded              : {} self-recursive edges ripple drops by design",
            cmp.self_edges
        );
    }
    if cmp.misattributed > 0 {
        println!(
            "  server misattributed  : {} call sites credited to a function that doesn't contain them",
            cmp.misattributed
        );
    }
    if cmp.outside_any_symbol > 0 {
        println!(
            "  outside any symbol    : {} call sites inside no indexed symbol (issue #18{})",
            cmp.outside_any_symbol,
            match grain {
                Granularity::Function => "; not comparable here",
                Granularity::File => "; compared as their file",
            }
        );
        for e in &cmp.outside_examples {
            println!("    outside: {e}");
        }
    }
    for (label, examples) in [
        ("ripple-only", &cmp.ripple_only),
        ("server-only", &cmp.server_only),
    ] {
        for e in examples.iter().take(5) {
            println!("    {label}: {e}");
        }
        if examples.len() > 5 {
            println!("    {label}: … {} more", examples.len() - 5);
        }
    }
}

/// `eval --risk`: does the risk score rank the files a held-out fix commit touched?
/// See `riskeval` for why that label and not co-change.
fn cmd_eval_risk(args: &[String]) -> Result<()> {
    let root: PathBuf = flag_value(args, "--root").map_or_else(|| ".".into(), PathBuf::from);
    let k: usize = flag_value(args, "--commits")
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);
    let store = RedbStore::open(db_path(&root)?);
    let graph = store.load()?;
    let tag = root_tag(&store, &root)?;

    let skip: usize = flag_value(args, "--skip")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let split = overlay::holdout_at(&root, skip, k);
    // risk from training history only, namespaced the way indexing did
    let trained: std::collections::HashMap<String, ir::RiskScores> = split
        .train
        .file_risk
        .iter()
        .map(|(path, r)| (resolve::namespace(&tag, path), *r))
        .filter(|(path, _)| !graph.nodes_in_file(path).is_empty())
        .collect();
    // structural fanout is a property of the graph, not of history — no leakage
    let mut fanout: std::collections::HashMap<String, f32> = std::collections::HashMap::new();
    for n in graph.nodes() {
        let slot = fanout.entry(n.module_path.clone()).or_default();
        *slot = slot.max(n.risk.fanout);
    }
    let fixed: HashSet<String> = split
        .test
        .iter()
        .filter(|c| c.is_fix)
        .flat_map(|c| c.files.iter().map(|p| resolve::namespace(&tag, p)))
        .filter(|p| trained.contains_key(p))
        .collect();

    let indexed_files = graph
        .nodes()
        .filter(|n| n.kind == ir::NodeKind::Module)
        .count();
    let terms = riskeval::terms_for(&trained, &fanout);
    if terms.is_empty() || fixed.is_empty() {
        println!(
            "not enough held-out history to judge risk: {} scorable files, {} touched by a              fix in {} test commits",
            terms.len(),
            fixed.len(),
            split.test.len()
        );
        return Ok(());
    }
    if let Some(spec) = flag_value(args, "--weights") {
        let w = parse_weights(spec)
            .with_context(|| format!("--weights wants four numbers like 0,0.2,0.1,0.3: {spec}"))?;
        let (p, lift) = riskeval::score_weights(&terms, &fixed, w);
        println!(
            "weights churn {:.1} bug {:.1} own {:.1} fanout {:.1} on {} test commits              ({} files, {} fixed): p@25% {:.1}%, lift {:.2}×",
            w[0],
            w[1],
            w[2],
            w[3],
            split.test.len(),
            terms.len(),
            fixed.len(),
            100.0 * p,
            lift
        );
        return Ok(());
    }
    riskeval::print(
        &terms,
        &fixed,
        split.test.len(),
        indexed_files.saturating_sub(terms.len()),
    );
    Ok(())
}

/// Build a fully-assembled in-memory graph for `roots` without persisting it — the
/// same pipeline `index_project` runs, minus the store writes and diagnostics.
///
/// `eval --review` indexes historical checkouts and must not touch the on-disk
/// index, so it needs the graph in memory only. `index_project` is the source of
/// truth for this pipeline; if the order there changes, change it here too.
fn build_indexed_graph(roots: &[PathBuf]) -> Result<InMemoryGraph> {
    build_indexed_graph_incremental(roots, &HashMap::new())
}

/// `build_indexed_graph`, reusing `cached` extracts for unchanged files (matched by
/// content hash) and re-extracting only the ones that changed on disk. This is what
/// makes sync-at-query cheap: pass the store's extract cache and only the working
/// tree's dirty files pay the parse. See docs/17-keeping-the-graph-in-sync.md.
fn build_indexed_graph_incremental(
    roots: &[PathBuf],
    cached: &HashMap<String, parse::CachedFile>,
) -> Result<InMemoryGraph> {
    let indexed = resolve::build_incremental(roots, cached)?;
    let mut nodes = indexed.result.nodes;
    let mut edges = indexed.result.edges;

    let mut git = overlay::GitOverlay::default();
    for (tag, root) in &indexed.roots {
        let o = overlay::mine(root);
        for (k, v) in o.file_risk {
            git.file_risk.insert(resolve::namespace(tag, &k), v);
        }
        for (a, b, s) in o.cochange {
            git.cochange
                .push((resolve::namespace(tag, &a), resolve::namespace(tag, &b), s));
        }
    }
    overlay::apply(&git, &mut nodes, &mut edges);

    let mut cross = resolve::link_cross_service(&indexed.files, &nodes);
    if !cross.route_paths.is_empty() {
        let mut by_id: HashMap<ir::SymbolId, Vec<String>> = HashMap::new();
        for (id, text) in cross.route_paths.drain(..) {
            let routes = by_id.entry(id).or_default();
            if !routes.contains(&text) {
                routes.push(text);
            }
        }
        for n in &mut nodes {
            if let Some(routes) = by_id.get(&n.id) {
                n.route_path = Some(routes.join(" "));
            }
        }
    }
    edges.append(&mut cross.edges);

    let scopes = resolve::TestScopes::of(&indexed.files, &indexed.roots, &lang::registry());
    let mut test_edges = resolve::link_tests(&scopes, &edges);
    edges.append(&mut test_edges);

    overlay::score_structure(&mut nodes, &edges);
    Ok(InMemoryGraph::from_parts(nodes, edges))
}

/// The graph a query answers from. Plain: the durable snapshot as last indexed.
/// With `sync`: an in-memory incremental rebuild over the same roots — the extract
/// cache is reused for unchanged files and only the working tree's changed/added
/// files are re-parsed, so the answer reflects the code as it is now, with no manual
/// `ripple index` and nothing written to disk. See docs/17-keeping-the-graph-in-sync.md.
///
/// The rebuild is not persisted on purpose: a read query must not take the index's
/// write lock. It re-reads and re-links every file, so it costs a warm re-index; the
/// export-signature propagation in the design doc is the optimisation that makes it
/// proportional to the edit instead. Ships opt-in behind `--sync` until that lands.
fn load_current(store: &RedbStore, root: &Path, sync: bool) -> Result<InMemoryGraph> {
    if !sync {
        return store.load();
    }
    let mut roots = store.read_roots()?;
    if roots.is_empty() {
        roots.push((
            String::new(),
            root.canonicalize().unwrap_or_else(|_| root.to_path_buf()),
        ));
    }
    let cached = store.read_extracts()?;
    let root_paths: Vec<PathBuf> = roots.into_iter().map(|(_, p)| p).collect();
    build_indexed_graph_incremental(&root_paths, &cached)
}

/// Where `review` ranks the defective symbol of one SZZ case: index the tree at the
/// introducing commit, review that commit's own diff, and find the best rank among
/// the symbols it changed in the defective file. `None` when the introducer's edit
/// to that file maps to no indexed symbol (nothing to rank).
fn measure_case(repo_root: &Path, case: &overlay::SzzCase) -> Result<Option<CaseRank>> {
    let scratch = tempfile::tempdir().context("temp dir for historical checkout")?;
    let wt = scratch.path().join("t"); // must not pre-exist for `worktree add`
    let added = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["worktree", "add", "--detach"])
        .arg(&wt)
        .arg(&case.introduced_at)
        .output()
        .context("git worktree add")?;
    if !added.status.success() {
        // a shallow clone or gc'd commit can't be checked out — skip, don't fail
        return Ok(None);
    }
    let measured = (|| -> Result<Option<CaseRank>> {
        let graph = build_indexed_graph(std::slice::from_ref(&wt))?;
        // the introducer's own diff, in its tree's coordinates (workdir == its tree)
        let base = format!("{}^", case.introduced_at);
        let changed = overlay::diff_lines(&wt, Some(&base));
        if changed.is_empty() {
            return Ok(None);
        }
        // full ranking, so a defective symbol past the budget still gets a rank
        let r = query::review_focus(&graph, &changed, usize::MAX, "");
        let total = r.focus.len();
        let rank = r
            .focus
            .iter()
            .position(|f| f.node.module_path == case.file)
            .map(|i| i + 1);
        Ok(rank.map(|rank| CaseRank { rank, total }))
    })();
    // best-effort teardown; a leaked worktree is noise, not a failure
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["worktree", "remove", "--force"])
        .arg(&wt)
        .output();
    measured
}

/// One SZZ case's outcome: where the defective symbol ranked, out of how many.
struct CaseRank {
    rank: usize,
    total: usize,
}

/// `eval --review`: does `review` rank the defective symbol high, within the one
/// change that introduced it? File-level `eval --risk` cannot answer this — it
/// scores across commits, not within one (#55). Reports mean normalized rank
/// against the 0.5 chance line, the shape of the manual SZZ measurement that
/// motivated the issue.
fn cmd_eval_review(args: &[String]) -> Result<()> {
    let root: PathBuf = flag_value(args, "--root").map_or_else(|| ".".into(), PathBuf::from);
    let budget: usize = flag_value(args, "--budget")
        .and_then(|s| s.parse().ok())
        .unwrap_or(15);
    let scan: usize = flag_value(args, "--commits")
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    let max_cases: usize = flag_value(args, "--cases")
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let converge: f32 = flag_value(args, "--converge")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.6);
    let escape_days: i64 = flag_value(args, "--escape-days")
        .and_then(|s| s.parse().ok())
        .unwrap_or(7);
    // introducers touching more files than this are bulk/initial commits, dropped
    // from the corpus (a shallow clone's squashed import, #67). 40 matches the
    // overlay's own co-change bulk guard.
    let max_introducer_files: usize = flag_value(args, "--max-introducer-files")
        .and_then(|s| s.parse().ok())
        .unwrap_or(40);

    let scan_out = overlay::szz_cases(
        &root,
        scan,
        max_cases,
        converge,
        escape_days,
        max_introducer_files,
    );
    let cases = scan_out.cases;
    let skipped_bulk = scan_out.skipped_bulk;
    if cases.is_empty() {
        println!(
            "no SZZ cases in the newest {scan} commits (fix commits whose removed lines \
             converge ≥{converge:.0}% on one introducer that escaped ≥{escape_days}d, \
             introducer ≤{max_introducer_files} files) — nothing to measure."
        );
        if skipped_bulk > 0 {
            println!(
                "  ({skipped_bulk} case(s) dropped — introducer was a bulk commit \
                 >{max_introducer_files} files)"
            );
        }
        return Ok(());
    }

    let mut norm_ranks: Vec<f32> = Vec::new();
    let mut hits = 0usize;
    let mut rows: Vec<(usize, usize, f32, &overlay::SzzCase)> = Vec::new();
    let mut unmeasured = 0usize;
    for case in &cases {
        match measure_case(&root, case)? {
            Some(cr) if cr.total > 0 => {
                #[allow(clippy::cast_precision_loss)]
                let norm = cr.rank as f32 / cr.total as f32;
                norm_ranks.push(norm);
                if cr.rank <= budget {
                    hits += 1;
                }
                rows.push((cr.rank, cr.total, norm, case));
            }
            _ => unmeasured += 1,
        }
    }

    if norm_ranks.is_empty() {
        println!(
            "found {} SZZ case(s) but none were measurable — each introducer's edit to \
             its defective file mapped to no indexed symbol.",
            cases.len()
        );
        if skipped_bulk > 0 {
            println!(
                "  ({skipped_bulk} case(s) dropped — introducer was a bulk commit \
                 >{max_introducer_files} files)"
            );
        }
        return Ok(());
    }

    #[allow(clippy::cast_precision_loss)]
    let mean = norm_ranks.iter().sum::<f32>() / norm_ranks.len() as f32;
    println!(
        "review targeting on an SZZ corpus ({} measurable case(s), budget {budget}):",
        norm_ranks.len()
    );
    println!("  mean normalized rank : {mean:.3}   (0.500 = chance)");
    println!(
        "  hit@budget           : {hits}/{} ({:.1}%)",
        norm_ranks.len(),
        100.0 * hits as f32 / norm_ranks.len() as f32
    );
    if mean > 0.0 {
        println!(
            "  lift over chance     : {:.2}× (chance ÷ mean)",
            0.5 / mean
        );
    }
    if unmeasured > 0 {
        println!("  ({unmeasured} case(s) skipped — no indexed symbol at the introducing edit)");
    }
    if skipped_bulk > 0 {
        println!(
            "  ({skipped_bulk} case(s) dropped — introducer was a bulk commit \
             >{max_introducer_files} files)"
        );
    }
    // deterministic: worst rank first, then by fix sha
    rows.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.3.fix.cmp(&b.3.fix)));
    println!("  rank/total  norm   esc(d)  conv   file  (introducer)");
    for (rank, total, norm, case) in rows {
        println!(
            "  {rank:>4}/{total:<5} {norm:.3}  {:>5}   {:.0}%   {}  ({})",
            case.escaped_days,
            100.0 * case.convergence,
            case.file,
            &case.introduced_at[..case.introduced_at.len().min(9)]
        );
    }
    Ok(())
}

/// Identifier-like tokens in a blob of source, deduped. The grep baseline's whole
/// vocabulary: which names does this file mention, textually, in any language.
fn identifier_tokens(src: &[u8]) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let mut cur = String::new();
    for &b in src {
        let c = b as char;
        if c.is_ascii_alphanumeric() || c == '_' {
            cur.push(c);
        } else if !cur.is_empty() {
            if cur.len() >= 3 {
                out.insert(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
        }
    }
    if cur.len() >= 3 {
        out.insert(cur);
    }
    out
}

/// Recall@k: share of `truth` files that appear in the first `k` of `ranked`.
fn recall_at_k(ranked: &[String], truth: &HashSet<String>, k: usize) -> f32 {
    if truth.is_empty() {
        return 0.0;
    }
    let hits = ranked.iter().take(k).filter(|f| truth.contains(*f)).count();
    #[allow(clippy::cast_precision_loss)]
    {
        hits as f32 / truth.len() as f32
    }
}

/// Reciprocal rank of the first `truth` file in `ranked`, 0 if none appears.
fn reciprocal_rank(ranked: &[String], truth: &HashSet<String>) -> f32 {
    ranked
        .iter()
        .position(|f| truth.contains(f))
        .map_or(0.0, |i| 1.0 / (i as f32 + 1.0))
}

/// `eval --vs-grep`: does ripple's blast radius beat plain grep at anticipating
/// what changes together?
///
/// Ground truth is held-out co-change: for each file a test commit touched, the
/// *other* files it touched. Two predictors rank the rest of the repo for that
/// seed — ripple by dependency reach (`impact`), grep by shared identifiers (the
/// files that textually mention a name the seed defines, which is what a developer
/// without ripple would grep for). Both get the same budget `k`; recall@k and MRR
/// say which fills those slots with the files that actually changed. This is the
/// "with ripple vs without" number, against a real baseline rather than chance.
fn cmd_eval_vs_grep(args: &[String]) -> Result<()> {
    let root: PathBuf = flag_value(args, "--root").map_or_else(|| ".".into(), PathBuf::from);
    let k: usize = flag_value(args, "--budget")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    let commits: usize = flag_value(args, "--commits")
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);

    let store = RedbStore::open(db_path(&root)?);
    let graph = store.load()?;
    let tag = root_tag(&store, &root)?;
    let extracts = store.read_extracts()?;

    let indexed = |p: &str| graph.get(ir::SymbolId::module(p)).is_some();

    // per file: the identifiers it defines (the grep query for a seed), and the
    // identifiers it mentions (what the baseline searches). Both keyed by the
    // namespaced module path, so they line up with co-change and the graph.
    let mut defines: HashMap<String, Vec<String>> = HashMap::new();
    let mut mentions: HashMap<String, HashSet<String>> = HashMap::new();
    for cf in extracts.values() {
        let names: Vec<String> = cf
            .extract
            .defs
            .iter()
            .map(|n| n.name.clone())
            .filter(|n| n.len() >= 3)
            .collect();
        if !names.is_empty() {
            defines.insert(cf.module_path.clone(), names);
        }
        if let Ok(bytes) = std::fs::read(&cf.canonical) {
            mentions.insert(cf.module_path.clone(), identifier_tokens(&bytes));
        }
    }
    let candidate_files: Vec<&String> = mentions.keys().collect();

    // ripple's ranked files for a seed: the blast radius of the seed's symbols,
    // deduped to distinct files in impact-score order (the seed itself dropped).
    let ripple_rank = |seed: &str| -> Vec<String> {
        let seeds: Vec<ir::SymbolId> = graph.nodes_in_file(seed).iter().map(|n| n.id).collect();
        let imp = query::impact(&graph, &seeds, usize::MAX);
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for h in imp.hits {
            let f = h.node.module_path;
            if f != seed && seen.insert(f.clone()) {
                out.push(f);
            }
        }
        out
    };

    // grep's ranked files for a seed: every other file, scored by how many of the
    // seed's defined names it mentions, most first. Deterministic tie-break on path.
    let grep_rank = |seed: &str| -> Vec<String> {
        let empty = Vec::new();
        let names = defines.get(seed).unwrap_or(&empty);
        if names.is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(usize, &String)> = candidate_files
            .iter()
            .filter(|f| **f != seed)
            .filter_map(|f| {
                let toks = &mentions[*f];
                let n = names.iter().filter(|nm| toks.contains(*nm)).count();
                (n > 0).then_some((n, *f))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
        scored.into_iter().map(|(_, f)| f.clone()).collect()
    };

    let split = overlay::holdout(&root, commits);
    let (mut r_recall, mut g_recall, mut r_mrr, mut g_mrr) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let mut seeds = 0usize;
    let mut truth_total = 0usize;
    for tc in &split.test {
        let files: Vec<String> = tc
            .files
            .iter()
            .map(|p| resolve::namespace(&tag, p))
            .filter(|p| indexed(p))
            .collect();
        if files.len() < 2 {
            continue;
        }
        for seed in &files {
            let truth: HashSet<String> = files.iter().filter(|f| *f != seed).cloned().collect();
            if truth.is_empty() {
                continue;
            }
            let rr = ripple_rank(seed);
            let gr = grep_rank(seed);
            r_recall += recall_at_k(&rr, &truth, k);
            g_recall += recall_at_k(&gr, &truth, k);
            r_mrr += reciprocal_rank(&rr, &truth);
            g_mrr += reciprocal_rank(&gr, &truth);
            seeds += 1;
            truth_total += truth.len();
        }
    }

    if seeds == 0 {
        println!(
            "no held-out co-change to score: {} test commits, none with two indexed files that \
             changed together. Try a wider --commits.",
            split.test.len()
        );
        return Ok(());
    }

    #[allow(clippy::cast_precision_loss)]
    let n = seeds as f32;
    let pct = |x: f32| 100.0 * x / n;
    // a random ranking's expected recall@k ≈ k / (files it could have picked)
    #[allow(clippy::cast_precision_loss)]
    let floor = 100.0 * (k as f32 / candidate_files.len().max(1) as f32).min(1.0);
    println!(
        "ripple vs grep — held-out co-change prediction ({} test commits, {seeds} seed files, \
         {:.1} co-changed files each on average, budget k={k}):",
        split.test.len(),
        truth_total as f32 / n
    );
    println!("                recall@{k}   MRR");
    println!("  ripple        {:>6.1}%   {:.3}", pct(r_recall), r_mrr / n);
    println!("  grep          {:>6.1}%   {:.3}", pct(g_recall), g_mrr / n);
    println!("  random floor  {floor:>6.1}%     —");
    let delta = pct(r_recall) - pct(g_recall);
    println!(
        "  → ripple {} grep by {:.1} pts recall@{k}",
        if delta >= 0.0 { "beats" } else { "trails" },
        delta.abs()
    );
    Ok(())
}

fn cmd_eval(args: &[String]) -> Result<()> {
    if flag_value(args, "--oracle") == Some("lsp") {
        return cmd_eval_oracle(args);
    }
    if args.iter().any(|a| a == "--risk") {
        return cmd_eval_risk(args);
    }
    if args.iter().any(|a| a == "--review") {
        return cmd_eval_review(args);
    }
    if args.iter().any(|a| a == "--vs-grep") {
        return cmd_eval_vs_grep(args);
    }
    let root: PathBuf = flag_value(args, "--root").map_or_else(|| ".".into(), PathBuf::from);
    let k: usize = flag_value(args, "--commits")
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);
    let store = RedbStore::open(db_path(&root)?);
    let graph = store.load()?;
    // a multi-root index namespaces module paths by root tag, so raw git paths
    // must be namespaced the same way or every lookup misses
    let tag = root_tag(&store, &root)?;

    let indexed = |p: &str| graph.get(ir::SymbolId::module(p)).is_some();
    let static_kinds = [
        EdgeKind::Calls,
        EdgeKind::Imports,
        EdgeKind::GraphqlCall,
        EdgeKind::DbQuery,
    ];
    // co-change is scored on held-out commits: the newest k commits are the test
    // set, and the pair counts come from older history only. Reading the graph's
    // own ChangesWith edges instead would score the mining window against itself.
    let split = overlay::holdout(&root, k);
    let trained: std::collections::HashSet<(String, String)> = split
        .train
        .cochange
        .iter()
        .flat_map(|(a, b, _)| {
            let (a, b) = (resolve::namespace(&tag, a), resolve::namespace(&tag, b));
            [(a.clone(), b.clone()), (b, a)]
        })
        .collect();
    let cochange = |a: &str, b: &str| trained.contains(&(a.to_owned(), b.to_owned()));
    // pairs the training window could actually score: both ends indexed. Separates
    // "co-change learned nothing" from "co-change learned the wrong pairs".
    let trained_pairs = trained
        .iter()
        .filter(|(a, b)| a < b && indexed(a) && indexed(b))
        .count();
    let commits: Vec<Vec<String>> = split
        .test
        .iter()
        .map(|c| {
            c.files
                .iter()
                .map(|p| resolve::namespace(&tag, p))
                .collect()
        })
        .collect();
    let test_commits = commits.len();
    // cache each test file's statically-reachable file set (from all its symbols)
    let mut reach: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    let mut reach_of = |file: &str| -> std::collections::HashSet<String> {
        if let Some(s) = reach.get(file) {
            return s.clone();
        }
        let seeds: Vec<_> = graph.nodes_in_file(file).iter().map(|n| n.id).collect();
        let s = query::reachable_modules(&graph, &seeds, &static_kinds, 3);
        reach.insert(file.to_owned(), s.clone());
        s
    };

    let (mut pairs, mut stat, mut co, mut either) = (0u32, 0u32, 0u32, 0u32);
    for files in commits {
        let idx: Vec<String> = files.into_iter().filter(|p| indexed(p)).collect();
        for i in 0..idx.len() {
            for j in (i + 1)..idx.len() {
                let (a, b) = (&idx[i], &idx[j]);
                pairs += 1;
                let s = reach_of(a).contains(b.as_str()) || reach_of(b).contains(a.as_str());
                let c = cochange(a, b);
                if s {
                    stat += 1;
                }
                if c {
                    co += 1;
                }
                if s || c {
                    either += 1;
                }
            }
        }
    }
    let pct = |n: u32| {
        if pairs == 0 {
            0.0
        } else {
            100.0 * n as f32 / pairs as f32
        }
    };
    if pairs == 0 {
        println!(
            "no same-commit file pairs among indexed files — nothing to evaluate.\n  \
             held out {test_commits} of {k} requested commits of {}; the graph holds {} files.",
            root.display(),
            graph
                .nodes()
                .filter(|n| n.kind == ir::NodeKind::Module)
                .count()
        );
        return Ok(());
    }
    println!(
        "held-out co-change prediction ({test_commits} test commits, \
         {} training commits, {pairs} same-commit file pairs):",
        split.train_commits
    );
    println!("  static edges alone : {:.1}%  ({stat})", pct(stat));
    println!(
        "  co-change alone    : {:.1}%  ({co})   ← from {trained_pairs} trained pairs",
        pct(co)
    );
    println!("  fused (either)     : {:.1}%  ({either})", pct(either));
    println!(
        "  → co-change lifts recall by {:.1} pts over static-only",
        pct(either) - pct(stat)
    );
    if split.train_commits == 0 {
        println!(
            "  note: no commits older than the {test_commits}-commit test window, so \
             co-change had nothing to learn from — the 0.0% above is not a result."
        );
    }
    Ok(())
}

// ── MCP server (newline-delimited JSON-RPC over stdio) ──────────────────────

/// The full-index build the daemon (re)runs for a project: index-then-load, the
/// same pipeline `index`/`mcp` use, so a daemon graph is identical to a cold one.
fn daemon_build(root: &Path) -> Result<InMemoryGraph> {
    index_project(std::slice::from_ref(&root.to_path_buf()), None)?;
    daemon::load_persisted(own_db_path(root))
}

fn cmd_daemon(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        None | Some("run") => {
            daemon::set_builder(daemon_build);
            let cap = flag_value(args, "--max-resident").and_then(|s| s.parse().ok());
            daemon::run(cap)
        }
        Some("status") => {
            let r = daemon::request(&daemon::Request::Status)
                .context("no ripple daemon running (start one with `ripple daemon`)")?;
            println!("{}", serde_json::to_string_pretty(&r.data)?);
            Ok(())
        }
        Some("stop") => {
            let _ = daemon::request(&daemon::Request::Stop);
            println!("ripple daemon stopped");
            Ok(())
        }
        Some("register") => {
            let arg = args
                .get(1)
                .context("usage: ripple daemon register <path>")?;
            // resolve the path against the *client's* cwd before sending — the daemon
            // runs elsewhere (a systemd service under `/`), so a relative `.` would
            // otherwise be resolved against its cwd, not the user's
            let root = std::fs::canonicalize(arg)
                .with_context(|| format!("no such directory: {arg}"))?
                .to_string_lossy()
                .into_owned();
            let r = daemon::request(&daemon::Request::Register { root })
                .context("no ripple daemon running (start one with `ripple daemon`)")?;
            if r.ok {
                println!("{}", serde_json::to_string_pretty(&r.data)?);
                Ok(())
            } else {
                bail!(
                    "{}",
                    r.error.unwrap_or_else(|| "register failed".to_owned())
                )
            }
        }
        Some(other) => bail!("unknown daemon subcommand: {other} (run|status|stop|register)"),
    }
}

/// The projects one `ripple mcp` process answers for.
///
/// A tool call may name any `root`, so the server is no longer pinned to the
/// project it was launched in (#118). Each project's graph is loaded once and
/// kept, so an agent spanning an API repo and a web repo pays the load per
/// project rather than per call. Omitting `root` answers from the launch root —
/// the behavior every existing client already depends on.
struct McpSession {
    default_root: PathBuf,
    graphs: HashMap<PathBuf, InMemoryGraph>,
}

impl McpSession {
    /// Load the launch root eagerly, so a broken `--root` fails at startup
    /// rather than on the first tool call.
    fn new(default_root: PathBuf) -> Result<Self> {
        let mut session = Self {
            default_root: default_root.clone(),
            graphs: HashMap::new(),
        };
        session.load(&default_root)?;
        Ok(session)
    }

    /// The root a call targets: its `root` argument if given, else the launch root.
    ///
    /// A relative path resolves against the server's cwd. A path that isn't there
    /// is an error naming it — never an empty graph answering "no symbol".
    fn resolve_root(&self, arg: Option<&str>) -> Result<PathBuf, String> {
        let Some(arg) = arg else {
            return Ok(self.default_root.clone());
        };
        std::fs::canonicalize(arg).map_err(|e| format!("no such project root '{arg}': {e}"))
    }

    fn graph(&mut self, root: &Path) -> Result<&mut InMemoryGraph, String> {
        if !self.graphs.contains_key(root) {
            self.load(root).map_err(|e| format!("{e:#}"))?;
        }
        self.graphs
            .get_mut(root)
            .ok_or_else(|| format!("no graph for {}", root.display()))
    }

    /// (Re)load `root`'s graph, indexing it first if it was never indexed — so an
    /// agent can point at a repo ripple has not seen before.
    fn load(&mut self, root: &Path) -> Result<()> {
        if !db_path(root)?.exists() {
            index_project(&[root.to_path_buf()], None)?;
        }
        let graph = RedbStore::open(db_path(root)?).load()?;
        self.graphs.insert(root.to_path_buf(), graph);
        Ok(())
    }
}

fn cmd_mcp(args: &[String]) -> Result<()> {
    let root_arg = flag_value(args, "--root").unwrap_or(".");
    // canonical from the start: the same path spelled two ways must not load two
    // graphs, and `root_tag` compares against canonical indexed roots anyway
    let root = std::fs::canonicalize(root_arg)
        .with_context(|| format!("no such directory: {root_arg}"))?;
    let mut session = McpSession::new(root)?;

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = req.get("id").cloned();
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        if id.is_none() {
            continue; // notifications get no response
        }
        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "ripple", "version": env!("CARGO_PKG_VERSION") }
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": mcp_tools() })),
            "tools/call" => mcp_call(&mut session, req.get("params")),
            other => Err(format!("method not found: {other}")),
        };
        let resp = match result {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err(e) => {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32603, "message": e } })
            }
        };
        writeln!(stdout, "{resp}")?;
        stdout.flush()?;
    }
    Ok(())
}

/// The `root` argument every tool accepts, so an agent can span projects instead
/// of being stuck with the one the server was launched in (#118).
const ROOT_DESC: &str = "project root to answer from — an absolute path, or one relative to the server's working directory. Omit to use the root `ripple mcp` was launched with.";

fn mcp_tools() -> Value {
    // every tool takes `root`; declaring it once keeps the description identical
    // across the nine schemas an agent reads
    let obj = |props: Value, req: Value| {
        let mut props = props;
        if let Some(map) = props.as_object_mut() {
            map.insert(
                "root".to_owned(),
                json!({ "type": "string", "description": ROOT_DESC }),
            );
        }
        json!({ "type": "object", "properties": props, "required": req })
    };
    json!([
        {
            "name": "locate",
            "description": "Where do I start for a task? Give the task in plain words (\"implement rate limiting on login\") and get risk- and centrality-ranked starting symbols across all indexed repos, each with why it matched and a one-hop blast-radius preview. Call this first for an implement/feature task, as `search` is for disambiguating a known name.",
            "inputSchema": obj(json!({
                "task": { "type": "string", "description": "the task in plain words; matched against names, paths, routes, and doc comments" },
                "budget": { "type": "integer", "description": "max seeds (default 10)" }
            }), json!(["task"]))
        },
        {
            "name": "search",
            "description": "Find symbols/files by name, path, or a few words describing the area (use this first to get exact names/paths for other tools). Best match first.",
            "inputSchema": obj(json!({
                "query": { "type": "string", "description": "one or more words; `_`, `-`, `.`, `/` split like spaces" },
                "limit": { "type": "integer", "description": "max results (default 30)" }
            }), json!(["query"]))
        },
        {
            "name": "impact",
            "description": "Risk-ranked blast radius of changing a symbol (what depends on it), across languages/services.",
            "inputSchema": obj(json!({
                "symbol": { "type": "string", "description": "exact symbol/qualified name (see `search`)" },
                "budget": { "type": "integer", "description": "max hits (default 20)" }
            }), json!(["symbol"]))
        },
        {
            "name": "review_focus",
            "description": "Rank the symbols changed in a diff by review priority (risk × downstream), with missing-co-change and untested flags.",
            "inputSchema": obj(json!({
                "base": { "type": "string", "description": "base rev; default = working tree vs HEAD" },
                "budget": { "type": "integer" }
            }), json!([]))
        },
        {
            "name": "neighbors",
            "description": "Direct callers/importers (in) or callees/imports (out) of a symbol.",
            "inputSchema": obj(json!({
                "symbol": { "type": "string" },
                "direction": { "type": "string", "enum": ["in", "out"] },
                "depth": { "type": "integer" },
                "limit": { "type": "integer", "description": "max results (default 50)" }
            }), json!(["symbol"]))
        },
        {
            "name": "risk",
            "description": "Git-derived risk (churn, bug-density, ownership, composite) for a symbol or file.",
            "inputSchema": obj(json!({ "target": { "type": "string" } }), json!(["target"]))
        },
        {
            "name": "path",
            "description": "How does one symbol reach another? Routes along dependency direction, shortest first, with the product of the edge confidences.",
            "inputSchema": obj(json!({
                "from": { "type": "string", "description": "exact or partial symbol/file name" },
                "to": { "type": "string" },
                "depth": { "type": "integer", "description": "max hops (default 6)" },
                "limit": { "type": "integer", "description": "max routes (default 3)" }
            }), json!(["from", "to"]))
        },
        {
            "name": "explain_edge",
            "description": "Why are two symbols connected? Returns the edge kind, confidence, provenance (Extracted/LspVerified/CoChange), and site between them.",
            "inputSchema": obj(json!({ "from": { "type": "string" }, "to": { "type": "string" } }), json!(["from", "to"]))
        },
        {
            "name": "reindex",
            "description": "Rebuild the graph from the current source (call after editing code so results aren't stale).",
            "inputSchema": obj(json!({}), json!([]))
        }
    ])
}

/// The argument names a tool accepts, read off its own `inputSchema`.
///
/// Derived rather than declared so the check can never disagree with the schema
/// the agent read out of `tools/list`.
fn tool_arg_names(tool: &str) -> Option<Vec<String>> {
    let tools = mcp_tools();
    let schema = tools
        .as_array()?
        .iter()
        .find(|t| t.get("name").and_then(Value::as_str) == Some(tool))?;
    let props = schema.get("inputSchema")?.get("properties")?.as_object()?;
    let mut names: Vec<String> = props.keys().cloned().collect();
    names.sort();
    Some(names)
}

/// Reject arguments a tool does not accept.
///
/// A silently dropped key reads as an answer: `impact` with a misspelled `root`
/// queried the launch project and replied "no symbol 'x'", which invites the
/// caller to conclude the code isn't there (#118).
fn check_args(tool: &str, args: &Value) -> Result<(), String> {
    let (Some(accepted), Some(given)) = (tool_arg_names(tool), args.as_object()) else {
        return Ok(()); // an unknown tool is the dispatch's error to report, not ours
    };
    let unknown: Vec<String> = given
        .keys()
        .filter(|k| !accepted.contains(k))
        .map(|k| format!("'{k}'"))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    let plural = if unknown.len() == 1 {
        "argument"
    } else {
        "arguments"
    };
    Err(format!(
        "unknown {plural} for tool '{tool}': {} — accepted: {}",
        unknown.join(", "),
        accepted.join(", ")
    ))
}

fn mcp_text(v: Value) -> Value {
    json!({ "content": [{ "type": "text", "text": v.to_string() }] })
}

/// Split a query into lowercase terms. Word separators an agent is likely to type
/// (`_`, `-`, `.`, `/`, `::`) are treated as spaces, so `review focus`,
/// `review_focus` and `query/review` all reach the same symbol.
fn search_terms(query: &str) -> Vec<String> {
    query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect()
}

/// How well a node answers a query, summed over its terms. Zero means no term
/// matched at all and the node is dropped.
///
/// Terms are scored independently and summed rather than required to all match:
/// an agent describing an area ("incremental reindex cache") writes words that no
/// single symbol carries, and requiring all of them returned nothing at all —
/// which is what the plain substring match on the whole query used to do.
fn search_score(n: &ir::Node, terms: &[String]) -> u32 {
    let name = n.name.to_lowercase();
    let qualified = n.qualified_name.to_lowercase();
    let module = n.module_path.to_lowercase();
    let name_terms = search_terms(&name);
    let per_term: u32 = terms
        .iter()
        .map(|t| {
            if name_terms.contains(t) {
                4 // a whole word of the name
            } else if name.starts_with(t.as_str()) {
                3
            } else if name.contains(t.as_str()) {
                2
            } else if qualified.contains(t.as_str()) || module.contains(t.as_str()) {
                1
            } else {
                0
            }
        })
        .sum();
    // the whole query names the whole symbol — `review focus` and `review_focus`
    // both land on `review_focus`, and must outrank the symbol merely called
    // `review`, which one strong term alone would otherwise win
    let exact = {
        let (mut q, mut s) = (terms.to_vec(), name_terms);
        q.sort();
        s.sort();
        q == s
    };
    per_term + if exact { 8 } else { 0 }
}

fn mcp_call(session: &mut McpSession, params: Option<&Value>) -> Result<Value, String> {
    let params = params.ok_or("missing params")?;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or("missing tool name")?;
    let a = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    check_args(name, &a)?;
    let str_arg = |k: &str| a.get(k).and_then(Value::as_str).map(str::to_owned);
    let usize_arg = |k: &str, d: usize| a.get(k).and_then(Value::as_u64).map_or(d, |n| n as usize);

    let root = session.resolve_root(str_arg("root").as_deref())?;
    let root = root.as_path();
    let graph = session.graph(root)?;

    match name {
        "reindex" => {
            // re-index the set the graph was built from, not just this root:
            // narrowing a multi-root index to one root drops the other repo's
            // symbols and leaves it pointing at a graph that no longer knows it
            let store = RedbStore::open(db_path(root).map_err(|e| e.to_string())?);
            let recorded: Vec<PathBuf> = store
                .read_roots()
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|(_, p)| p)
                .collect();
            let roots = if recorded.is_empty() {
                vec![root.to_path_buf()]
            } else {
                recorded
            };
            let summary = index_project(&roots, None).map_err(|e| e.to_string())?;
            let db = db_path(root).map_err(|e| e.to_string())?;
            *graph = RedbStore::open(db).load().map_err(|e| e.to_string())?;
            Ok(mcp_text(json!({ "reindexed": summary })))
        }
        "locate" => {
            let task = str_arg("task").ok_or("task required")?;
            let budget = usize_arg("budget", 10);
            let store = RedbStore::open(db_path(root).map_err(|e| e.to_string())?);
            let tags = store.read_roots().map_err(|e| e.to_string())?;
            let located = query::locate(graph, &task, budget);
            Ok(mcp_text(locate_payload(&located, &tags)))
        }
        "search" => {
            let terms = search_terms(&str_arg("query").ok_or("query required")?);
            let limit = usize_arg("limit", 30);
            let mut hits: Vec<_> = graph
                .nodes()
                .filter_map(|n| match search_score(n, &terms) {
                    0 => None,
                    s => Some((s, n)),
                })
                .collect();
            // best match first; the name/path pair keeps equal scores in a fixed order
            hits.sort_by(|(sa, a), (sb, b)| {
                sb.cmp(sa).then_with(|| {
                    (a.module_path.as_str(), a.name.as_str())
                        .cmp(&(b.module_path.as_str(), b.name.as_str()))
                })
            });
            let total = hits.len();
            let out: Vec<_> = hits
                .iter()
                .take(limit)
                .map(|(score, n)| {
                    json!({
                        "symbol": n.name, "qualified": n.qualified_name, "module": n.module_path,
                        "kind": format!("{:?}", n.kind), "score": score,
                    })
                })
                .collect();
            Ok(mcp_text(
                json!({ "shown": out.len(), "total": total, "results": out }),
            ))
        }
        "impact" => {
            let sym = str_arg("symbol").ok_or("symbol required")?;
            let seeds: Vec<_> = graph
                .lookup(&sym)
                .map(|(nodes, _)| nodes)
                .unwrap_or_default()
                .into_iter()
                .map(|n| n.id)
                .collect();
            if seeds.is_empty() {
                return Ok(mcp_text(
                    json!({ "error": format!("no symbol '{sym}' — try `search`") }),
                ));
            }
            let result = query::impact(graph, &seeds, usize_arg("budget", 20));
            let out: Vec<_> = result
                .hits
                .iter()
                .map(|h| {
                    let parent = graph.get(h.from);
                    json!({
                        "symbol": h.node.name, "module": h.node.module_path,
                        "score": h.score, "weight": h.weight, "depth": h.depth,
                        "via": format!("{:?}", h.via), "risk": h.node.risk.composite,
                        "from": parent.map(|n| n.name.clone()),
                        "from_module": parent.map(|n| n.module_path.clone()),
                    })
                })
                .collect();
            Ok(mcp_text(json!({
                "seeds_matched": seeds.len(),
                "shown": result.hits.len(),
                "reached": result.reached,
                "blast_radius": out,
            })))
        }
        "review_focus" => {
            // the same namespacing the CLI does: a multi-root index stores
            // tag-prefixed module paths, and an unnamespaced diff matches none of
            // them — this tool answered "nothing changed" for every multi-root repo
            let store = RedbStore::open(db_path(root).map_err(|e| e.to_string())?);
            let tag = root_tag(&store, root).map_err(|e| e.to_string())?;
            let changed = namespaced(&tag, overlay::diff_lines(root, str_arg("base").as_deref()));
            let r = query::review_focus(graph, &changed, usize_arg("budget", 15), &tag);
            Ok(mcp_text(review_payload(&r)))
        }
        "neighbors" => {
            let sym = str_arg("symbol").ok_or("symbol required")?;
            let dir = if str_arg("direction").as_deref() == Some("in") {
                Dir::In
            } else {
                Dir::Out
            };
            let depth = usize_arg("depth", 1);
            let limit = usize_arg("limit", 50);
            let mut out = Vec::new();
            for start in graph.lookup(&sym).map(|(n, _)| n).unwrap_or_default() {
                for h in graph.neighbors(start.id, dir, Some(&NEIGHBOR_KINDS), depth) {
                    out.push(json!({
                        "symbol": h.node.name, "module": h.node.module_path,
                        "edge": format!("{:?}", h.edge.kind), "confidence": h.edge.confidence, "depth": h.depth,
                    }));
                }
            }
            let total = out.len();
            out.truncate(limit);
            Ok(mcp_text(
                json!({ "shown": out.len(), "total": total, "neighbors": out }),
            ))
        }
        "risk" => {
            let t = str_arg("target").ok_or("target required")?;
            let mut hits = graph.lookup(&t).map(|(n, _)| n).unwrap_or_default();
            if hits.is_empty() {
                hits = graph.nodes_in_file(&t);
            }
            let out: Vec<_> = hits
                .iter()
                .take(50)
                .map(|n| {
                    json!({
                        "symbol": n.name, "module": n.module_path,
                        "composite": n.risk.composite, "churn": n.risk.churn,
                        "bug_density": n.risk.bug_density, "ownership": n.risk.ownership,
                    })
                })
                .collect();
            Ok(mcp_text(json!({ "risk": out })))
        }
        "path" => {
            let from = str_arg("from").ok_or("from required")?;
            let to = str_arg("to").ok_or("to required")?;
            let widen = |q: &str| graph.lookup(q).map(|(n, _)| n).unwrap_or_default();
            let (depth, limit) = (usize_arg("depth", 6), usize_arg("limit", 3));
            let mut routes = Vec::new();
            for s in widen(&from) {
                for t in widen(&to) {
                    if s.id == t.id {
                        continue;
                    }
                    for r in query::paths(graph, s.id, t.id, depth, limit) {
                        routes.push(json!({
                            "hops": r.steps.len(),
                            "confidence": r.confidence,
                            "from": s.name, "from_module": s.module_path,
                            "steps": r.steps.iter().map(|st| json!({
                                "edge": format!("{:?}", st.edge.kind),
                                "edge_confidence": st.edge.confidence,
                                "site_line": st.edge.site.start_line,
                                "symbol": st.node.name, "module": st.node.module_path,
                            })).collect::<Vec<_>>(),
                        }));
                    }
                }
            }
            routes.truncate(limit);
            Ok(mcp_text(json!({ "routes": routes })))
        }
        "explain_edge" => {
            let from = str_arg("from").ok_or("from required")?;
            let to = str_arg("to").ok_or("to required")?;
            let widen = |q: &str| graph.lookup(q).map(|(n, _)| n).unwrap_or_default();
            let to_ids: std::collections::HashSet<_> =
                widen(&to).into_iter().map(|n| n.id).collect();
            let mut out = Vec::new();
            for f in widen(&from) {
                for e in graph.out_edges(f.id).iter().chain(graph.in_edges(f.id)) {
                    let other = if e.src == f.id { e.dst } else { e.src };
                    if to_ids.contains(&other) {
                        out.push(json!({
                            "kind": format!("{:?}", e.kind), "confidence": e.confidence,
                            "source": format!("{:?}", e.source),
                            "direction": if e.src == f.id { "from→to" } else { "to→from" },
                            "site_line": e.site.start_line,
                        }));
                    }
                }
            }
            Ok(mcp_text(json!({ "edges": out })))
        }
        other => Err(format!("unknown tool: {other}")),
    }
}

fn cmd_review(args: &[String]) -> Result<()> {
    let json = args.iter().any(|a| a == "--json");
    let root: PathBuf = flag_value(args, "--root").map_or_else(|| ".".into(), PathBuf::from);
    let budget: usize = flag_value(args, "--budget")
        .and_then(|s| s.parse().ok())
        .unwrap_or(15);
    let base = positional(args); // optional rev; default = working tree vs HEAD

    let mut store = RedbStore::open(db_path(&root)?);
    // a multi-root index stores tag-prefixed module paths, and review compares the
    // diff's paths to them by equality — unnamespaced, every match silently missed
    let tag = root_tag(&store, &root)?;
    let changed = namespaced(&tag, overlay::diff_lines(&root, base.map(String::as_str)));
    if changed.is_empty() {
        println!("no changes to review (vs {})", base.map_or("HEAD", |s| s));
        return Ok(());
    }
    let mut graph = load_current(&store, &root, args.iter().any(|a| a == "--sync"))?;
    let seeds: Vec<ir::SymbolId> = changed
        .keys()
        .flat_map(|f| graph.nodes_in_file(f))
        .map(|n| n.id)
        .collect();
    graph = verify_upgrade(&mut store, graph, &root, args, &seeds, json)?;
    let r = query::review_focus(&graph, &changed, budget, &tag);
    // the spans a diff is attributed to come from the index, so an index older
    // than the edits attributes changed lines to whatever used to be there
    if !args.iter().any(|a| a == "--sync") {
        warn_if_stale(&store, &changed.keys().map(String::as_str).collect());
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&review_payload(&r))?);
    } else {
        let cut = r.total.saturating_sub(r.focus.len());
        let note = if cut > 0 {
            format!(" — {cut} more cut by --budget {budget}")
        } else {
            String::new()
        };
        println!(
            "review focus ({} of {} changed symbols), highest priority first{note}:",
            r.focus.len(),
            r.total
        );
        for f in &r.focus {
            println!(
                "  {:.2}  {} ({})  — {}",
                f.review_priority,
                f.node.name,
                f.node.module_path,
                if f.reasons.is_empty() {
                    "—".into()
                } else {
                    f.reasons.join(", ")
                }
            );
        }
        if !r.missing_cochange.is_empty() {
            println!("\n⚠ expected co-changes absent (usually changed together):");
            for n in r.missing_cochange.iter().take(8) {
                println!("  {}", n.module_path);
            }
        }
    }
    Ok(())
}

fn cmd_risk(args: &[String]) -> Result<()> {
    let json = args.iter().any(|a| a == "--json");
    let query = positional(args).context(USAGE)?.clone();
    let root: PathBuf = flag_value(args, "--root").map_or_else(|| ".".into(), PathBuf::from);

    let graph = RedbStore::open(db_path(&root)?).load()?;
    // match by symbol name/qualified name, or by module path (file)
    let mut hits: Vec<_> = graph
        .lookup(&query)
        .map(|(nodes, _)| nodes)
        .unwrap_or_default()
        .into_iter()
        .map(|n| (n.name.clone(), n.module_path.clone(), n.risk))
        .collect();
    if hits.is_empty() {
        hits = graph
            .nodes_in_file(&query)
            .into_iter()
            .map(|n| (n.name.clone(), n.module_path.clone(), n.risk))
            .collect();
    }
    if hits.is_empty() {
        bail!("no symbol or file matching: {query}");
    }

    if json {
        let out: Vec<_> = hits
            .iter()
            .map(|(name, module, r)| {
                serde_json::json!({
                    "name": name, "module": module,
                    "composite": r.composite, "churn": r.churn,
                    "bug_density": r.bug_density, "ownership": r.ownership,
                    "fanout": r.fanout, "test_proximity": r.test_proximity,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        for (name, module, r) in &hits {
            println!(
                "{name} ({module})\n  composite {:.2} | churn {:.2} bug {:.2} ownership {:.2} fanout {:.2} tested {:.2}",
                r.composite, r.churn, r.bug_density, r.ownership, r.fanout, r.test_proximity
            );
        }
    }
    Ok(())
}

fn cmd_neighbors(args: &[String]) -> Result<()> {
    let json = args.iter().any(|a| a == "--json");
    let symbol = positional(args).context(USAGE)?.clone();
    let root: PathBuf = flag_value(args, "--root").map_or_else(|| ".".into(), PathBuf::from);
    let dir = if args.iter().any(|a| a == "--in") {
        Dir::In
    } else {
        Dir::Out
    };
    let depth: usize = flag_value(args, "--depth")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    let store = RedbStore::open(db_path(&root)?);
    let graph = store.load()?;

    let matches = lookup_or_bail(&graph, &symbol, json, args)?;
    warn_if_stale(
        &store,
        &matches.iter().map(|n| n.module_path.as_str()).collect(),
    );

    let arrow = if dir == Dir::In {
        "callers/importers of"
    } else {
        "neighbors of"
    };
    for start in matches {
        let hops = graph.neighbors(start.id, dir, Some(&NEIGHBOR_KINDS), depth);
        if json {
            let out: Vec<_> = hops
                .iter()
                .map(|h| {
                    serde_json::json!({
                        "name": h.node.name, "kind": format!("{:?}", h.node.kind),
                        "edge": format!("{:?}", h.edge.kind), "confidence": h.edge.confidence,
                        "module": h.node.module_path, "depth": h.depth,
                        // which node this hop hangs off — without it a depth-3 walk is
                        // a flat list and the chain can't be reconstructed
                        "from": parent_of(h, dir).and_then(|p| graph.get(p)).map(|n| n.name.clone()),
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&out)?);
        } else {
            println!(
                "{arrow} {} ({}) [{}]",
                start.name,
                start.module_path,
                format_args!("{:?}", start.kind)
            );
            for row in &tree_order(start.id, &hops, dir) {
                let h = &row.hop;
                // several call sites are several edges; collapsing them silently would
                // hide that a component renders the target three times (#28)
                let times = if row.sites > 1 {
                    format!(" ×{}", row.sites)
                } else {
                    String::new()
                };
                let indent = "  ".repeat(h.depth);
                println!(
                    "{indent}{:?}<{:.2}>{times} {} ({})",
                    h.edge.kind, h.edge.confidence, h.node.name, h.node.module_path
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn vs_grep_metrics() {
        let truth: HashSet<String> = ["b.ts", "c.ts"].iter().map(|s| (*s).to_owned()).collect();
        let ranked = v(&["x.ts", "b.ts", "y.ts", "c.ts"]);
        // b.ts and c.ts both inside the top 4 → full recall; top-1 misses both → 0
        assert!((recall_at_k(&ranked, &truth, 4) - 1.0).abs() < f32::EPSILON);
        assert!((recall_at_k(&ranked, &truth, 1) - 0.0).abs() < f32::EPSILON);
        assert!((recall_at_k(&ranked, &truth, 2) - 0.5).abs() < f32::EPSILON);
        // first truth hit is at index 1 → reciprocal rank 1/2
        assert!((reciprocal_rank(&ranked, &truth) - 0.5).abs() < f32::EPSILON);
        assert_eq!(reciprocal_rank(&v(&["x.ts"]), &truth), 0.0);
        // empty truth is a no-score, never a divide-by-zero
        assert_eq!(recall_at_k(&ranked, &HashSet::new(), 4), 0.0);
    }

    #[test]
    fn identifier_tokens_splits_on_non_word_and_drops_shorts() {
        let toks = identifier_tokens(b"const markDmAsRead = (a, b) => foo_bar.x;");
        assert!(toks.contains("markDmAsRead"));
        assert!(toks.contains("foo_bar"));
        assert!(toks.contains("const"));
        // one- and two-char names are grep noise (`a`, `b`, `x`) and are dropped
        assert!(!toks.contains("a"));
        assert!(!toks.contains("x"));
    }

    fn hop(src: &str, dst: &str, depth: usize) -> store::Hop {
        let span = ir::Span {
            start_line: 1,
            start_col: 1,
            end_line: 1,
            end_col: 1,
        };
        let node = |name: &str| ir::Node {
            id: ir::SymbolId::of("m.ex", name),
            kind: ir::NodeKind::Function,
            name: name.to_owned(),
            qualified_name: name.to_owned(),
            module_path: "m.ex".to_owned(),
            span,
            extra_spans: Vec::new(),
            is_exported: true,
            risk: ir::RiskScores::default(),
            doc: None,
            route_path: None,
        };
        store::Hop {
            edge: ir::Edge {
                src: node(src).id,
                dst: node(dst).id,
                kind: EdgeKind::Calls,
                confidence: 1.0,
                site: span,
                source: ir::EdgeSource::Extracted,
            },
            node: node(dst),
            depth,
        }
    }

    fn sym(name: &str, module: &str) -> ir::Node {
        let span = ir::Span {
            start_line: 1,
            start_col: 1,
            end_line: 1,
            end_col: 1,
        };
        ir::Node {
            id: ir::SymbolId::of(module, name),
            kind: ir::NodeKind::Function,
            name: name.to_owned(),
            qualified_name: name.to_owned(),
            module_path: module.to_owned(),
            span,
            extra_spans: Vec::new(),
            is_exported: true,
            risk: ir::RiskScores::default(),
            doc: None,
            route_path: None,
        }
    }

    /// `search` matched the whole query as one substring, so any query with a space
    /// in it — the way an agent describes an area — matched nothing at all.
    #[test]
    fn a_multi_word_query_finds_what_one_substring_could_not() {
        let n = sym("build_incremental", "crates/resolve/src/lib.rs");
        assert_eq!(
            search_score(&n, &search_terms("build_incremental")),
            16,
            "both words are the name: 4 + 4, plus the whole-symbol bonus"
        );
        assert_eq!(
            search_score(&n, &search_terms("incremental build")),
            16,
            "the same symbol, described in two words, in either order"
        );
        assert_eq!(
            search_score(&n, &search_terms("resolve incremental")),
            5,
            "one term off the path (1) plus one whole word of the name (4)"
        );
        assert_eq!(search_score(&n, &search_terms("elixir dsl")), 0);
    }

    /// Separators an agent types instead of spaces must not glue terms together.
    #[test]
    fn search_terms_split_on_word_separators() {
        assert_eq!(
            search_terms("query/review_focus"),
            v(&["query", "review", "focus"])
        );
        assert_eq!(search_terms("Store::load"), v(&["store", "load"]));
        assert_eq!(search_terms("   "), Vec::<String>::new());
    }

    /// More terms hit must outrank a single lucky one, or a multi-word query is
    /// no better than its worst word.
    #[test]
    fn a_symbol_matching_more_terms_ranks_higher() {
        let both = sym("review_focus", "crates/query/src/lib.rs");
        let one = sym("review", "crates/cli/src/verify.rs");
        let terms = search_terms("review focus");
        assert!(
            search_score(&both, &terms) > search_score(&one, &terms),
            "review_focus (both words) must beat review (one)"
        );
    }

    /// A depth-2 walk printed every second-level hop at the same indentation whatever
    /// it came from, so the output could not be read as a chain. Each hop must follow
    /// its own parent.
    #[test]
    fn a_walk_is_ordered_under_its_parents() {
        let root = ir::SymbolId::of("m.ex", "root");
        // root → a → a2, root → b, plus an orphan whose parent was never reached
        // `b` twice from the same parent: two call sites, one row, count of 2
        let hops = vec![
            hop("root", "a", 1),
            hop("root", "b", 1),
            hop("root", "b", 1),
            hop("a", "a2", 2),
            hop("nowhere", "orphan", 2),
        ];
        let names: Vec<(String, usize)> = tree_order(root, &hops, Dir::Out)
            .into_iter()
            .map(|r| (r.hop.node.name, r.sites))
            .collect();
        assert_eq!(
            names,
            vec![
                ("a".to_owned(), 1),
                ("a2".to_owned(), 1),
                ("b".to_owned(), 2),
                ("orphan".to_owned(), 1)
            ],
            "a2 follows a; b is reached twice so it says so once; an unreachable hop is \
             still printed rather than dropped"
        );
    }

    /// The parser reads its flag table off `USAGE`, so the failure that remains is
    /// a flag the code consults but never documents: it would take no value, and
    /// its argument would come back as a positional — the `--root <path>` bug (#24).
    #[test]
    fn every_value_flag_the_code_reads_is_in_usage() {
        let source = include_str!("main.rs");
        let mut undocumented = Vec::new();
        for (i, _) in source.match_indices("flag_value(args, \"") {
            let rest = &source[i + "flag_value(args, \"".len()..];
            let Some(end) = rest.find('"') else { continue };
            let flag = &rest[..end];
            if !USAGE.contains(flag) {
                undocumented.push(flag.to_owned());
            }
        }
        undocumented.sort();
        undocumented.dedup();
        assert!(
            undocumented.is_empty(),
            "flags read by the code but absent from USAGE, so the parser will not \
             skip their values: {undocumented:?}"
        );
        // and the derivation itself works
        assert!(value_flags().contains("--root"));
        assert!(value_flags().contains("--in-file"));
        assert!(!value_flags().contains("--json"), "--json takes no value");
    }

    #[test]
    fn verify_budget_accepts_s_ms_and_bare_millis() {
        use std::time::Duration;
        assert_eq!(parse_duration(Some("2s")), Some(Duration::from_secs(2)));
        assert_eq!(
            parse_duration(Some("500ms")),
            Some(Duration::from_millis(500))
        );
        assert_eq!(
            parse_duration(Some("750")),
            Some(Duration::from_millis(750))
        );
        assert_eq!(parse_duration(Some("soon")), None);
        assert_eq!(parse_duration(None), None);
    }

    #[test]
    fn positionals_skip_flag_values() {
        // --root's value must not leak as a positional (was a real bug)
        assert_eq!(positional(&v(&["--root", "/repo"])), None);
        assert_eq!(
            positional(&v(&["HEAD~5", "--root", "/repo"])).unwrap(),
            "HEAD~5"
        );
        assert_eq!(positional(&v(&["--root", "/repo", "sym"])).unwrap(), "sym");
        assert_eq!(
            positional(&v(&["--in", "--root", "/r", "sym", "--depth", "2"])).unwrap(),
            "sym"
        );
        assert_eq!(positionals(&v(&["a", "--root", "/r", "b"])), vec!["a", "b"]);
    }

    /// Path-based commands need the index's root tag, or a multi-root graph looks
    /// empty to them (`eval` silently reported 0 pairs before this).
    #[test]
    fn root_tag_comes_from_the_index() {
        let dir = std::env::temp_dir().join(format!("ripple-roottag-{}", std::process::id()));
        let (web, api) = (dir.join("web"), dir.join("api"));
        std::fs::create_dir_all(&web).unwrap();
        std::fs::create_dir_all(&api).unwrap();
        let mut store = RedbStore::open(own_db_path(&web));

        // no roots recorded yet (an index built before roots existed)
        assert_eq!(root_tag(&store, &web).unwrap(), "");

        store
            .write_roots(&[
                ("web".to_owned(), web.canonicalize().unwrap()),
                ("api".to_owned(), api.canonicalize().unwrap()),
            ])
            .unwrap();
        assert_eq!(root_tag(&store, &web).unwrap(), "web");
        assert_eq!(root_tag(&store, &api).unwrap(), "api");
        // an unindexed root is an error, not a silently empty result
        assert!(root_tag(&store, &dir).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn named(name: &str, module: &str) -> ir::Node {
        ir::Node {
            id: ir::SymbolId::of(module, name),
            kind: ir::NodeKind::Function,
            name: name.to_owned(),
            qualified_name: name.to_owned(),
            module_path: module.to_owned(),
            span: ir::Span {
                start_line: 1,
                start_col: 1,
                end_line: 1,
                end_col: 1,
            },
            extra_spans: Vec::new(),
            is_exported: true,
            risk: ir::RiskScores::default(),
            doc: None,
            route_path: None,
        }
    }

    /// Six unrelated `run`s in three languages used to be seeded as one symbol,
    /// and the union read exactly like one symbol's blast radius (#37).
    #[test]
    fn an_exact_name_matching_several_symbols_says_so() {
        let (a, b) = (named("run", "verify.rs"), named("run", "b.ts"));

        let note = lookup_note("run", &[&a, &b], store::Match::Exact).expect("ambiguous");
        assert!(note.contains("matches 2 symbols in 2 files"), "{note}");
        assert!(note.contains("--in-file"), "{note}");

        assert!(
            lookup_note("run", &[&a], store::Match::Exact).is_none(),
            "one exact match is not worth a word"
        );
        // a looser rule still says which rule fired
        let loose = lookup_note("run", &[&a], store::Match::Substring).expect("loose");
        assert!(loose.contains("substring"), "{loose}");
    }

    /// The CLI and the MCP tool answer the same question and must not drift: the
    /// MCP one silently missed every fix the CLI gained until they shared this.
    #[test]
    fn the_review_payload_carries_what_a_caller_needs_to_read_it() {
        let node = named("changed", "a.ts");
        let r = query::ReviewResult {
            focus: vec![query::FocusItem {
                node,
                review_priority: 1.5,
                downstream: 2,
                changed_lines: 7,
                reasons: vec!["untested".into()],
            }],
            total: 9,
            missing_cochange: Vec::new(),
            untested: Vec::new(),
            tests_known: true,
        };
        let v = review_payload(&r);
        assert_eq!(v["total"], 9, "the pre-truncation count survives (#41)");
        assert_eq!(v["untested_known"], true, "tested vs unknowable (#36)");
        assert_eq!(v["focus"][0]["changed_lines"], 7);
        assert_eq!(v["focus"][0]["symbol"], "changed");
    }

    /// A renamed function kept answering with a 0.95 next to it, because nothing
    /// ever compared the graph against disk (#38).
    #[test]
    fn a_changed_file_is_reported_stale_and_an_unchanged_one_is_not() {
        let dir = std::env::temp_dir().join(format!("ripple-stale-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (same, edited) = (dir.join("same.ts"), dir.join("edited.ts"));
        std::fs::write(&same, "export const a = 1;\n").unwrap();
        std::fs::write(&edited, "export const b = 1;\n").unwrap();

        let stamp = |p: &std::path::Path, module: &str, text: &str| {
            (
                module.to_owned(),
                store::FileStamp {
                    canonical: p.to_owned(),
                    module_path: module.to_owned(),
                    hash: parse::content_hash(text),
                },
            )
        };
        let stamps = HashMap::from([
            stamp(&same, "same.ts", "export const a = 1;\n"),
            stamp(&edited, "edited.ts", "export const b = 1;\n"),
            stamp(&dir.join("gone.ts"), "gone.ts", "whatever"),
        ]);

        std::fs::write(&edited, "export const b = 2;\n").unwrap();
        let asked = HashSet::from(["same.ts", "edited.ts", "gone.ts", "never-indexed.ts"]);
        assert_eq!(
            stale_modules(&stamps, &asked),
            vec!["edited.ts".to_owned(), "gone.ts".to_owned()],
            "a rewritten file and a deleted one are stale; an unchanged one and an \
             unindexed one are not"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn in_file_narrows_an_ambiguous_name() {
        let (a, b) = (named("run", "crates/cli/verify.rs"), named("run", "b.ts"));
        let args = v(&["run", "--in-file", "verify.rs"]);

        let narrowed = narrow_to_file(vec![&a, &b], "run", &args).unwrap();
        assert_eq!(narrowed.len(), 1);
        assert_eq!(narrowed[0].module_path, "crates/cli/verify.rs");

        // no match is an error naming the filter, not a silent empty answer
        let miss = v(&["run", "--in-file", "nowhere.ex"]);
        assert!(narrow_to_file(vec![&a, &b], "run", &miss).is_err());
        // and without the flag nothing is dropped
        assert_eq!(
            narrow_to_file(vec![&a, &b], "run", &v(&["run"]))
                .unwrap()
                .len(),
            2
        );
    }

    /// `load_current(sync=true)` answers from the working tree, not the last index:
    /// a function added after indexing is invisible to a plain load and present under
    /// sync, with no re-index and nothing written. See docs/17-keeping-the-graph-in-sync.md.
    #[test]
    fn sync_reflects_working_tree_edits_without_reindex() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::write(
            root.join("util.ts"),
            "export function helper() { return 1; }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("main.ts"),
            "import { helper } from \"./util\";\nexport function boot() { return helper(); }\n",
        )
        .unwrap();
        index_project(std::slice::from_ref(&root.to_path_buf()), None).expect("index");

        // a new exported function appears on disk, unindexed
        std::fs::write(
            root.join("util.ts"),
            "export function helper() { return 1; }\nexport function newFn() { return 2; }\n",
        )
        .unwrap();

        let store = RedbStore::open(db_path(root).unwrap());
        let newfn = ir::SymbolId::of("util.ts", "newFn");

        let stale = load_current(&store, root, false).unwrap();
        assert!(
            stale.get(newfn).is_none(),
            "a plain load answers from the snapshot, which predates the edit"
        );

        let synced = load_current(&store, root, true).unwrap();
        assert!(
            synced.get(newfn).is_some(),
            "--sync rebuilds from the working tree and sees the new function"
        );
    }

    #[test]
    fn every_mcp_tool_accepts_a_root() {
        let tools = mcp_tools();
        for t in tools.as_array().unwrap() {
            let name = t.get("name").and_then(Value::as_str).unwrap();
            let root = t
                .pointer("/inputSchema/properties/root")
                .unwrap_or_else(|| panic!("{name} has no root argument"));
            assert_eq!(root.get("type").and_then(Value::as_str), Some("string"));
            assert_eq!(
                root.get("description").and_then(Value::as_str),
                Some(ROOT_DESC)
            );
            // root stays optional so pre-#118 clients keep working
            let required = t.pointer("/inputSchema/required").unwrap();
            assert!(
                !required
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|r| r.as_str() == Some("root")),
                "{name} must not require root"
            );
        }
    }

    #[test]
    fn unknown_mcp_args_are_rejected_naming_the_key_and_the_accepted_ones() {
        // the #118 report: a misspelled root used to be dropped, and the answer
        // came back as "no symbol", which reads as "the code isn't there"
        let err = check_args("impact", &json!({ "symbol": "x", "rooot": "/tmp" })).unwrap_err();
        assert_eq!(
            err,
            "unknown argument for tool 'impact': 'rooot' — accepted: budget, root, symbol"
        );

        let err = check_args("locate", &json!({ "task": "x", "a": 1, "b": 2 })).unwrap_err();
        assert_eq!(
            err,
            "unknown arguments for tool 'locate': 'a', 'b' — accepted: budget, root, task"
        );

        assert!(check_args("impact", &json!({ "symbol": "x", "root": "/tmp" })).is_ok());
        assert!(check_args("impact", &json!({})).is_ok());
        // an unknown tool is the dispatch's error to report
        assert!(check_args("nope", &json!({ "whatever": 1 })).is_ok());
    }

    #[test]
    fn mcp_session_resolves_roots_and_names_a_missing_one() {
        let dir = tempfile::tempdir().unwrap();
        let launch = dir.path().canonicalize().unwrap();
        let session = McpSession {
            default_root: launch.clone(),
            graphs: HashMap::new(),
        };
        // omitted root keeps the launch project
        assert_eq!(session.resolve_root(None).unwrap(), launch);
        // relative paths resolve against the server's cwd, absolute ones as given
        assert_eq!(
            session
                .resolve_root(Some(&launch.to_string_lossy()))
                .unwrap(),
            launch
        );
        let err = session
            .resolve_root(Some("/definitely/not/a/repo"))
            .unwrap_err();
        assert!(
            err.contains("no such project root '/definitely/not/a/repo'"),
            "{err}"
        );
    }
}
