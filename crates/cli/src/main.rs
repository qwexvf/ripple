//! ripple CLI.
//!   ripple parse <file>            M0: dump extracted symbols
//!   ripple index <path>            M1: build graph → .ripple/graph.redb
//!   ripple neighbors <symbol>      M1: traverse the persisted graph

use anyhow::{bail, Context, Result};
use ir::EdgeKind;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use store::{Dir, GraphStore, InMemoryGraph, RedbStore};

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
        Some(other) => bail!("unknown command: {other}\n{USAGE}"),
        None => bail!("{USAGE}"),
    }
}

const USAGE: &str = "usage:\n  ripple parse <file> [--json]\n  ripple index <path>...\n  ripple neighbors <symbol> [--in|--out] [--depth N] [--root <path>] [--json]\n  ripple impact <symbol>... [--budget N] [--root <path>] [--json]\n  ripple review [<base>] [--budget N] [--root <path>] [--json]\n  ripple risk <symbol|file> [--root <path>] [--json]\n  ripple mcp [--root <path>]   (MCP server over stdio for AI agents)";

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
const VALUE_FLAGS: &[&str] = &["--root", "--depth", "--budget", "--ignore", "--commits"];

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
    let (graphql, db) = (cross.graphql, cross.db);
    edges.append(&mut cross.edges);

    store.write(&nodes, &edges)?;
    store.write_extracts(&indexed.files)?;

    let s = indexed.stats;
    Ok(format!(
        "indexed {} files across {} root(s) ({} added, {} changed, {} unchanged, {} removed) → {} nodes, {} edges ({} co-change, {} graphql, {} db) ({})",
        indexed.result.files_indexed, indexed.roots.len(),
        s.added, s.changed, s.unchanged, s.removed,
        nodes.len(), edges.len(), cochange_applied, graphql, db,
        db_path(&roots[0]).display()
    ))
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

    let graph = RedbStore::open(db_path(&root)).load()?;
    let seeds: Vec<_> = symbols
        .iter()
        .flat_map(|s| graph.find_by_name(s))
        .map(|n| n.id)
        .collect();
    if seeds.is_empty() {
        bail!("no symbols matched: {}", symbols.join(", "));
    }

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

/// Historical validation: over recent commits, how many same-commit file pairs
/// does the graph link? Static edges are leakage-free (independent of commit
/// history); the static-vs-co-change gap shows why co-change is needed.
fn cmd_eval(args: &[String]) -> Result<()> {
    let root: PathBuf = flag_value(args, "--root").map_or_else(|| ".".into(), PathBuf::from);
    let k: usize = flag_value(args, "--commits")
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);
    let graph = RedbStore::open(db_path(&root)).load()?;

    let indexed = |p: &str| graph.get(ir::SymbolId::module(p)).is_some();
    let static_kinds = [
        EdgeKind::Calls,
        EdgeKind::Imports,
        EdgeKind::GraphqlCall,
        EdgeKind::DbQuery,
    ];
    let cochange = |a: &str, b: &str| {
        let (ma, mb) = (ir::SymbolId::module(a), ir::SymbolId::module(b));
        graph
            .out_edges(ma)
            .iter()
            .any(|e| e.kind == EdgeKind::ChangesWith && e.dst == mb)
    };
    // cache each test file's statically-reachable file set (from all its symbols)
    let commits = overlay::recent_commit_files(&root, k);
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
                let c = cochange(a, b) || cochange(b, a);
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
    println!(
        "historical co-change prediction over recent commits ({pairs} same-commit file pairs):"
    );
    println!(
        "  static edges alone : {:.1}%  ({stat})   ← leakage-free baseline",
        pct(stat)
    );
    println!("  co-change alone    : {:.1}%  ({co})", pct(co));
    println!("  fused (either)     : {:.1}%  ({either})", pct(either));
    println!(
        "  → co-change lifts recall by {:.1} pts over static-only",
        pct(either) - pct(stat)
    );
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
            "description": "Why are two symbols connected? Returns the edge kind, confidence, and site between them.",
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
    let graph = RedbStore::open(db_path(&root)).load()?;
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
                "{name} ({module})\n  composite {:.2} | churn {:.2} bug {:.2} ownership {:.2}",
                r.composite, r.churn, r.bug_density, r.ownership
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
}
