//! Cross-service linking: matches the tree-sitter-extracted facts on each file
//! (`CachedFile.extract.cross`, produced during the single index parse) into
//! edges — TS module → resolver (`GraphqlCall`), resolver → context (`Calls`),
//! function → Ecto schema (`DbQuery`). No parsing here; extraction lives in
//! `lang::cross`. See docs/10-cross-service-resolution.md.

use ir::{Edge, EdgeKind, EdgeSource, Node, NodeKind, Span, SymbolId};
use parse::CachedFile;
use std::collections::{HashMap, HashSet};

// cross-service edge confidences (see docs/06-risk-and-queries.md)
const CONF_GRAPHQL: f32 = 0.9; // TS operation ↔ Absinthe resolver, name-matched
const CONF_ELIXIR_CALL: f32 = 0.9; // resolved remote call (alias → module → fn)
const CONF_DB_QUERY: f32 = 0.85; // function → Ecto schema reference
const CONF_IMPORTED_CALL: f32 = 0.9; // bare call resolved through an explicit `import`

pub struct CrossEdges {
    pub edges: Vec<Edge>,
    pub graphql: usize,
    pub elixir_calls: usize,
    pub db: usize,
    /// bare calls resolved through an `import`
    pub imported: usize,
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
                fqns_in_file
                    .entry(n.module_path.as_str())
                    .or_default()
                    .push(n.qualified_name.as_str());
            }
            NodeKind::Function | NodeKind::Method => {
                fn_by_loc.insert((n.module_path.as_str(), n.name.as_str()), n.id);
                fn_spans.entry(n.module_path.as_str()).or_default().push((
                    n.span.start_line,
                    n.span.end_line,
                    n.id,
                ));
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

    // GraphQL operation name → the (scope, root field) pairs it selects,
    // aggregated across all .gql files
    let mut op_fields: HashMap<&str, Vec<(&str, &str)>> = HashMap::new();
    for f in files {
        for op in &f.extract.cross.gql_ops {
            op_fields
                .entry(op.name.as_str())
                .or_default()
                .push((op.scope.as_str(), op.field.as_str()));
        }
    }

    // scope → scopes whose fields it includes (Absinthe `import_fields`)
    let mut includes: HashMap<&str, Vec<&str>> = HashMap::new();
    for f in files {
        let Some(ex) = &f.extract.cross.elixir else {
            continue;
        };
        for (scope, included) in &ex.scope_includes {
            includes
                .entry(scope.as_str())
                .or_default()
                .push(included.as_str());
        }
    }

    // producer: (root scope, field) → resolver functions. A field reached from a
    // root scope through `import_fields` is a root field too, so each declared
    // scope is expanded to every root scope that pulls it in. More than one
    // candidate means the match is ambiguous — all are kept and the confidence
    // is split, never collapsed to a single fabricated edge.
    let roots_by_scope = roots_by_scope(&includes);
    let mut producer: HashMap<(&str, &str), Vec<SymbolId>> = HashMap::new();
    for f in files {
        let Some(ex) = &f.extract.cross.elixir else {
            continue;
        };
        for field in &ex.fields {
            // `field.module` is already resolved (alias→FQN) at extraction time
            let Some(&file) = fqn_to_module.get(field.module.as_str()) else {
                continue;
            };
            let Some(&id) = fn_by_loc.get(&(file, field.func.as_str())) else {
                continue;
            };
            let Some(roots) = roots_by_scope.get(field.scope.as_str()) else {
                continue;
            };
            for root in roots {
                producer
                    .entry((*root, field.field.as_str()))
                    .or_default()
                    .push(id);
            }
        }
    }
    for ids in producer.values_mut() {
        ids.sort_unstable();
        ids.dedup();
    }

    // ── build edges (deduped by src/dst/kind) ──
    let mut edges = Vec::new();
    let mut seen: HashSet<(SymbolId, SymbolId, u8)> = HashSet::new();
    let mut emit = |edges: &mut Vec<Edge>,
                    src: SymbolId,
                    dst: SymbolId,
                    kind: EdgeKind,
                    conf: f32,
                    line: u32| {
        if src != dst && seen.insert((src, dst, kind as u8)) {
            edges.push(Edge {
                src,
                dst,
                kind,
                confidence: conf,
                site: Span {
                    start_line: line,
                    start_col: 1,
                    end_line: line,
                    end_col: 1,
                },
                source: EdgeSource::Extracted,
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
            let Some(fields) = op_fields.get(op.as_str()) else {
                continue;
            };
            for key in fields {
                let Some(resolvers) = producer.get(key) else {
                    continue;
                };
                // N candidates for one field → 1/N each (docs/06-risk-and-queries.md)
                let conf = CONF_GRAPHQL / resolvers.len() as f32;
                for &resolver in resolvers {
                    if emit(&mut edges, src_id, resolver, EdgeKind::GraphqlCall, conf, 0) {
                        graphql += 1;
                    }
                }
            }
        }
    }

    // Bare calls that cross a module boundary through an explicit `import`.
    //
    // Unqualified call resolution is same-file by nature — a bare name normally
    // can't reach another module. Elixir's `import Mod` breaks that, and test,
    // fixture and Phoenix code lean on it, so without this the callers of those
    // functions are simply absent (measured with `eval --oracle lsp`).
    let mut names_in_file: HashMap<&str, HashSet<&str>> = HashMap::new();
    for n in nodes {
        if matches!(n.kind, NodeKind::Function | NodeKind::Method) {
            names_in_file
                .entry(n.module_path.as_str())
                .or_default()
                .insert(n.name.as_str());
        }
    }
    let mut imported = 0;
    for f in files {
        let Some(ex) = &f.extract.cross.elixir else {
            continue;
        };
        if ex.imports.is_empty() {
            continue;
        }
        let empty_names = HashSet::new();
        let local_names = names_in_file
            .get(f.module_path.as_str())
            .unwrap_or(&empty_names);
        let empty = Vec::new();
        let spans = fn_spans.get(f.module_path.as_str()).unwrap_or(&empty);

        for r in &f.extract.refs {
            if r.kind != parse::RefKind::Call || local_names.contains(r.name.as_str()) {
                continue; // a local definition wins; that edge already exists
            }
            let mut targets: Vec<SymbolId> = ex
                .imports
                .iter()
                .filter_map(|fqn| fqn_to_module.get(fqn.as_str()))
                .filter_map(|file| fn_by_loc.get(&(*file, r.name.as_str())).copied())
                .collect();
            targets.sort_unstable();
            targets.dedup();
            let Some(caller) = enclosing(spans, r.site.start_line) else {
                continue;
            };
            let conf = CONF_IMPORTED_CALL / targets.len().max(1) as f32;
            for t in targets {
                if emit(
                    &mut edges,
                    caller,
                    t,
                    EdgeKind::Calls,
                    conf,
                    r.site.start_line,
                ) {
                    imported += 1;
                }
            }
        }
    }

    // Calls (resolver → context) + DbQuery (function → schema)
    let mut elixir_calls = 0;
    let mut db = 0;
    for f in files {
        let Some(ex) = &f.extract.cross.elixir else {
            continue;
        };
        let empty = Vec::new();
        let spans = fn_spans.get(f.module_path.as_str()).unwrap_or(&empty);

        for (fqn, func, line) in &ex.remote_calls {
            let (Some(&file), Some(caller)) =
                (fqn_to_module.get(fqn.as_str()), enclosing(spans, *line))
            else {
                continue;
            };
            if let Some(&target) = fn_by_loc.get(&(file, func.as_str())) {
                if emit(
                    &mut edges,
                    caller,
                    target,
                    EdgeKind::Calls,
                    CONF_ELIXIR_CALL,
                    *line,
                ) {
                    elixir_calls += 1;
                }
            }
        }
        for (fqn, line) in &ex.schema_refs {
            if !schema_fqns.contains(fqn.as_str()) {
                continue;
            }
            let (Some(&target), Some(caller)) =
                (fqn_to_class.get(fqn.as_str()), enclosing(spans, *line))
            else {
                continue;
            };
            if emit(
                &mut edges,
                caller,
                target,
                EdgeKind::DbQuery,
                CONF_DB_QUERY,
                *line,
            ) {
                db += 1;
            }
        }
    }

    CrossEdges {
        edges,
        graphql,
        elixir_calls,
        db,
        imported,
    }
}

/// For every declared scope, the root scopes whose fields it contributes to —
/// itself if it *is* a root, plus any root that includes it (transitively) via
/// `import_fields`. Scopes no root reaches are absent: their fields are
/// type-level, not selectable as a document's root field.
fn roots_by_scope<'a>(includes: &HashMap<&'a str, Vec<&'a str>>) -> HashMap<&'a str, Vec<&'a str>> {
    let mut out: HashMap<&'a str, Vec<&'a str>> = HashMap::new();
    for root in lang::cross::GQL_ROOT_SCOPES {
        let mut seen: HashSet<&'a str> = HashSet::new();
        let mut queue = vec![root];
        while let Some(scope) = queue.pop() {
            if !seen.insert(scope) {
                continue;
            }
            out.entry(scope).or_default().push(root);
            if let Some(next) = includes.get(scope) {
                queue.extend(next);
            }
        }
    }
    for roots in out.values_mut() {
        roots.sort_unstable();
    }
    out
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
