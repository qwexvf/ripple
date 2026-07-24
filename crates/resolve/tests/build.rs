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
    assert!(r.result.nodes.iter().any(|n| n.module_path.starts_with("proj/")));
    assert!(r.result.nodes.iter().any(|n| n.module_path.starts_with("members/")));
    // symbols from both roots are present
    assert!(r.result.nodes.iter().any(|n| n.name == "helper"));
    assert!(r.result.nodes.iter().any(|n| n.name == "Service"));
}

#[test]
fn deterministic_build() {
    let a = resolve::build(&fixture()).unwrap();
    let b = resolve::build(&fixture()).unwrap();
    assert_eq!(a.nodes.len(), b.nodes.len());
    assert_eq!(a.edges.len(), b.edges.len());
}
