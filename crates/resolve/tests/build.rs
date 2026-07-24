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

#[test]
fn deterministic_build() {
    let a = resolve::build(&fixture()).unwrap();
    let b = resolve::build(&fixture()).unwrap();
    assert_eq!(a.nodes.len(), b.nodes.len());
    assert_eq!(a.edges.len(), b.edges.len());
}
