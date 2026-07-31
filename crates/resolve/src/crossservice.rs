//! Cross-service linking: matches the tree-sitter-extracted facts on each file
//! (`CachedFile.extract.cross`, produced during the single index parse) into
//! edges — TS module → resolver (`GraphqlCall`), resolver → context (`Calls`),
//! function → Ecto schema (`DbQuery`). No parsing here; extraction lives in
//! `lang::cross`. See docs/10-cross-service-resolution.md.

use crate::routes::RouteIndex;
use ir::{Edge, EdgeKind, EdgeSource, Node, NodeKind, Span, SymbolId};
use lang::cross;
use parse::CachedFile;
use std::collections::{HashMap, HashSet};

// cross-service edge confidences (see docs/06-risk-and-queries.md)
const CONF_GRAPHQL: f32 = 0.9; // TS operation ↔ Absinthe resolver, name-matched
/// TS operation ↔ the *context module* a `dataloader(Mod)` field is served by. Lower
/// than a named resolver because it is one level coarser: the field is served by that
/// module, but which function serves it is not written anywhere.
const CONF_GRAPHQL_CONTEXT: f32 = 0.5;
/// A call that names its target module, resolved through that module's FQN.
const CONF_QUALIFIED_CALL: f32 = 0.9;
const CONF_DB_QUERY: f32 = 0.85; // function → Ecto schema reference
const CONF_IMPORTED_CALL: f32 = 0.9; // bare call resolved through an explicit `import`

pub struct CrossEdges {
    pub edges: Vec<Edge>,
    pub graphql: usize,
    /// calls resolved through an explicit module FQN
    pub qualified_calls: usize,
    pub db: usize,
    /// bare calls resolved through an `import`
    pub imported: usize,
    /// edges whose caller is a file or module rather than a function, because the
    /// call sits outside every definition (a module body, a `test` block, a script)
    pub file_granular: usize,
    /// selections a consumer asked for that no producer answered. A convention
    /// nobody has taught the detector shows up here as a number; before there was
    /// a counter, 11 operations lost to codegen casing showed up as nothing at all.
    pub unmatched_consumers: usize,
    /// distinct producer keys no consumer ever reached — a schema field, route or
    /// topic that nothing calls, or one whose callers ripple cannot see yet
    pub unused_providers: usize,
}

/// Link cross-service edges from the per-file facts already on each `CachedFile`.
pub fn link_cross_service(files: &[CachedFile], nodes: &[Node]) -> CrossEdges {
    // ── maps derived from the built graph ──
    // Several files can define one FQN once more than one repository is indexed —
    // an umbrella split across repos, a vendored copy. Keeping one was last-write-
    // wins, which silently attached every edge through that name to whichever file
    // the map happened to see last. They are candidates now, and the confidence is
    // split across them like any other ambiguity (#44).
    let mut fqn_to_module: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut fqn_to_class: HashMap<&str, Vec<SymbolId>> = HashMap::new();
    let mut fqns_in_file: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut fn_by_loc: HashMap<(&str, &str), SymbolId> = HashMap::new();
    // (start, end, id, is the container a function or the whole module body?)
    let mut fn_spans: HashMap<&str, Vec<(u32, u32, SymbolId, Granularity)>> = HashMap::new();
    for n in nodes {
        match n.kind {
            NodeKind::Class => {
                fqn_to_module
                    .entry(n.qualified_name.as_str())
                    .or_default()
                    .push(n.module_path.as_str());
                fqn_to_class
                    .entry(n.qualified_name.as_str())
                    .or_default()
                    .push(n.id);
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
        if f.extract.cross.entity_def {
            if let Some(fqns) = fqns_in_file.get(f.module_path.as_str()) {
                schema_fqns.extend(fqns);
            }
        }
    }

    // GraphQL operation name → the (scope, root field) pairs it selects,
    // aggregated across all .gql files
    let mut op_fields: HashMap<String, Vec<(&str, &[String])>> = HashMap::new();
    for f in files {
        for op in &f.extract.cross.graphql.operations {
            op_fields
                .entry(op.name.clone())
                .or_default()
                .push((op.scope.as_str(), op.path.as_slice()));
        }
    }

    // fragment name → its definition, so a spread can be expanded at link time
    let mut fragments: HashMap<&str, &lang::cross::GqlFragment> = HashMap::new();
    for f in files {
        for fragment in &f.extract.cross.graphql.fragments {
            fragments.insert(fragment.name.as_str(), fragment);
        }
    }
    // operation → the fragments it spreads. Most nested selections in a codegen app are
    // written in fragments, so without this the type-graph walk has almost nothing to
    // walk (363 spreads across 24 definitions on one real app).
    let mut op_spreads: HashMap<String, Vec<&lang::cross::GqlSpread>> = HashMap::new();
    for f in files {
        for spread in &f.extract.cross.graphql.spreads {
            op_spreads
                .entry(spread.op.clone())
                .or_default()
                .push(spread);
        }
    }

    // scope → scopes whose fields it includes (Absinthe `import_fields`)
    let mut includes: HashMap<&str, Vec<&str>> = HashMap::new();
    for f in files {
        for (scope, included) in &f.extract.cross.graphql.scope_includes {
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
    let mut producer: RouteIndex<SymbolId> = RouteIndex::default();
    // (scope, field) → the scope that field's children are declared in. This is the
    // schema's type graph, and it is what lets a nested selection be followed:
    // `lfgPosts` returns `:lfg_post`, so `lfgPosts { author }` asks object:lfg_post
    // for `author` (issue #22).
    let mut field_type: HashMap<(&str, &str), String> = HashMap::new();
    // fields whose resolver names a module, not a function: the edge is file-granular
    let mut context_producer: RouteIndex<SymbolId> = RouteIndex::default();
    for f in files {
        for p in &f.extract.cross.provides {
            let Some((declared, field)) = cross::graphql_scope_field(&p.key) else {
                continue; // another transport; this linker only speaks GraphQL so far
            };
            // a field is reachable under its own scope *and* under every root scope
            // that pulls that scope in via an include
            let mut scopes: Vec<&str> = vec![declared];
            if let Some(roots) = roots_by_scope.get(declared) {
                scopes.extend(roots.iter().copied());
            }
            if let Some(returns) = &p.returns {
                for scope in &scopes {
                    // the same spelling the detector uses for a type block's scope
                    field_type.insert((scope, field), returns.clone());
                }
            }
            match &p.handler {
                // the module FQN is already resolved (alias→FQN) by the detector
                cross::HandlerRef::Function { module, name } => {
                    let Some(hosts) = fqn_to_module.get(module.as_str()) else {
                        continue;
                    };
                    let ids: Vec<SymbolId> = hosts
                        .iter()
                        .filter_map(|file| fn_by_loc.get(&(*file, name.as_str())).copied())
                        .collect();
                    for scope in scopes {
                        for id in &ids {
                            producer.insert(cross::graphql_field_key(scope, field), *id);
                        }
                    }
                }
                // no function is named, so the module node is the honest target
                cross::HandlerRef::Module(module) => {
                    let Some(hosts) = fqn_to_module.get(module.as_str()) else {
                        continue;
                    };
                    for scope in scopes {
                        for host in hosts {
                            context_producer.insert(
                                cross::graphql_field_key(scope, field),
                                SymbolId::module(host),
                            );
                        }
                    }
                }
            }
        }
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
    // what the consumers reached, and what they asked for and did not find. Both
    // are reported: a number is how a drifted convention announces itself, and
    // silence is how the last one hid (#32)
    let mut matched: HashSet<ir::RouteKey> = HashSet::new();
    let mut unmatched_consumers = 0usize;
    for f in files {
        if f.extract.cross.graphql.op_refs.is_empty() {
            continue;
        }
        let src_id = SymbolId::module(&f.module_path);
        for op in &f.extract.cross.graphql.op_refs {
            let Some(fields) = op_fields.get(op.as_str()) else {
                continue;
            };
            for (scope, path) in fields {
                let resolvers = resolvers_for(&producer, &field_type, scope, path, &mut matched);
                let contexts_empty =
                    resolvers_for(&context_producer, &field_type, scope, path, &mut matched)
                        .is_empty();
                if resolvers.is_empty() && contexts_empty {
                    unmatched_consumers += 1;
                }
                if !resolvers.is_empty() {
                    // N candidates for one field → 1/N each (docs/06-risk-and-queries.md)
                    let conf = CONF_GRAPHQL / resolvers.len() as f32;
                    for resolver in resolvers {
                        if emit(&mut edges, src_id, resolver, EdgeKind::GraphqlCall, conf, 0) {
                            graphql += 1;
                        }
                    }
                }
                // a dataloader field names a context, not a function — coarser, and
                // priced accordingly (unchanged below)
                let contexts =
                    resolvers_for(&context_producer, &field_type, scope, path, &mut matched);
                if !contexts.is_empty() {
                    let conf = CONF_GRAPHQL_CONTEXT / contexts.len() as f32;
                    for context in contexts {
                        if emit(&mut edges, src_id, context, EdgeKind::GraphqlCall, conf, 0) {
                            graphql += 1;
                        }
                    }
                }
            }
            // `...LfgPostFields` — the fragment's own type condition names the scope its
            // fields live in, so an expanded spread needs no descent
            for spread in op_spreads.get(op.as_str()).into_iter().flatten() {
                expand_spread(
                    spread.fragment.as_str(),
                    &fragments,
                    &mut HashSet::new(),
                    MAX_FRAGMENT_HOPS,
                    &mut |scope: &str, path: &[String]| {
                        let mut hits = Vec::new();
                        let rs = resolvers_for(&producer, &field_type, scope, path, &mut matched);
                        if !rs.is_empty() {
                            hits.push((rs, CONF_GRAPHQL));
                        }
                        let cs = resolvers_for(
                            &context_producer,
                            &field_type,
                            scope,
                            path,
                            &mut matched,
                        );
                        if !cs.is_empty() {
                            hits.push((cs, CONF_GRAPHQL_CONTEXT));
                        }
                        for (targets, base) in hits {
                            let conf = base / targets.len() as f32;
                            for t in targets {
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
        if f.extract.cross.star_imports.is_empty() {
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
            let mut targets: Vec<SymbolId> = f
                .extract
                .cross
                .star_imports
                .iter()
                .filter_map(|fqn| fqn_to_module.get(fqn.as_str()))
                .flatten()
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
    let mut qualified_calls = 0;
    let mut db = 0;
    for f in files {
        let empty = Vec::new();
        let spans = fn_spans.get(f.module_path.as_str()).unwrap_or(&empty);

        for (fqn, func, line) in &f.extract.cross.qualified_calls {
            let Some(files) = fqn_to_module.get(fqn.as_str()) else {
                continue;
            };
            let mut targets: Vec<SymbolId> = files
                .iter()
                .filter_map(|file| fn_by_loc.get(&(*file, func.as_str())).copied())
                .collect();
            targets.sort_unstable();
            targets.dedup();
            let (caller, grain) = caller_at(spans, &f.module_path, *line);
            let conf = CONF_QUALIFIED_CALL / targets.len().max(1) as f32;
            for target in targets {
                if emit(&mut edges, caller, target, EdgeKind::Calls, conf, *line) {
                    qualified_calls += 1;
                    file_granular += usize::from(grain == Granularity::File);
                }
            }
        }
        for (fqn, line) in &f.extract.cross.entity_refs {
            if !schema_fqns.contains(fqn.as_str()) {
                continue;
            }
            let Some(targets) = fqn_to_class.get(fqn.as_str()) else {
                continue;
            };
            let (caller, grain) = caller_at(spans, &f.module_path, *line);
            let conf_each = CONF_DB_QUERY / targets.len().max(1) as f32;
            for &target in targets {
                if emit(
                    &mut edges,
                    caller,
                    target,
                    EdgeKind::DbQuery,
                    conf_each,
                    *line,
                ) {
                    db += 1;
                    file_granular += usize::from(grain == Granularity::File);
                }
            }
        }
    }

    let declared: HashSet<&ir::RouteKey> = producer.keys().chain(context_producer.keys()).collect();
    let unused_providers = declared.iter().filter(|k| !matched.contains(**k)).count();

    CrossEdges {
        edges,
        graphql,
        qualified_calls,
        db,
        file_granular,
        imported,
        unmatched_consumers,
        unused_providers,
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
    // the type condition is already the wire name both sides agree on
    let scope = fragment.type_condition.clone();
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
fn resolvers_for(
    producer: &RouteIndex<SymbolId>,
    field_type: &HashMap<(&str, &str), String>,
    root_scope: &str,
    path: &[String],
    matched: &mut HashSet<ir::RouteKey>,
) -> Vec<SymbolId> {
    let Some((last, parents)) = path.split_last() else {
        return Vec::new();
    };
    let mut scope: &str = root_scope;
    for parent in parents {
        match field_type.get(&(scope, parent.as_str())) {
            Some(next) => scope = next.as_str(),
            None => return Vec::new(),
        }
    }
    // one target reached twice is one target: several scopes can carry the same
    // field, and a diluted 1/N over a phantom ambiguity is a wrong number
    let key = cross::graphql_field_key(scope, last);
    let mut ids: Vec<SymbolId> = producer
        .matches(&key)
        .into_iter()
        .map(|(id, _quality)| *id)
        .collect();
    if !ids.is_empty() {
        matched.insert(key);
    }
    ids.sort_unstable();
    ids.dedup();
    ids
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
