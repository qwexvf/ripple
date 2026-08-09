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
/// A consumer key matching a declared endpoint, before the match quality and how
/// much of the path was literal are applied. Same band as a matched GraphQL
/// operation: both sides wrote the route down, and the two spellings agree.
const CONF_ENDPOINT: f32 = 0.9;
/// A handler ← the declaration that mounts it. Same band as a matched endpoint:
/// the declaration names the handler outright. Split 1/N when the named module
/// resolves to several files. See #54.
const CONF_SERVES: f32 = 0.9;

pub struct CrossEdges {
    pub edges: Vec<Edge>,
    pub graphql: usize,
    /// calls resolved through an explicit module FQN
    pub qualified_calls: usize,
    pub db: usize,
    /// bare calls resolved through an `import`
    pub imported: usize,
    /// handlers linked back to the router/schema declaration that mounts them (#54)
    pub mounted: usize,
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
    /// consumer→handler edges across a non-GraphQL boundary (HTTP today)
    pub endpoints: usize,
    /// handler symbol → the literal words of the route it serves (`auth login`),
    /// so a task described by its URL reaches the handler in `query::locate`. A
    /// handler serving several routes accumulates all of them. Applied to the
    /// node's `route_path` by the caller, which holds the nodes mutably.
    pub route_paths: Vec<(SymbolId, String)>,
}

/// The literal path segments of a route key, joined for search (`/auth/login`
/// with a `:id` param → `auth login`). `None` when the route is all params.
fn route_text(key: &ir::RouteKey) -> Option<String> {
    let words: Vec<&str> = key
        .path
        .iter()
        .filter_map(|s| match s {
            ir::Segment::Literal(w) => Some(w.as_str()),
            _ => None,
        })
        .collect();
    if words.is_empty() {
        None
    } else {
        Some(words.join(" "))
    }
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
    // (handler, the declaration that mounts it, confidence). Filled by both provider
    // passes and emitted once below, so the rule is stated in one place for every
    // transport. A declaration mounting many handlers is not ambiguous — the split is
    // over how many files answered to the *one* module name it wrote down.
    let mut serves: Vec<(SymbolId, SymbolId, f32)> = Vec::new();
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
                    let declaration = declaration_at(&fn_spans, &f.module_path, p.line);
                    let conf = CONF_SERVES / ids.len().max(1) as f32;
                    serves.extend(ids.iter().map(|id| (*id, declaration, conf)));
                    for scope in scopes {
                        for id in &ids {
                            producer.insert(cross::graphql_field_key(scope, field), *id);
                        }
                    }
                }
                // a GraphQL field served by the file that declares it has no
                // meaning today — only a router does that — so it is not linked
                cross::HandlerRef::Here => {}
                // no function is named, so the module node is the honest target
                cross::HandlerRef::Module(module) => {
                    let Some(hosts) = fqn_to_module.get(module.as_str()) else {
                        continue;
                    };
                    let declaration = declaration_at(&fn_spans, &f.module_path, p.line);
                    let conf = CONF_SERVES / hosts.len().max(1) as f32;
                    serves.extend(
                        hosts
                            .iter()
                            .map(|h| (SymbolId::module(h), declaration, conf)),
                    );
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

    // ── every other transport: one pass, no framework ──
    //
    // A detector emitted `Provides` and `Consumes` keyed the same way; all that is
    // left is a lookup. Adding Express or FastAPI adds a detector, not a branch
    // here — that is what phases 1 and 2 were for.
    let mut endpoints_idx: RouteIndex<SymbolId> = RouteIndex::default();
    let mut route_paths: Vec<(SymbolId, String)> = Vec::new();
    for f in files {
        for p in &f.extract.cross.provides {
            if p.key.transport == ir::Transport::Graphql {
                continue; // its own protocol pass above, with scope expansion
            }
            let handler = match &p.handler {
                cross::HandlerRef::Function { module, name } => fqn_to_module
                    .get(module.as_str())
                    .into_iter()
                    .flatten()
                    .find_map(|file| fn_by_loc.get(&(*file, name.as_str())).copied()),
                cross::HandlerRef::Module(module) => fqn_to_module
                    .get(module.as_str())
                    .and_then(|files| files.first())
                    .map(|file| SymbolId::module(file)),
                // no symbol was named, so the declaration itself is the target —
                // the module the declaration sits in, or the file when nothing
                // encloses it. `SymbolId::module` alone was the file node, which is
                // a different symbol from the one `review` ranks (#54).
                cross::HandlerRef::Here => Some(declaration_at(&fn_spans, &f.module_path, p.line)),
            };
            if let Some(handler) = handler {
                // `Here` names no separate handler: the declaring file serves it, so
                // there is nothing for the declaration to gain a dependent from.
                // Pushing it anyway linked a file node to the module node beside it
                // and inflated the declaring file's own fanout.
                if !matches!(p.handler, cross::HandlerRef::Here) {
                    let declaration = declaration_at(&fn_spans, &f.module_path, p.line);
                    serves.push((handler, declaration, CONF_SERVES));
                }
                // only request routes locate a feature: an HTTP/RPC path names what a
                // caller asked for. A `Db` key's "path" is a table name and a `PubSub`
                // key's is a topic — stamping those made a migration's `up/change` the
                // top hit for "send a notification", which is never where work starts.
                if matches!(
                    p.key.transport,
                    ir::Transport::Http | ir::Transport::Grpc | ir::Transport::Rpc
                ) {
                    if let Some(text) = route_text(&p.key) {
                        route_paths.push((handler, text));
                    }
                }
                endpoints_idx.insert(p.key.clone(), handler);
            }
        }
    }

    // A router calls nothing it routes to, so the file governing every route in a
    // service otherwise has a fanout of zero and sinks to the bottom of every review
    // (#54). Emitted after both provider passes so one rule covers every transport:
    // whoever the declaration named now depends on the declaration. `emit` drops the
    // self-edge a `HandlerRef::Here` produces.
    let mut mounted = 0;
    // the strongest evidence wins when a handler is named more than once: sorting
    // by confidence descending puts it first, and `emit` keeps the first of a pair
    serves.sort_by(|a, b| {
        (a.0, a.1)
            .cmp(&(b.0, b.1))
            .then(b.2.total_cmp(&a.2))
            .then(a.0.cmp(&b.0))
    });
    for (handler, declaration, conf) in &serves {
        if emit(
            &mut edges,
            *handler,
            *declaration,
            EdgeKind::Serves,
            *conf,
            0,
        ) {
            mounted += 1;
        }
    }

    let mut endpoints = 0;
    for f in files {
        let empty = Vec::new();
        let spans = fn_spans.get(f.module_path.as_str()).unwrap_or(&empty);
        for c in &f.extract.cross.consumes {
            let hits = endpoints_idx.matches(&c.key);
            if hits.is_empty() {
                unmatched_consumers += 1;
                continue;
            }
            matched.insert(c.key.clone());
            let (caller, grain) = caller_at(spans, &f.module_path, c.line);
            let kind = match c.key.transport {
                ir::Transport::PubSub => EdgeKind::AsyncCall,
                // a table is declared, not called: the schema depends on the
                // migration that creates it exactly the way a handler depends on
                // the router that mounts it (#54)
                ir::Transport::Db => EdgeKind::Serves,
                _ => EdgeKind::HttpCall,
            };
            let n = hits.len() as f32;
            for (target, quality) in hits {
                // how well the key matched, then how much of the path the consumer
                // actually spelled: `/api/${a}/${b}` pins less than `/api/users`
                let conf = quality.confidence(CONF_ENDPOINT) * (0.5 + 0.5 * c.confidence_hint) / n;
                if emit(&mut edges, caller, *target, kind, conf, c.line) {
                    endpoints += 1;
                    file_granular += usize::from(grain == Granularity::File);
                }
            }
        }
    }

    let declared: HashSet<&ir::RouteKey> = producer
        .keys()
        .chain(context_producer.keys())
        .chain(endpoints_idx.keys())
        .collect();
    let unused_providers = declared.iter().filter(|k| !matched.contains(**k)).count();

    CrossEdges {
        edges,
        graphql,
        qualified_calls,
        db,
        file_granular,
        imported,
        mounted,
        unmatched_consumers,
        unused_providers,
        endpoints,
        route_paths,
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
/// The symbol a route declaration is written inside.
///
/// A Phoenix `socket`/`get` sits in the router module's body, not in any function,
/// so this lands on the module — which is the symbol `review` ranks. Attributing it
/// to `SymbolId::module(path)` instead pointed the edge at the *file* node, a
/// different symbol that no reviewer ever looks at (#54).
fn declaration_at(
    fn_spans: &HashMap<&str, Vec<(u32, u32, SymbolId, Granularity)>>,
    module_path: &str,
    line: u32,
) -> SymbolId {
    let empty = Vec::new();
    let spans = fn_spans.get(module_path).unwrap_or(&empty);
    caller_at(spans, module_path, line).0
}

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
