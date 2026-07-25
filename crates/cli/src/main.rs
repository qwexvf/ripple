//! ripple CLI.
//!   ripple parse <file>            M0: dump extracted symbols
//!   ripple index <path>            M1: build graph → .ripple/graph.redb
//!   ripple neighbors <symbol>      M1: traverse the persisted graph

mod verify;

use anyhow::{bail, Context, Result};
use ir::EdgeKind;
use serde_json::{json, Value};
use std::collections::HashSet;
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
        Some("impact") => cmd_impact(&args[1..]),
        Some("review") => cmd_review(&args[1..]),
        Some("risk") => cmd_risk(&args[1..]),
        Some("mcp") => cmd_mcp(&args[1..]),
        Some("eval") => cmd_eval(&args[1..]),
        Some("lsp") => cmd_lsp(&args[1..]),
        Some(other) => bail!("unknown command: {other}\n{USAGE}"),
        None => bail!("{USAGE}"),
    }
}

const USAGE: &str = "usage:\n  ripple parse <file> [--json]\n  ripple index <path>...\n  ripple neighbors <symbol> [--in|--out] [--depth N] [--root <path>] [--json]\n  ripple impact <symbol>... [--budget N] [--root <path>] [--json] [--verify lsp]\n  ripple review [<base>] [--budget N] [--root <path>] [--json] [--verify lsp]\n    --verify lsp [--verify-budget 2s] [--floor-contradicted|--drop-contradicted]  (upgrade call edges from a language server)\n  ripple risk <symbol|file> [--root <path>] [--json]\n  ripple mcp [--root <path>]   (MCP server over stdio for AI agents)\n  ripple lsp doctor [--root <path>] [--json]   (are language servers usable here?)";

fn db_path(root: &Path) -> PathBuf {
    root.join(".ripple").join("graph.redb")
}

/// Edge kinds surfaced by `neighbors` (call/import/co-change/cross-service).
const NEIGHBOR_KINDS: [EdgeKind; 5] = [
    EdgeKind::Calls,
    EdgeKind::Imports,
    EdgeKind::ChangesWith,
    EdgeKind::GraphqlCall,
    EdgeKind::DbQuery,
];

/// Flags that consume the following token as their value.
const VALUE_FLAGS: &[&str] = &[
    "--root",
    "--depth",
    "--budget",
    "--ignore",
    "--commits",
    "--oracle",
    "--sample",
    "--verify",
    "--verify-budget",
];

/// Positional args, correctly skipping `--flag value` pairs (so `--root X` never
/// leaks `X` as a positional).
fn positionals(args: &[String]) -> Vec<&String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a.starts_with("--") {
            i += if VALUE_FLAGS.contains(&a.as_str()) {
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
    let roots: Vec<PathBuf> = {
        let r: Vec<PathBuf> = positionals(args).into_iter().map(PathBuf::from).collect();
        if r.is_empty() {
            vec![".".into()]
        } else {
            r
        }
    };
    println!("{}", index_project(&roots)?);
    Ok(())
}

/// Build the graph for `roots` (parse + git overlay + cross-service) and persist
/// it. Returns a one-line summary. Shared by `index` and the MCP `reindex` tool.
fn index_project(roots: &[PathBuf]) -> Result<String> {
    let mut store = RedbStore::open(db_path(&roots[0])); // db + cache live under root[0]

    let cached = store.read_extracts()?;
    let indexed = resolve::build_incremental(roots, &cached)?;
    let mut nodes = indexed.result.nodes;
    let mut edges = indexed.result.edges;

    // git overlay per root (best-effort), namespaced to match module paths
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
    let cochange_applied = overlay::apply(&git, &mut nodes, &mut edges);

    // cross-service: TS→resolver (GraphqlCall), resolver→context (Calls), fn→schema (DbQuery)
    let mut cross = resolve::link_cross_service(&indexed.files, &nodes);
    let (graphql, db, imported) = (cross.graphql, cross.db, cross.imported);
    edges.append(&mut cross.edges);

    // structural risk needs every edge, including the cross-service ones
    let with_dependents = overlay::score_structure(&mut nodes, &edges);

    store.write(&nodes, &edges)?;
    store.write_extracts(&indexed.files)?;
    store.write_roots(&indexed.roots)?;

    let s = indexed.stats;
    Ok(format!(
        "indexed {} files across {} root(s) ({} added, {} changed, {} unchanged, {} removed) → {} nodes, {} edges ({} co-change, {} graphql, {} db, {} imported, {} with dependents) ({})",
        indexed.result.files_indexed, indexed.roots.len(),
        s.added, s.changed, s.unchanged, s.removed,
        nodes.len(), edges.len(), cochange_applied, graphql, db, imported, with_dependents,
        db_path(&roots[0]).display()
    ))
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
fn cmd_lsp(args: &[String]) -> Result<()> {
    match positional(args).map(String::as_str) {
        Some("doctor") => cmd_lsp_doctor(&args[1..]),
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

    let store = RedbStore::open(db_path(&root));
    let graph = store.load().ok();
    // A cross-repo index spans several roots, and each root has its own language
    // mix — the Elixir server belongs to the repo that has mix.exs, not to the one
    // that happens to hold the database.
    let mut roots = store.read_roots().unwrap_or_default();
    if roots.is_empty() {
        roots.push((String::new(), root.clone()));
    }
    let adapters: Vec<String> = lang::registry().iter().map(|a| a.id().to_owned()).collect();
    let specs = lsp::load(&root)?;

    let mut checked = Vec::new();
    for (tag, path) in &roots {
        let indexed = indexed_languages(graph.as_ref(), tag);
        let reports: Vec<lsp::Report> = specs.iter().map(|spec| lsp::probe(spec, path)).collect();
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
            }))?
        );
        return Ok(());
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

    let mut store = RedbStore::open(db_path(&root));
    let mut graph = store.load()?;
    let seeds: Vec<_> = symbols
        .iter()
        .flat_map(|s| graph.find_by_name(s))
        .map(|n| n.id)
        .collect();
    if seeds.is_empty() {
        bail!("no symbols matched: {}", symbols.join(", "));
    }
    graph = verify_upgrade(&mut store, graph, &root, args, &seeds, json)?;

    let hits = query::impact(&graph, &seeds, budget);
    if json {
        let out: Vec<_> = hits
            .iter()
            .map(|h| {
                serde_json::json!({
                    "symbol": h.node.name, "module": h.node.module_path,
                    "score": h.score, "weight": h.weight, "depth": h.depth,
                    "via": format!("{:?}", h.via), "risk": h.node.risk.composite,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!(
            "blast radius of {} — {} hits (ranked):",
            symbols.join(", "),
            hits.len()
        );
        for h in &hits {
            println!(
                "  {:.2}  {:?}<{:.2}> {} ({})",
                h.score, h.via, h.weight, h.node.name, h.node.module_path
            );
        }
    }
    Ok(())
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
    let plan = verify::Plan {
        focus: verify::focus_files(&graph, seeds),
        roots: &roots,
        budget,
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
    if !outcome.changed() {
        return Ok(graph);
    }
    let nodes: Vec<ir::Node> = graph.nodes().cloned().collect();
    store
        .write(&nodes, &outcome.edges)
        .context("persisting verified edges")?;
    Ok(InMemoryGraph::from_parts(nodes, outcome.edges))
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

    let store = RedbStore::open(db_path(&root));
    let graph = store.load()?;
    let mut roots = store.read_roots()?;
    if roots.is_empty() {
        roots.push((String::new(), root.clone()));
    }
    let specs = lsp::load(&root)?;
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
            let cmp = compare_calls(&graph, &mut client, path, tag, &files);
            report_oracle(spec, server.as_deref(), &files, &cmp);
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
            let Some(sites) = found else {
                cmp.server_unknown += 1;
                continue;
            };
            let Some(target) = graph
                .nodes_in_file(module)
                .into_iter()
                .find(|n| n.name == name)
            else {
                cmp.ripple_unknown += 1;
                continue;
            };

            let ours: HashSet<(String, String)> = graph
                .in_edges(target.id)
                .iter()
                .filter(|e| e.kind == EdgeKind::Calls)
                .filter_map(|e| graph.get(e.src))
                .map(|n| (n.module_path.clone(), n.name.clone()))
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
                let caller = bare_name(&site.name).to_owned();
                // ripple drops X → X deliberately (a blast radius from a symbol to
                // itself says nothing), so counting it as a miss measures a
                // documented choice rather than a defect
                if module == target.module_path && caller == target.name {
                    cmp.self_edges += 1;
                    continue;
                }
                theirs.insert((module, caller));
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

fn report_oracle(spec: &lsp::ServerSpec, server: Option<&str>, files: &[String], cmp: &Comparison) {
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

fn cmd_eval(args: &[String]) -> Result<()> {
    if flag_value(args, "--oracle") == Some("lsp") {
        return cmd_eval_oracle(args);
    }
    let root: PathBuf = flag_value(args, "--root").map_or_else(|| ".".into(), PathBuf::from);
    let k: usize = flag_value(args, "--commits")
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);
    let store = RedbStore::open(db_path(&root));
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
        .map(|files| files.iter().map(|p| resolve::namespace(&tag, p)).collect())
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

fn cmd_mcp(args: &[String]) -> Result<()> {
    let root: PathBuf = flag_value(args, "--root").map_or_else(|| ".".into(), PathBuf::from);
    // index-if-missing so an agent can point at a repo that was never indexed
    if !db_path(&root).exists() {
        index_project(std::slice::from_ref(&root))?;
    }
    let mut graph = RedbStore::open(db_path(&root)).load()?;

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
            "tools/call" => mcp_call(&mut graph, &root, req.get("params")),
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

fn mcp_tools() -> Value {
    let obj = |props: Value, req: Value| json!({ "type": "object", "properties": props, "required": req });
    json!([
        {
            "name": "search",
            "description": "Find symbols/files by substring (use this first to get exact names/paths for other tools).",
            "inputSchema": obj(json!({
                "query": { "type": "string" },
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

fn mcp_text(v: Value) -> Value {
    json!({ "content": [{ "type": "text", "text": v.to_string() }] })
}

fn mcp_call(
    graph: &mut InMemoryGraph,
    root: &Path,
    params: Option<&Value>,
) -> Result<Value, String> {
    let params = params.ok_or("missing params")?;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or("missing tool name")?;
    let a = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let str_arg = |k: &str| a.get(k).and_then(Value::as_str).map(str::to_owned);
    let usize_arg = |k: &str, d: usize| a.get(k).and_then(Value::as_u64).map_or(d, |n| n as usize);

    match name {
        "reindex" => {
            let summary = index_project(std::slice::from_ref(&root.to_path_buf()))
                .map_err(|e| e.to_string())?;
            *graph = RedbStore::open(db_path(root))
                .load()
                .map_err(|e| e.to_string())?;
            Ok(mcp_text(json!({ "reindexed": summary })))
        }
        "search" => {
            let q = str_arg("query").ok_or("query required")?.to_lowercase();
            let limit = usize_arg("limit", 30);
            let mut hits: Vec<_> = graph
                .nodes()
                .filter(|n| {
                    n.name.to_lowercase().contains(&q)
                        || n.qualified_name.to_lowercase().contains(&q)
                        || n.module_path.to_lowercase().contains(&q)
                })
                .collect();
            hits.sort_by(|a, b| {
                (a.module_path.as_str(), a.name.as_str())
                    .cmp(&(b.module_path.as_str(), b.name.as_str()))
            });
            let total = hits.len();
            let out: Vec<_> = hits
                .iter()
                .take(limit)
                .map(|n| {
                    json!({
                        "symbol": n.name, "qualified": n.qualified_name, "module": n.module_path,
                        "kind": format!("{:?}", n.kind),
                    })
                })
                .collect();
            Ok(mcp_text(
                json!({ "shown": out.len(), "total": total, "results": out }),
            ))
        }
        "impact" => {
            let sym = str_arg("symbol").ok_or("symbol required")?;
            let seeds: Vec<_> = graph.find_by_name(&sym).into_iter().map(|n| n.id).collect();
            if seeds.is_empty() {
                return Ok(mcp_text(
                    json!({ "error": format!("no symbol '{sym}' — try `search`") }),
                ));
            }
            let hits = query::impact(graph, &seeds, usize_arg("budget", 20));
            let out: Vec<_> = hits
                .iter()
                .map(|h| {
                    json!({
                        "symbol": h.node.name, "module": h.node.module_path,
                        "score": h.score, "weight": h.weight, "depth": h.depth,
                        "via": format!("{:?}", h.via), "risk": h.node.risk.composite,
                    })
                })
                .collect();
            Ok(mcp_text(
                json!({ "seeds_matched": seeds.len(), "blast_radius": out }),
            ))
        }
        "review_focus" => {
            let changed = overlay::diff_lines(root, str_arg("base").as_deref());
            let r = query::review_focus(graph, &changed, usize_arg("budget", 15));
            Ok(mcp_text(json!({
                "focus": r.focus.iter().map(|f| json!({
                    "symbol": f.node.name, "module": f.node.module_path,
                    "priority": f.review_priority, "downstream": f.downstream, "reasons": f.reasons,
                })).collect::<Vec<_>>(),
                "missing_cochange": r.missing_cochange.iter().map(|n| &n.module_path).collect::<Vec<_>>(),
                "untested": r.untested.iter().map(|n| &n.name).collect::<Vec<_>>(),
            })))
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
            for start in graph.find_by_name(&sym) {
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
            let mut hits = graph.find_by_name(&t);
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
        "explain_edge" => {
            let from = str_arg("from").ok_or("from required")?;
            let to = str_arg("to").ok_or("to required")?;
            let to_ids: std::collections::HashSet<_> =
                graph.find_by_name(&to).into_iter().map(|n| n.id).collect();
            let mut out = Vec::new();
            for f in graph.find_by_name(&from) {
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

    let changed = overlay::diff_lines(&root, base.map(String::as_str));
    if changed.is_empty() {
        println!("no changes to review (vs {})", base.map_or("HEAD", |s| s));
        return Ok(());
    }
    let mut store = RedbStore::open(db_path(&root));
    let mut graph = store.load()?;
    let seeds: Vec<ir::SymbolId> = changed
        .keys()
        .flat_map(|f| graph.nodes_in_file(f))
        .map(|n| n.id)
        .collect();
    graph = verify_upgrade(&mut store, graph, &root, args, &seeds, json)?;
    let r = query::review_focus(&graph, &changed, budget);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "focus": r.focus.iter().map(|f| serde_json::json!({
                    "symbol": f.node.name, "module": f.node.module_path,
                    "priority": f.review_priority, "downstream": f.downstream,
                    "reasons": f.reasons,
                })).collect::<Vec<_>>(),
                "missing_cochange": r.missing_cochange.iter().map(|n| &n.module_path).collect::<Vec<_>>(),
                "untested": r.untested.iter().map(|n| &n.name).collect::<Vec<_>>(),
            }))?
        );
    } else {
        println!(
            "review focus ({} changed symbols), highest priority first:",
            r.focus.len()
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

    let graph = RedbStore::open(db_path(&root)).load()?;
    // match by symbol name/qualified name, or by module path (file)
    let mut hits: Vec<_> = graph
        .find_by_name(&query)
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
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        for (name, module, r) in &hits {
            println!(
                "{name} ({module})\n  composite {:.2} | churn {:.2} bug {:.2} ownership {:.2} fanout {:.2}",
                r.composite, r.churn, r.bug_density, r.ownership, r.fanout
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

    let store = RedbStore::open(db_path(&root));
    let graph = store.load()?;

    let matches = graph.find_by_name(&symbol);
    if matches.is_empty() {
        bail!("symbol not found: {symbol}");
    }

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
            for h in &hops {
                let indent = "  ".repeat(h.depth);
                println!(
                    "{indent}{:?}<{:.2}> {} ({})",
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
        let mut store = RedbStore::open(db_path(&web));

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
}
