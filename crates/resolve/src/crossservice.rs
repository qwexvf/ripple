//! Cross-service linking: matches the tree-sitter-extracted facts on each file
//! (`CachedFile.extract.cross`, produced during the single index parse) into
//! edges — TS module → resolver (`GraphqlCall`), resolver → context (`Calls`),
//! function → Ecto schema (`DbQuery`). No parsing here; extraction lives in
//! `lang::cross`. See docs/10-cross-service-resolution.md.

use ir::{Edge, EdgeKind, Node, NodeKind, Span, SymbolId};
use parse::CachedFile;
use std::collections::{HashMap, HashSet};

// cross-service edge confidences (see docs/06-risk-and-queries.md)
const CONF_GRAPHQL: f32 = 0.9; // TS operation ↔ Absinthe resolver, name-matched
const CONF_ELIXIR_CALL: f32 = 0.9; // resolved remote call (alias → module → fn)
const CONF_DB_QUERY: f32 = 0.85; // function → Ecto schema reference

pub struct CrossEdges {
    pub edges: Vec<Edge>,
    pub graphql: usize,
    pub elixir_calls: usize,
    pub db: usize,
}

/// Link cross-service edges from the per-file facts already on each `CachedFile`.
pub fn link_cross_service(files: &[CachedFile], nodes: &[Node]) -> CrossEdges {
    // ── maps derived from the built graph ──
    let mut fqn_to_module: HashMap<&str, &str> = HashMap::new();
    let mut fqn_to_class: HashMap<&str, SymbolId> = HashMap::new();
    let mut fqns_in_file: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut fn_by_loc: HashMap<(&str, &str), SymbolId> = HashMap::new();
    let mut fn_spans: HashMap<&str, Vec<(u32, u32, SymbolId)>> = HashMap::new();
    for n in nodes {
        match n.kind {
            NodeKind::Class => {
                fqn_to_module.insert(n.qualified_name.as_str(), n.module_path.as_str());
                fqn_to_class.insert(n.qualified_name.as_str(), n.id);
                fqns_in_file.entry(n.module_path.as_str()).or_default().push(n.qualified_name.as_str());
            }
            NodeKind::Function | NodeKind::Method => {
                fn_by_loc.insert((n.module_path.as_str(), n.name.as_str()), n.id);
                fn_spans
                    .entry(n.module_path.as_str())
                    .or_default()
                    .push((n.span.start_line, n.span.end_line, n.id));
            }
            _ => {}
        }
    }

    // schema module FQNs (files that declared `schema "table"`)
    let mut schema_fqns: HashSet<&str> = HashSet::new();
    for f in files {
        if f.extract.cross.elixir.as_ref().is_some_and(|e| e.is_schema) {
            if let Some(fqns) = fqns_in_file.get(f.module_path.as_str()) {
                schema_fqns.extend(fqns);
            }
        }
    }

    // GraphQL operation → root fields (aggregated across all .gql files)
    let mut op_fields: HashMap<&str, Vec<&str>> = HashMap::new();
    for f in files {
        for (op, field) in &f.extract.cross.gql_ops {
            op_fields.entry(op.as_str()).or_default().push(field.as_str());
        }
    }

    // producer: root field (camelCase) → resolver function symbol
    let mut producer: HashMap<&str, SymbolId> = HashMap::new();
    for f in files {
        let Some(ex) = &f.extract.cross.elixir else { continue };
        for (field, fqn, func) in &ex.fields {
            // `fqn` is already resolved (alias→FQN) at extraction time
            if let Some(&file) = fqn_to_module.get(fqn.as_str()) {
                if let Some(&id) = fn_by_loc.get(&(file, func.as_str())) {
                    producer.insert(field.as_str(), id);
                }
            }
        }
    }

    // ── build edges (deduped by src/dst/kind) ──
    let mut edges = Vec::new();
    let mut seen: HashSet<(SymbolId, SymbolId, u8)> = HashSet::new();
    let mut emit = |edges: &mut Vec<Edge>, src: SymbolId, dst: SymbolId, kind: EdgeKind, conf: f32, line: u32| {
        if src != dst && seen.insert((src, dst, kind as u8)) {
            edges.push(Edge {
                src,
                dst,
                kind,
                confidence: conf,
                site: Span { start_line: line, start_col: 1, end_line: line, end_col: 1 },
            });
            true
        } else {
            false
        }
    };

    // GraphqlCall: consumer TS module → resolver
    let mut graphql = 0;
    for f in files {
        if f.extract.cross.ts_docs.is_empty() {
            continue;
        }
        let src_id = SymbolId::module(&f.module_path);
        for op in &f.extract.cross.ts_docs {
            let Some(fields) = op_fields.get(op.as_str()) else { continue };
            for field in fields {
                if let Some(&resolver) = producer.get(field) {
                    if emit(&mut edges, src_id, resolver, EdgeKind::GraphqlCall, CONF_GRAPHQL, 0) {
                        graphql += 1;
                    }
                }
            }
        }
    }

    // Calls (resolver → context) + DbQuery (function → schema)
    let mut elixir_calls = 0;
    let mut db = 0;
    for f in files {
        let Some(ex) = &f.extract.cross.elixir else { continue };
        let empty = Vec::new();
        let spans = fn_spans.get(f.module_path.as_str()).unwrap_or(&empty);

        for (fqn, func, line) in &ex.remote_calls {
            let (Some(&file), Some(caller)) = (fqn_to_module.get(fqn.as_str()), enclosing(spans, *line))
            else {
                continue;
            };
            if let Some(&target) = fn_by_loc.get(&(file, func.as_str())) {
                if emit(&mut edges, caller, target, EdgeKind::Calls, CONF_ELIXIR_CALL, *line) {
                    elixir_calls += 1;
                }
            }
        }
        for (fqn, line) in &ex.schema_refs {
            if !schema_fqns.contains(fqn.as_str()) {
                continue;
            }
            let (Some(&target), Some(caller)) = (fqn_to_class.get(fqn.as_str()), enclosing(spans, *line))
            else {
                continue;
            };
            if emit(&mut edges, caller, target, EdgeKind::DbQuery, CONF_DB_QUERY, *line) {
                db += 1;
            }
        }
    }

    CrossEdges { edges, graphql, elixir_calls, db }
}

/// Innermost function span containing `line` (smallest range), so a call inside
/// a nested def is attributed to that def, not an enclosing one.
fn enclosing(spans: &[(u32, u32, SymbolId)], line: u32) -> Option<SymbolId> {
    spans
        .iter()
        .filter(|(s, e, _)| line >= *s && line <= *e)
        .min_by_key(|(s, e, _)| e - s)
        .map(|&(.., id)| id)
}

