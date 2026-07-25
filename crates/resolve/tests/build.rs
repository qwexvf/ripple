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
