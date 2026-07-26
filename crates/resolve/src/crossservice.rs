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
/// TS operation ↔ the *context module* a `dataloader(Mod)` field is served by. Lower
/// than a named resolver because it is one level coarser: the field is served by that
/// module, but which function serves it is not written anywhere.
const CONF_GRAPHQL_CONTEXT: f32 = 0.5;
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
    /// edges whose caller is a file or module rather than a function, because the
    /// call sits outside every definition (a module body, a `test` block, a script)
    pub file_granular: usize,
}

/// Link cross-service edges from the per-file facts already on each `CachedFile`.
pub fn link_cross_service(files: &[CachedFile], nodes: &[Node]) -> CrossEdges {
    // ── maps derived from the built graph ──
    let mut fqn_to_module: HashMap<&str, &str> = HashMap::new();
    let mut fqn_to_class: HashMap<&str, SymbolId> = HashMap::new();
    let mut fqns_in_file: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut fn_by_loc: HashMap<(&str, &str), SymbolId> = HashMap::new();
    // (start, end, id, is the container a function or the whole module body?)
    let mut fn_spans: HashMap<&str, Vec<(u32, u32, SymbolId, Granularity)>> = HashMap::new();
    for n in nodes {
        match n.kind {
            NodeKind::Class => {
                fqn_to_module.insert(n.qualified_name.as_str(), n.module_path.as_str());
                fqn_to_class.insert(n.qualified_name.as_str(), n.id);
                fqns_in_file
                    .entry(n.module_path.as_str())
                    .or_default()
                    .push(n.qualified_name.as_str());
                // widest container: a call in a module body belongs to the module.
                // `enclosing` takes the smallest containing span, so this is only
                // reached when no function contains the call.
                let spans = fn_spans.entry(n.module_path.as_str()).or_default();
                for s in n.definition_spans() {
                    spans.push((s.start_line, s.end_line, n.id, Granularity::File));
                }
            }
            NodeKind::Function | NodeKind::Method => {
                fn_by_loc.insert((n.module_path.as_str(), n.name.as_str()), n.id);
                // every definition site, not just the first: one symbol can be
                // written as several clauses or overloads, and a call in the second
                // one belongs to it just as much
                let spans = fn_spans.entry(n.module_path.as_str()).or_default();
                for s in n.definition_spans() {
                    spans.push((s.start_line, s.end_line, n.id, Granularity::Function));
                }
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
    let mut op_fields: HashMap<String, Vec<(&str, &[String])>> = HashMap::new();
    for f in files {
        for op in &f.extract.cross.gql_ops {
            op_fields
                .entry(document_key(&op.name))
                .or_default()
                .push((op.scope.as_str(), op.path.as_slice()));
        }
    }

    // fragment name → its definition, so a spread can be expanded at link time
    let mut fragments: HashMap<&str, &lang::cross::GqlFragment> = HashMap::new();
    for f in files {
        for fragment in &f.extract.cross.gql_fragments {
            fragments.insert(fragment.name.as_str(), fragment);
        }
    }
    // operation → the fragments it spreads. Most nested selections in a codegen app are
    // written in fragments, so without this the type-graph walk has almost nothing to
    // walk (363 spreads across 24 definitions on one real app).
    let mut op_spreads: HashMap<String, Vec<&lang::cross::GqlSpread>> = HashMap::new();
    for f in files {
        for spread in &f.extract.cross.gql_spreads {
            op_spreads
                .entry(document_key(&spread.op))
                .or_default()
                .push(spread);
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
    // (scope, field) → the scope that field's children are declared in. This is the
    // schema's type graph, and it is what lets a nested selection be followed:
    // `lfgPosts` returns `:lfg_post`, so `lfgPosts { author }` asks object:lfg_post
    // for `author` (issue #22).
    let mut field_type: HashMap<(&str, &str), String> = HashMap::new();
    // fields whose resolver names a module, not a function: the edge is file-granular
    let mut context_producer: HashMap<(&str, &str), Vec<SymbolId>> = HashMap::new();
    for f in files {
        let Some(ex) = &f.extract.cross.elixir else {
            continue;
        };
        for field in &ex.fields {
            // a field is reachable under its own scope *and* under every root scope
            // that pulls that scope in via `import_fields`
            let mut scopes: Vec<&str> = vec![field.scope.as_str()];
            if let Some(roots) = roots_by_scope.get(field.scope.as_str()) {
                scopes.extend(roots.iter().copied());
            }
            if let Some(returns) = &field.returns {
                for scope in &scopes {
                    // the same spelling the extractor uses for a type block's scope
                    field_type.insert((scope, field.field.as_str()), format!("object:{returns}"));
                }
            }
            // `field.module` is already resolved (alias→FQN) at extraction time
            let Some(&file) = fqn_to_module.get(field.module.as_str()) else {
                continue;
            };
            let Some(&id) = fn_by_loc.get(&(file, field.func.as_str())) else {
                continue;
            };
            for scope in scopes {
                producer
                    .entry((scope, field.field.as_str()))
                    .or_default()
                    .push(id);
            }
        }
        // context-module fields: no function is named, so the module node is the target
        for field in &ex.context_fields {
            let mut scopes: Vec<&str> = vec![field.scope.as_str()];
            if let Some(roots) = roots_by_scope.get(field.scope.as_str()) {
                scopes.extend(roots.iter().copied());
            }
            if let Some(returns) = &field.returns {
                for scope in &scopes {
                    field_type.insert((scope, field.field.as_str()), format!("object:{returns}"));
                }
            }
            let Some(&file) = fqn_to_module.get(field.module.as_str()) else {
                continue;
            };
            let id = SymbolId::module(file);
            for scope in scopes {
                context_producer
                    .entry((scope, field.field.as_str()))
                    .or_default()
                    .push(id);
            }
        }
    }
    for ids in context_producer.values_mut() {
        ids.sort_unstable();
        ids.dedup();
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
            let Some(fields) = op_fields.get(&document_key(op)) else {
                continue;
            };
            for (scope, path) in fields {
                if let Some(resolvers) = resolvers_for(&producer, &field_type, scope, path) {
                    // N candidates for one field → 1/N each (docs/06-risk-and-queries.md)
                    let conf = CONF_GRAPHQL / resolvers.len() as f32;
                    for &resolver in resolvers {
                        if emit(&mut edges, src_id, resolver, EdgeKind::GraphqlCall, conf, 0) {
                            graphql += 1;
                        }
                    }
                }
                // a dataloader field names a context, not a function — coarser, and
                // priced accordingly (unchanged below)
                if let Some(contexts) = resolvers_for(&context_producer, &field_type, scope, path) {
                    let conf = CONF_GRAPHQL_CONTEXT / contexts.len() as f32;
                    for &context in contexts {
                        if emit(&mut edges, src_id, context, EdgeKind::GraphqlCall, conf, 0) {
                            graphql += 1;
                        }
                    }
                }
            }
            // `...LfgPostFields` — the fragment's own type condition names the scope its
            // fields live in, so an expanded spread needs no descent
            for spread in op_spreads.get(&document_key(op)).into_iter().flatten() {
                expand_spread(
                    spread.fragment.as_str(),
                    &fragments,
                    &mut HashSet::new(),
                    MAX_FRAGMENT_HOPS,
                    &mut |scope: &str, path: &[String]| {
                        let mut hits = Vec::new();
                        if let Some(rs) = resolvers_for(&producer, &field_type, scope, path) {
                            hits.push((rs, CONF_GRAPHQL));
                        }
                        if let Some(cs) = resolvers_for(&context_producer, &field_type, scope, path)
                        {
                            hits.push((cs, CONF_GRAPHQL_CONTEXT));
                        }
                        for (targets, base) in hits {
                            let conf = base / targets.len() as f32;
                            for &t in targets {
                                if emit(&mut edges, src_id, t, EdgeKind::GraphqlCall, conf, 0) {
                                    graphql += 1;
                                }
                            }
                        }
                    },
                );
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
    let mut file_granular = 0;
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
            let (caller, grain) = caller_at(spans, &f.module_path, r.site.start_line);
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
                    file_granular += usize::from(grain == Granularity::File);
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
            let Some(&file) = fqn_to_module.get(fqn.as_str()) else {
                continue;
            };
            let (caller, grain) = caller_at(spans, &f.module_path, *line);
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
                    file_granular += usize::from(grain == Granularity::File);
                }
            }
        }
        for (fqn, line) in &ex.schema_refs {
            if !schema_fqns.contains(fqn.as_str()) {
                continue;
            }
            let Some(&target) = fqn_to_class.get(fqn.as_str()) else {
                continue;
            };
            let (caller, grain) = caller_at(spans, &f.module_path, *line);
            if emit(
                &mut edges,
                caller,
                target,
                EdgeKind::DbQuery,
                CONF_DB_QUERY,
                *line,
            ) {
                db += 1;
                file_granular += usize::from(grain == Granularity::File);
            }
        }
    }

    CrossEdges {
        edges,
        graphql,
        elixir_calls,
        db,
        file_granular,
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

/// Join key for a GraphQL operation, from either side of the codegen boundary.
///
/// A document may name its operation `updateLfgRequest`; codegen emits
/// `UpdateLfgRequestDocument`, which is what the TypeScript side references. Keying on
/// the raw name silently lost every edge from such an operation — 11 of 242 operations
/// on one real frontend are written lowercase-first.
fn document_key(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// How deep a chain of fragments spreading fragments is followed. They nest (a page
/// fragment composing card fragments), but not deeply, and a cycle must cost nothing.
const MAX_FRAGMENT_HOPS: usize = 4;

/// Visit every field path a fragment selects, rooted at the scope its type condition
/// names, following fragments it spreads in turn.
fn expand_spread(
    name: &str,
    fragments: &HashMap<&str, &lang::cross::GqlFragment>,
    seen: &mut HashSet<String>,
    hops: usize,
    visit: &mut impl FnMut(&str, &[String]),
) {
    if hops == 0 || !seen.insert(name.to_owned()) {
        return;
    }
    let Some(fragment) = fragments.get(name) else {
        return; // spread of a fragment defined nowhere we indexed
    };
    let scope = format!(
        "object:{}",
        lang::cross::decamelize(&fragment.type_condition)
    );
    for path in &fragment.paths {
        visit(&scope, path);
    }
    for (_, nested) in &fragment.spreads {
        expand_spread(nested, fragments, seen, hops - 1, visit);
    }
}

/// The resolvers behind one selection path, walked down the schema's type graph.
///
/// `["lfgPosts", "author"]` starts at the root scope, resolves `lfgPosts` there to get
/// its type, then asks that type's scope for `author`. A path whose intermediate type
/// is unknown resolves to nothing rather than falling back to the root field — a
/// wrong edge is worse than a missing one.
fn resolvers_for<'a>(
    producer: &'a HashMap<(&str, &str), Vec<SymbolId>>,
    field_type: &'a HashMap<(&str, &str), String>,
    root_scope: &'a str,
    path: &'a [String],
) -> Option<&'a Vec<SymbolId>> {
    let (last, parents) = path.split_last()?;
    // borrow the map's own strings rather than cloning: the returned resolvers borrow
    // from `producer`, so the key must outlive the walk
    let mut scope: &str = root_scope;
    for parent in parents {
        scope = field_type.get(&(scope, parent.as_str()))?.as_str();
    }
    producer.get(&(scope, last.as_str()))
}

/// Innermost definition span containing `line` (smallest range), so a call inside
/// a nested def is attributed to that def, not an enclosing one.
fn enclosing(
    spans: &[(u32, u32, SymbolId, Granularity)],
    line: u32,
) -> Option<(SymbolId, Granularity)> {
    spans
        .iter()
        .filter(|(s, e, ..)| line >= *s && line <= *e)
        .min_by_key(|(s, e, ..)| e - s)
        .map(|&(_, _, id, grain)| (id, grain))
}

/// Who to credit a cross-service call to, and whether that answer is file-granular.
///
/// A real call frequently sits inside no function at all: an ExUnit `test` block, a
/// Phoenix `plug` line, a module body, a `.exs` script. Cross-service linking used
/// to drop those, while same-file resolution attributed them to the enclosing
/// definition or the file — so the two paths disagreed about the same construct and
/// the coarser (but true) edges existed only for one of them. This makes both fall
/// back the same way: enclosing definition, else the module body, else the file.
fn caller_at(
    spans: &[(u32, u32, SymbolId, Granularity)],
    module_path: &str,
    line: u32,
) -> (SymbolId, Granularity) {
    enclosing(spans, line).unwrap_or((SymbolId::module(module_path), Granularity::File))
}

/// Whether an attributed caller names a function or only the file it lives in.
/// Tracked so the count of coarser edges is reported rather than blended in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Granularity {
    Function,
    File,
}
