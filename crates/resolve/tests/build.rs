use ir::{EdgeKind, SymbolId};
use std::path::Path;

fn fixture() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/proj")
}

#[test]
fn resolves_cross_file_call_and_import() {
    let r = resolve::build(&fixture()).unwrap();

    let helper = SymbolId::of("a.ts", "helper");
    let run = SymbolId::of("b.ts", "run");
    let boot = SymbolId::of("b.ts", "boot");

    assert!(r.nodes.iter().any(|n| n.id == helper && n.name == "helper"));
    assert!(r.nodes.iter().any(|n| n.id == run && n.name == "run"));

    // run() calls helper() — resolved across files via the import
    assert!(
        r.edges
            .iter()
            .any(|e| e.src == run && e.dst == helper && e.kind == EdgeKind::Calls),
        "run should call helper across files"
    );
    // boot() calls run() — local resolution
    assert!(
        r.edges
            .iter()
            .any(|e| e.src == boot && e.dst == run && e.kind == EdgeKind::Calls),
        "boot should call run locally"
    );
    // module imports helper
    assert!(
        r.edges
            .iter()
            .any(|e| e.dst == helper && e.kind == EdgeKind::Imports),
        "b.ts should import helper"
    );
}

#[test]
fn resolves_member_calls_by_type() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/members");
    let r = resolve::build(&root).unwrap();

    let service_handle = SymbolId::of("svc.ts", "Service.handle");
    let other_handle = SymbolId::of("svc.ts", "Other.handle");
    let use_typed = SymbolId::of("svc.ts", "useTyped");
    let use_new = SymbolId::of("svc.ts", "useNew");

    // distinct ids for same-named methods on different classes
    assert_ne!(service_handle, other_handle);

    // typed param `s: Service` → s.handle() resolves to Service.handle only
    let typed_targets: Vec<_> = r
        .edges
        .iter()
        .filter(|e| e.src == use_typed && e.kind == EdgeKind::Calls)
        .map(|e| e.dst)
        .collect();
    assert!(typed_targets.contains(&service_handle));
    assert!(
        !typed_targets.contains(&other_handle),
        "typed receiver must not hit the other class's method"
    );

    // A one-line method calling the same-named method of another class. The
    // definition-header guard (for languages where `def f(x)` parses as a call)
    // must not swallow it just because it shares the definition's line.
    let inline_handle = SymbolId::of("svc.ts", "Inline.handle");
    assert!(
        r.edges.iter().any(|e| e.src == inline_handle
            && e.dst == service_handle
            && e.kind == EdgeKind::Calls),
        "a one-line Inline.handle calling s.handle() should still reach Service.handle"
    );

    // `new Service().handle()` resolves to Service.handle
    assert!(r
        .edges
        .iter()
        .any(|e| e.src == use_new && e.dst == service_handle && e.kind == EdgeKind::Calls));
}

#[test]
fn incremental_matches_full_and_reuses_cache() {
    use std::collections::HashMap;
    let root = fixture();

    // full build
    let full = resolve::build(&root).unwrap();

    // first incremental (cold cache) then second (warm cache)
    let roots = std::slice::from_ref(&root);
    let cold = resolve::build_incremental(roots, &HashMap::new()).unwrap();
    let cache: HashMap<_, _> = cold
        .files
        .iter()
        .map(|f| (f.module_path.clone(), f.clone()))
        .collect();
    let warm = resolve::build_incremental(roots, &cache).unwrap();

    // incremental result is identical to a full rebuild
    assert_eq!(full.nodes.len(), warm.result.nodes.len());
    assert_eq!(full.edges.len(), warm.result.edges.len());

    // warm run reused every cached extract (no re-parse)
    assert_eq!(warm.stats.unchanged, warm.result.files_indexed);
    assert_eq!(warm.stats.added, 0);
    assert_eq!(warm.stats.changed, 0);
}

#[test]
fn resolves_tsconfig_path_alias() {
    // fixture tsconfig uses `@app/*` → `src/*`, with a comment + trailing commas (JSONC)
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tsconfig");
    let r = resolve::build(&root).unwrap();

    let helper = SymbolId::of("src/util.ts", "helper");
    let run = SymbolId::of("src/main.ts", "run");

    // import "@app/util" resolved through the alias → run() calls helper()
    assert!(
        r.edges
            .iter()
            .any(|e| e.src == run && e.dst == helper && e.kind == EdgeKind::Calls),
        "aliased import @app/util should resolve so run calls helper"
    );
}

#[test]
fn multi_root_merges_and_namespaces() {
    let a = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/proj");
    let b = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/members");
    let roots = vec![a, b];
    let r = resolve::build_incremental(&roots, &std::collections::HashMap::new()).unwrap();

    assert_eq!(r.roots.len(), 2);
    // module paths are namespaced by root tag (dir name) so both repos coexist
    assert!(r
        .result
        .nodes
        .iter()
        .any(|n| n.module_path.starts_with("proj/")));
    assert!(r
        .result
        .nodes
        .iter()
        .any(|n| n.module_path.starts_with("members/")));
    // symbols from both roots are present
    assert!(r.result.nodes.iter().any(|n| n.name == "helper"));
    assert!(r.result.nodes.iter().any(|n| n.name == "Service"));
}

/// TS document → GraphQL operation → Absinthe root field → Elixir resolver,
/// including the `import_fields` hop every real Absinthe schema uses. The
/// same-named field on `object :player` must NOT be linked: it isn't a root
/// field, and before scoping it collided with the root one.
#[test]
fn links_graphql_operations_to_root_field_resolvers() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/crossservice");
    let indexed = resolve::build_incremental(
        std::slice::from_ref(&root),
        &std::collections::HashMap::new(),
    )
    .unwrap();
    // cross-service linking is its own pass over the built graph (as in `ripple index`)
    let r = resolve::link_cross_service(&indexed.files, &indexed.result.nodes);

    let page = SymbolId::module("page.ts");
    let me = SymbolId::of("resolver.ex", "me");
    let follow = SymbolId::of("resolver.ex", "follow");
    let decoy = SymbolId::of("resolver.ex", "decoy");

    let targets: Vec<SymbolId> = r
        .edges
        .iter()
        .filter(|e| e.src == page && e.kind == EdgeKind::GraphqlCall)
        .map(|e| e.dst)
        .collect();

    assert!(
        targets.contains(&me),
        "query CurrentPlayer should reach the imported root field's resolver"
    );
    assert!(
        targets.contains(&follow),
        "mutation FollowPlayer should reach its resolver"
    );
    assert!(
        !targets.contains(&decoy),
        "a same-named field on object :player is not a root field and must not be linked"
    );

    // unambiguous match → full confidence, not split
    let conf = r
        .edges
        .iter()
        .find(|e| e.src == page && e.dst == me && e.kind == EdgeKind::GraphqlCall)
        .map(|e| e.confidence)
        .unwrap();
    assert!(
        (conf - 0.9).abs() < f32::EPSILON,
        "expected 0.9, got {conf}"
    );
}

/// Two imported objects declaring the same root field: both candidates are kept
/// and share the confidence, rather than one silently winning the key.
#[test]
fn ambiguous_root_field_splits_confidence() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/crossservice");
    let indexed = resolve::build_incremental(
        std::slice::from_ref(&root),
        &std::collections::HashMap::new(),
    )
    .unwrap();
    let r = resolve::link_cross_service(&indexed.files, &indexed.result.nodes);

    let page = SymbolId::module("page.ts");
    let legacy = SymbolId::of("resolver.ex", "legacy");

    // `query Duplicated` matches both :player_queries and :legacy_queries
    let legacy_edge = r
        .edges
        .iter()
        .find(|e| e.src == page && e.dst == legacy && e.kind == EdgeKind::GraphqlCall)
        .expect("ambiguous candidate should still be linked");
    assert!(
        (legacy_edge.confidence - 0.45).abs() < 1e-6,
        "2 candidates should halve confidence, got {}",
        legacy_edge.confidence
    );
}

/// Elixir definitions are macro calls, so local calls need the ref pipeline plus
/// two guards: a definition's own name in its header is not a call to itself, and
/// clauses of a multi-clause function are not calls to each other.
#[test]
fn resolves_elixir_local_calls() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/elixir");
    let r = resolve::build(&root).unwrap();

    let get_player = SymbolId::of("players.ex", "get_player");
    let list_players = SymbolId::of("players.ex", "list_players");
    let normalize = SymbolId::of("players.ex", "normalize");
    let fetch = SymbolId::of("players.ex", "fetch");
    let kind = SymbolId::of("players.ex", "kind");
    let countdown = SymbolId::of("players.ex", "countdown");

    let calls: Vec<(SymbolId, SymbolId)> = r
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls)
        .map(|e| (e.src, e.dst))
        .collect();

    // piped local calls resolve, including to a private helper
    assert!(
        calls.contains(&(get_player, normalize)),
        "get_player |> normalize"
    );
    assert!(calls.contains(&(get_player, fetch)), "get_player |> fetch");
    assert!(
        calls.contains(&(list_players, normalize)),
        "list_players |> normalize"
    );

    // A typespec names types, not call sites. Its refs sit outside any function,
    // so they would attach to whatever encloses them — the `defmodule` node.
    let defmodule = SymbolId::of("players.ex", "App.Players");
    for (dst, what) in [
        (get_player, "@spec get_player(String.t())"),
        (normalize, "@type ... normalize(term())"),
    ] {
        assert!(
            !calls.contains(&(defmodule, dst)),
            "{what} must not be a call site"
        );
    }

    // the whole graph for this fixture, so new noise can't slip in unnoticed
    assert_eq!(
        calls.len(),
        3,
        "expected exactly the 3 real local calls, got {calls:?}"
    );

    // no definition-header artifacts: no self-edges, no clause-to-clause edges
    assert!(
        !calls.iter().any(|(s, d)| s == d),
        "a definition must not call itself: {calls:?}"
    );
    for sym in [kind, countdown] {
        assert!(
            !calls.contains(&(sym, sym)),
            "multi-clause function linked to its own clauses"
        );
    }
}

/// Dependencies and build output are not this repo's code. Indexing them drowns
/// the graph: on a real Elixir umbrella `deps/` held 2176 source files against the
/// project's 762, three quarters of the call graph.
#[test]
fn skips_dependency_and_build_directories() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ignored");
    let r = resolve::build(&root).unwrap();

    let names: Vec<&str> = r.nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(
        names.contains(&"appOwnCode"),
        "the repo's own code is indexed"
    );
    assert!(
        !names.contains(&"vendoredLibraryCode"),
        "deps/ must not be indexed: {names:?}"
    );
    assert!(
        !names.contains(&"buildOutputCode"),
        "_build/ must not be indexed: {names:?}"
    );
}

/// Elixir `import Mod` lets a bare call cross a module boundary — the class of
/// edge `eval --oracle lsp` showed ripple was missing entirely.
#[test]
fn resolves_bare_calls_through_an_elixir_import() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/elixir_import");
    let indexed = resolve::build_incremental(
        std::slice::from_ref(&root),
        &std::collections::HashMap::new(),
    )
    .unwrap();
    let cross = resolve::link_cross_service(&indexed.files, &indexed.result.nodes);

    let run = SymbolId::of("importer.ex", "run");
    let prefers_local = SymbolId::of("importer.ex", "prefers_local");
    let helper_fun = SymbolId::of("helpers.ex", "helper_fun");
    let imported_shadowed = SymbolId::of("helpers.ex", "shadowed");

    let calls: Vec<(SymbolId, SymbolId)> = cross
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls)
        .map(|e| (e.src, e.dst))
        .collect();

    assert!(
        calls.contains(&(run, helper_fun)),
        "bare helper_fun() should reach the imported module: {calls:?}"
    );
    assert!(
        !calls.contains(&(prefers_local, imported_shadowed)),
        "a local definition of the same name must win over the import"
    );
    assert_eq!(cross.imported, 1, "exactly one call resolved via import");
}

/// Rust reaches other modules through paths, so resolving only same-file names left
/// `impact` blind on any Rust project — it reported zero dependents for functions
/// used across crates, including ripple's own.
#[test]
fn resolves_rust_path_calls() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust_paths");
    let r = resolve::build(&root).unwrap();

    let run = SymbolId::of("app.rs", "run");
    let client_new = SymbolId::of("store.rs", "Client::new");
    let local_new = SymbolId::of("app.rs", "Local::new");
    let helper = SymbolId::of("store.rs", "helper");

    let targets: Vec<SymbolId> = r
        .edges
        .iter()
        .filter(|e| e.src == run && e.kind == EdgeKind::Calls)
        .map(|e| e.dst)
        .collect();

    assert!(
        targets.contains(&client_new),
        "Client::new() should reach the `new` defined on Client: {targets:?}"
    );
    assert!(
        targets.contains(&helper),
        "store::helper(1) should reach helper across the module"
    );
    assert!(
        targets.contains(&local_new),
        "Local::new() should reach this file's own Local::new"
    );
    // `HashMap::new()` is a type we don't define: a capitalized qualifier must
    // resolve to its owner or to nothing. Falling back on the bare name linked
    // every collection constructor to an unrelated `new` (769 false edges on
    // ripple's own repo).
    let news: Vec<SymbolId> = targets
        .iter()
        .filter(|t| **t == client_new || **t == local_new)
        .copied()
        .collect();
    assert_eq!(
        news.len(),
        2,
        "exactly the two `new`s we call, nothing dragged in by HashMap::new()"
    );
}

#[test]
fn deterministic_build() {
    let a = resolve::build(&fixture()).unwrap();
    let b = resolve::build(&fixture()).unwrap();
    assert_eq!(a.nodes.len(), b.nodes.len());
    assert_eq!(a.edges.len(), b.edges.len());
}

/// One symbol written as several clauses keeps every definition site. Identity is
/// (path, qualified name), so the clauses share an id and used to overwrite each
/// other — leaving only one span, which silently breaks "which symbol contains this
/// line?" for everything downstream (LSP verification, review attribution).
#[test]
fn a_multi_clause_function_keeps_every_definition_site() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/elixir");
    let r = resolve::build(&root).unwrap();

    let kind: Vec<&ir::Node> = r
        .nodes
        .iter()
        .filter(|n| n.id == SymbolId::of("players.ex", "kind"))
        .collect();
    assert_eq!(kind.len(), 1, "one node per symbol, not one per clause");

    // `def kind(:admin)` on line 17, `def kind(_other)` on 18
    let spans: Vec<(u32, u32)> = kind[0]
        .definition_spans()
        .map(|s| (s.start_line, s.end_line))
        .collect();
    assert_eq!(spans, vec![(17, 17), (18, 18)]);
    assert!(kind[0].contains_line(18), "the second clause counts too");
    assert!(!kind[0].contains_line(19));
}

/// A cross-module call that sits inside no function — a module body, an ExUnit
/// `test` block — is attributed to the enclosing module instead of being dropped
/// (issue #18). Same-file resolution already did this; cross-service linking didn't,
/// so the same construct produced an edge or no edge depending on which path saw it.
#[test]
fn calls_outside_any_function_are_attributed_to_their_module() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/module_body");
    let indexed = resolve::build_incremental(
        std::slice::from_ref(&root),
        &std::collections::HashMap::new(),
    )
    .unwrap();
    // cross-module calls are linked in the cross-service pass, as in `ripple index`
    let cross = resolve::link_cross_service(&indexed.files, &indexed.result.nodes);

    let calls: Vec<(SymbolId, SymbolId)> = cross
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls)
        .map(|e| (e.src, e.dst))
        .collect();

    let me = SymbolId::of("resolver.ex", "me");
    let follow = SymbolId::of("resolver.ex", "follow");
    let legacy = SymbolId::of("resolver.ex", "legacy");
    let boot = SymbolId::of("boot.ex", "App.Boot");
    let start = SymbolId::of("boot.ex", "start");
    let boot_test = SymbolId::of("boot_test.exs", "App.BootTest");

    assert!(
        calls.contains(&(boot, me)),
        "a module-body call belongs to the module"
    );
    assert!(
        calls.contains(&(boot_test, legacy)),
        "a call in a `test` block belongs to the test module"
    );
    // and a call that *is* inside a function still names the function
    assert!(calls.contains(&(start, follow)));
    assert!(
        !calls.contains(&(boot, follow)),
        "the module must not absorb a call a function already owns"
    );
    // and the coarser edges are counted, not blended into the function-level ones
    assert_eq!(cross.file_granular, 2, "the module body and the test block");
}

/// A call inside a variable initialiser belongs to the enclosing *function*, not to
/// the variable. `const keys = new Set(collect())` captures `keys` as a definition
/// whose span contains the call, and it used to win as the innermost one — so the
/// edge named a value binding as the caller (issue #25).
#[test]
fn a_call_in_a_variable_initialiser_belongs_to_the_function() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/var_init");
    let r = resolve::build(&root).unwrap();

    let collect = SymbolId::of("keys.ts", "collect");
    let report = SymbolId::of("keys.ts", "report");
    let keys = SymbolId::of("keys.ts", "keys");

    let callers: Vec<SymbolId> = r
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls && e.dst == collect)
        .map(|e| e.src)
        .collect();

    assert_eq!(
        callers,
        vec![report],
        "the function calls it, not the const"
    );
    assert!(
        !callers.contains(&keys),
        "a value binding is not something that calls"
    );
}

/// Rendering a component is a call, an export list is still an export, and an import
/// from a `.tsx` file lands on `.ts` files too. All three had to hold before a React
/// component had any callers at all.
#[test]
fn a_rendered_component_is_a_caller() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/jsx");
    let r = resolve::build(&root).unwrap();

    let search = SymbolId::of("search.tsx", "SearchInput");
    let panel = SymbolId::of("panel.tsx", "Panel");
    let classes = SymbolId::of("util.ts", "classes");

    let calls: Vec<(SymbolId, SymbolId)> = r
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls)
        .map(|e| (e.src, e.dst))
        .collect();

    assert!(
        calls.contains(&(search, panel)),
        "<Panel /> renders it, and `export {{ Panel }}` exports it"
    );
    assert!(
        calls.contains(&(search, classes)),
        "a .tsx file importing a .ts file must resolve"
    );
}

/// An import that lands on a barrel file must follow the re-export chain: the barrel
/// defines nothing, so a direct lookup finds nothing and every consumer edge is lost
/// (issue #27 — 693 of them on one real app, through a single generated barrel).
#[test]
fn an_import_through_a_barrel_reaches_the_real_definition() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/barrel");
    let r = resolve::build(&root).unwrap();

    let render = SymbolId::of("page.ts", "render");
    let get_fragment_data = SymbolId::of("gen/masking.ts", "getFragmentData");
    let unmask = SymbolId::of("gen/masking.ts", "unmask");

    let calls: Vec<(SymbolId, SymbolId)> = r
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls)
        .map(|e| (e.src, e.dst))
        .collect();

    assert!(
        calls.contains(&(render, get_fragment_data)),
        "`export * from` passes the name through"
    );
    assert!(
        calls.contains(&(render, unmask)),
        "`export {{ unmask as reveal }} from` renames it on the way out"
    );
}

/// `import { a as b }` binds `b` to the source's `a`, and `import * as ns` binds a
/// whole module so `ns.foo()` resolves through its exports (issue #1). Both were
/// unresolvable, which silently dropped every call made through them.
#[test]
fn an_aliased_and_a_namespace_import_both_resolve() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/alias");
    let r = resolve::build(&root).unwrap();

    let run = SymbolId::of("page.ts", "run");
    let original = SymbolId::of("util.ts", "original");
    let other = SymbolId::of("util.ts", "other");

    let call = |dst: SymbolId| {
        r.edges
            .iter()
            .find(|e| e.kind == EdgeKind::Calls && e.src == run && e.dst == dst)
    };
    assert!(
        call(original).is_some(),
        "renamed('x') is a call to util.original"
    );
    let ns = call(other).expect("helpers.other('y') is a call to util.other");
    assert!(
        ns.confidence >= 0.9,
        "a namespace receiver is pinned by the import, not inferred: {}",
        ns.confidence
    );
}

/// Two functions binding the same name to different types is ordinary code, and a
/// file-wide type map sent every `client.send()` to whichever binding came last
/// (issue #2). Visibility is decided by position: the binding inside the calling
/// definition wins, module-level bindings stay visible, another function's local does
/// not leak.
#[test]
fn a_binding_is_scoped_to_the_function_that_declares_it() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scope");
    let r = resolve::build(&root).unwrap();

    let as_admin = SymbolId::of("callers.ts", "asAdmin");
    let as_user = SymbolId::of("callers.ts", "asUser");
    let admin_send = SymbolId::of("clients.ts", "AdminClient.send");
    let user_send = SymbolId::of("clients.ts", "UserClient.send");

    let targets = |src: SymbolId| -> Vec<SymbolId> {
        r.edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls && e.src == src)
            .map(|e| e.dst)
            .collect()
    };
    assert_eq!(
        targets(as_admin),
        vec![admin_send],
        "asAdmin's client is an AdminClient"
    );
    assert_eq!(
        targets(as_user),
        vec![user_send],
        "asUser's client is a UserClient — not whichever binding was declared last"
    );

    // `unbound` declares no `client`, and another function's local must not leak in:
    // with no type to go on this falls back to by-name candidates, split 1/N
    let unbound = SymbolId::of("callers.ts", "unbound");
    let mut got = targets(unbound);
    got.sort_by_key(|id| id.0);
    let mut both = vec![admin_send, user_send];
    both.sort_by_key(|id| id.0);
    assert_eq!(got, both, "an unbound receiver is ambiguous, not confident");
    let conf = r
        .edges
        .iter()
        .find(|e| e.kind == EdgeKind::Calls && e.src == unbound)
        .map(|e| e.confidence)
        .expect("edge");
    assert!(
        conf < 0.5,
        "two equally plausible targets must split confidence, got {conf}"
    );
}

/// A nested selection has a resolver of its own, and matching only root fields missed
/// every one (issue #22): `currentPlayer { team { … } }` must reach `team`'s resolver,
/// which is declared on `object :player`, not on the root.
#[test]
fn a_nested_selection_reaches_its_own_resolver() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nested");
    let indexed = resolve::build_incremental(
        std::slice::from_ref(&root),
        &std::collections::HashMap::new(),
    )
    .unwrap();
    let r = resolve::link_cross_service(&indexed.files, &indexed.result.nodes);

    let page = SymbolId::module("page.ts");
    let me = SymbolId::of("resolver.ex", "me");
    let team_of = SymbolId::of("resolver.ex", "team_of");
    let reached: Vec<SymbolId> = r
        .edges
        .iter()
        .filter(|e| e.src == page && e.kind == EdgeKind::GraphqlCall)
        .map(|e| e.dst)
        .collect();

    assert!(reached.contains(&me), "the root field's resolver");
    assert!(
        reached.contains(&team_of),
        "the nested field's resolver, found by descending currentPlayer's type"
    );

    // the same selection written through a fragment: its type condition names the
    // scope directly, so `...PlayerFields` reaches the fields inside it. Most nested
    // selections in a codegen app are written this way (363 spreads on one real app).
    let via_fragment = SymbolId::module("fragment_page.ts");
    let badges = SymbolId::module("badges.ex");
    let ctx = r
        .edges
        .iter()
        .find(|e| e.src == via_fragment && e.dst == badges && e.kind == EdgeKind::GraphqlCall)
        .expect("a spread reaches the fields the fragment selects");
    // `resolve: dataloader(App.Badges)` names a context, not a function, so the honest
    // target is that module — worth less than a named resolver, and priced that way
    assert!(
        ctx.confidence < 0.9,
        "module-granular is worth less than a named function: {}",
        ctx.confidence
    );
}

/// A document may name its operation `currentPlayer` while codegen emits
/// `CurrentPlayerDocument` — the name the TypeScript side references. Keying the join on
/// the raw name lost every edge from such an operation (11 of 242 on one real frontend).
#[test]
fn an_operation_named_lowercase_still_joins() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/opcase");
    let indexed = resolve::build_incremental(
        std::slice::from_ref(&root),
        &std::collections::HashMap::new(),
    )
    .unwrap();
    let r = resolve::link_cross_service(&indexed.files, &indexed.result.nodes);

    let page = SymbolId::module("page.ts");
    let me = SymbolId::of("resolver.ex", "me");
    assert!(
        r.edges
            .iter()
            .any(|e| e.src == page && e.dst == me && e.kind == EdgeKind::GraphqlCall),
        "the page reaches the resolver despite the casing difference"
    );
}

/// The hono case from #36: a `.test.ts` file imports and calls the function it
/// tests. Nothing in the workspace ever built a `Tests` edge before, so `review`
/// called every symbol in every repo untested.
#[test]
fn a_test_file_tests_what_it_calls() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/testlink/ts");
    let indexed = resolve::build_incremental(
        std::slice::from_ref(&root),
        &std::collections::HashMap::new(),
    )
    .unwrap();
    let scopes = resolve::TestScopes::of(&indexed.files, &indexed.roots, &lang::registry());
    let tests = resolve::link_tests(&scopes, &indexed.result.edges);

    let get_path = SymbolId::of("src/util.ts", "getPath");
    let runs = SymbolId::of("src/util.test.ts", "runs");
    let fixture_fn = SymbolId::of("src/util.test.ts", "fixture");

    assert!(
        tests
            .iter()
            .any(|e| e.src == runs && e.dst == get_path && e.kind == EdgeKind::Tests),
        "the test reaches the function it calls: {:?}",
        tests.iter().map(|e| (e.src, e.dst)).collect::<Vec<_>>()
    );
    assert!(
        !tests.iter().any(|e| e.dst == fixture_fn),
        "a helper the test calls inside itself is not a tested symbol"
    );
    assert!(
        !tests.iter().any(|e| e.src == get_path),
        "nothing flows out of the file under test"
    );
    // priced off the call it rests on, never at 1.0
    let edge = tests.iter().find(|e| e.dst == get_path).unwrap();
    assert!(
        edge.confidence > 0.0 && edge.confidence < 0.8,
        "a Tests edge is an inference on top of a call: {}",
        edge.confidence
    );
}

/// Rust's unit tests live in the file under test, so no path convention can see
/// them — the `@scope.test` capture is the only signal, and this is the case that
/// proves the two mechanisms are both needed.
#[test]
fn a_cfg_test_module_tests_the_file_it_sits_in() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/testlink/rs");
    let indexed = resolve::build_incremental(
        std::slice::from_ref(&root),
        &std::collections::HashMap::new(),
    )
    .unwrap();
    let scopes = resolve::TestScopes::of(&indexed.files, &indexed.roots, &lang::registry());
    let tests = resolve::link_tests(&scopes, &indexed.result.edges);

    let real = SymbolId::of("lib.rs", "real");
    let covers = SymbolId::of("lib.rs", "covers_real");
    assert!(
        tests
            .iter()
            .any(|e| e.src == covers && e.dst == real && e.kind == EdgeKind::Tests),
        "a test and the code it tests share a file: {:?}",
        tests.iter().map(|e| (e.src, e.dst)).collect::<Vec<_>>()
    );
}

fn xrepo() -> (std::path::PathBuf, std::path::PathBuf) {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/xrepo");
    (base.join("web"), base.join("api"))
}

/// The point of indexing two repositories at once: the frontend's
/// `import { send } from "@org/api-client"` lands on the backend's source, because
/// one repo declares that package name and the other consumes it.
#[test]
fn a_workspace_package_import_crosses_roots() {
    let (web, api) = xrepo();
    let r = resolve::build_incremental(&[web, api], &std::collections::HashMap::new()).unwrap();

    let main = SymbolId::module("web/src/main.ts");
    let run = SymbolId::of("web/src/main.ts", "run");
    let send = SymbolId::of("api/src/client.ts", "send");

    // through the barrel: the package resolves to api/src/index.ts, which only
    // re-exports ./client
    let import = r
        .result
        .edges
        .iter()
        .find(|e| e.src == main && e.dst == send && e.kind == EdgeKind::Imports)
        .expect("the import crosses the repo boundary");
    let call = r
        .result
        .edges
        .iter()
        .find(|e| e.src == run && e.dst == send && e.kind == EdgeKind::Calls)
        .expect("and so does the call through it");

    // priced below an in-repo resolution: the syntax pins the target, the premise
    // that these two trees are one program does not
    for e in [import, call] {
        assert!(
            e.confidence > 0.7 && e.confidence < 0.95,
            "a cross-root edge is discounted, not free: {}",
            e.confidence
        );
    }
}

/// One root's tsconfig aliases must not resolve another root's imports: `@web/*`
/// belongs to the web repo, and `api/src/leak.ts` naming it means an external
/// dependency ripple doesn't have, not the web repo's file.
#[test]
fn a_tsconfig_alias_does_not_leak_into_another_root() {
    let (web, api) = xrepo();
    let r = resolve::build_incremental(&[web, api], &std::collections::HashMap::new()).unwrap();

    let x = SymbolId::of("web/src/util.ts", "x");
    assert!(
        !r.result.edges.iter().any(|e| e.dst == x),
        "@web/util resolved from the api root, through the web root's tsconfig"
    );
}

/// Name-guessed resolution stays inside its root. Both repos define `Dup.run`; a
/// receiver-typed call in one must not become a 1/N split across both — otherwise
/// adding a second repo silently rewrites the first one's confidences.
#[test]
fn a_same_named_method_in_another_root_is_not_a_candidate() {
    let (web, api) = xrepo();
    let r = resolve::build_incremental(&[web, api], &std::collections::HashMap::new()).unwrap();

    let caller = SymbolId::of("web/src/dup.ts", "callsDup");
    let ours = SymbolId::of("web/src/dup.ts", "Dup.run");
    let theirs = SymbolId::of("api/src/client.ts", "Dup.run");

    let targets: Vec<_> = r
        .result
        .edges
        .iter()
        .filter(|e| e.src == caller && e.kind == EdgeKind::Calls)
        .collect();
    assert_eq!(
        targets.len(),
        1,
        "one target, not one per repo: {targets:?}"
    );
    assert_eq!(targets[0].dst, ours);
    assert!(!r.result.edges.iter().any(|e| e.dst == theirs));
}

/// The regression guard for the whole global-index change: indexing a repo alone
/// and indexing it beside another must produce the same edges inside it. SymbolIds
/// differ (multi-root namespaces the module path), so compare by name.
#[test]
fn edges_inside_a_root_survive_a_second_root() {
    let proj = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/proj");
    let members = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/members");

    let named = |r: &resolve::Indexed, tag: &str| {
        let by_id: std::collections::HashMap<_, _> = r
            .result
            .nodes
            .iter()
            .map(|n| (n.id, (n.qualified_name.clone(), n.module_path.clone())))
            .collect();
        let mut out: Vec<String> = r
            .result
            .edges
            .iter()
            .filter_map(|e| {
                let (sn, sm) = by_id.get(&e.src)?;
                let (dn, dm) = by_id.get(&e.dst)?;
                // a module node's qualified name *is* its (namespaced) path, so the
                // tag has to come off both halves of the label
                let strip = |m: &str| m.strip_prefix(tag).unwrap_or(m).to_owned();
                Some(format!(
                    "{}:{} -> {}:{} {:?} {:.4}",
                    strip(sm),
                    strip(sn),
                    strip(dm),
                    strip(dn),
                    e.kind,
                    e.confidence
                ))
            })
            .filter(|line| !line.contains("members/"))
            .collect();
        out.sort();
        out
    };

    let alone = resolve::build_incremental(
        std::slice::from_ref(&proj),
        &std::collections::HashMap::new(),
    )
    .unwrap();
    let together =
        resolve::build_incremental(&[proj.clone(), members], &std::collections::HashMap::new())
            .unwrap();

    assert_eq!(
        named(&alone, ""),
        named(&together, "proj/"),
        "adding a second root changed the first one's edges"
    );
}

/// A test file's `import` is how it reaches the code it tests; without counting it
/// the test *module* stayed a dependent, so a well-tested symbol still scored
/// fanout and #42's fix only worked on the function-level caller.
#[test]
fn an_import_from_a_whole_test_file_is_also_a_test_edge() {
    let ts = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/testlink/ts");
    let indexed =
        resolve::build_incremental(std::slice::from_ref(&ts), &std::collections::HashMap::new())
            .unwrap();
    let scopes = resolve::TestScopes::of(&indexed.files, &indexed.roots, &lang::registry());
    let tests = resolve::link_tests(&scopes, &indexed.result.edges);

    let get_path = SymbolId::of("src/util.ts", "getPath");
    let test_module = SymbolId::module("src/util.test.ts");
    assert!(
        tests
            .iter()
            .any(|e| e.src == test_module && e.dst == get_path),
        "the test file itself tests what it imports"
    );

    // Rust's tests live in the file they test, so that file's module node holds
    // production code too and must not be marked test-side
    let rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/testlink/rs");
    let indexed =
        resolve::build_incremental(std::slice::from_ref(&rs), &std::collections::HashMap::new())
            .unwrap();
    let scopes = resolve::TestScopes::of(&indexed.files, &indexed.roots, &lang::registry());
    let tests = resolve::link_tests(&scopes, &indexed.result.edges);
    let module = SymbolId::module("lib.rs");
    assert!(
        !tests.iter().any(|e| e.src == module),
        "a file that merely contains a #[cfg(test)] mod is not a test file"
    );
}
