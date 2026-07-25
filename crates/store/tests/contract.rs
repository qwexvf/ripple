//! GraphStore contract: any backend must round-trip the graph and the extract
//! cache identically. Written against `&mut dyn GraphStore` so a future
//! `SamyamaStore` runs the same suite by adding one `#[test]` wrapper.

use ir::{Edge, EdgeKind, Node, NodeKind, Span, SymbolId};
use parse::{CachedFile, FileExtract};
use std::path::PathBuf;
use store::{Dir, GraphStore, RedbStore};

fn span() -> Span {
    Span {
        start_line: 1,
        start_col: 1,
        end_line: 1,
        end_col: 1,
    }
}

fn node(module: &str, name: &str, kind: NodeKind) -> Node {
    Node {
        id: SymbolId::of(module, name),
        kind,
        name: name.to_owned(),
        qualified_name: name.to_owned(),
        module_path: module.to_owned(),
        span: span(),
        extra_spans: Vec::new(),
        is_exported: true,
        risk: ir::RiskScores::default(),
    }
}

fn tmp(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "ripple-contract-{}-{}.redb",
        tag,
        std::process::id()
    ))
}

/// The reusable contract every GraphStore impl must satisfy.
fn run_contract(store: &mut dyn GraphStore) {
    let caller = node("a.ts", "run", NodeKind::Function);
    let callee = node("b.ts", "helper", NodeKind::Function);
    let edge = Edge {
        src: caller.id,
        dst: callee.id,
        kind: EdgeKind::Calls,
        confidence: 0.95,
        site: span(),
        source: ir::EdgeSource::Extracted,
    };

    store
        .write(&[caller.clone(), callee.clone()], &[edge])
        .unwrap();
    let graph = store.load().unwrap();

    // nodes round-trip
    assert_eq!(graph.node_count(), 2);
    assert_eq!(graph.get(caller.id).unwrap().name, "run");

    // edge survives and is traversable in both directions
    let out = graph.neighbors(caller.id, Dir::Out, Some(&[EdgeKind::Calls]), 1);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].node.id, callee.id);
    let inc = graph.neighbors(callee.id, Dir::In, Some(&[EdgeKind::Calls]), 1);
    assert_eq!(inc.len(), 1);
    assert_eq!(inc[0].node.id, caller.id);

    // extract cache round-trips
    let cf = CachedFile {
        canonical: PathBuf::from("/proj/a.ts"),
        module_path: "a.ts".to_owned(),
        hash: "deadbeef".to_owned(),
        extract: FileExtract::default(),
    };
    store.write_extracts(&[cf]).unwrap();
    let cache = store.read_extracts().unwrap();
    assert_eq!(cache.len(), 1);
    assert_eq!(cache["a.ts"].hash, "deadbeef");

    // roots round-trip, and are empty before anything records them
    assert!(store.read_roots().unwrap().is_empty());
    let roots = vec![
        ("web".to_owned(), PathBuf::from("/proj/web")),
        ("api".to_owned(), PathBuf::from("/proj/api")),
    ];
    store.write_roots(&roots).unwrap();
    assert_eq!(store.read_roots().unwrap(), roots);

    // writing the graph again must not wipe the extract cache or the roots
    store.write(std::slice::from_ref(&caller), &[]).unwrap();
    assert_eq!(store.read_extracts().unwrap().len(), 1);
    assert_eq!(store.read_roots().unwrap(), roots);

    // write_index replaces all three at once — what `ripple index` uses, so that a
    // crash can't leave a graph, a cache and a root list describing different runs
    let cf2 = CachedFile {
        canonical: PathBuf::from("/proj/b.ts"),
        module_path: "b.ts".to_owned(),
        hash: "cafe".to_owned(),
        extract: FileExtract::default(),
    };
    let roots2 = vec![("solo".to_owned(), PathBuf::from("/proj"))];
    store
        .write_index(&[caller, callee], &[], &[cf2], &roots2)
        .unwrap();
    let graph = store.load().unwrap();
    assert_eq!(graph.node_count(), 2);
    let cache = store.read_extracts().unwrap();
    assert_eq!(cache.len(), 1, "the old cache row is replaced, not merged");
    assert_eq!(cache["b.ts"].hash, "cafe");
    assert_eq!(store.read_roots().unwrap(), roots2);
}

/// Edges are stored as JSON, so a graph written before `source` existed must still
/// load — as `Extracted`, since that's what produced every pre-provenance edge.
#[test]
fn an_edge_without_provenance_loads_as_extracted() {
    let old = r#"{"src":1,"dst":2,"kind":"Calls","confidence":0.95,
                  "site":{"start_line":1,"start_col":1,"end_line":1,"end_col":1}}"#;
    let e: Edge = serde_json::from_str(old).expect("pre-provenance edge must deserialize");
    assert_eq!(e.source, ir::EdgeSource::Extracted);
}

/// Exact-only lookup made every Elixir module unfindable by the name a human types:
/// a module's name *is* its qualified name, so `impact LfgPost` matched nothing while
/// `impact FiveNoobs.Lfgs.LfgPost` worked. The rule widens only when the stricter one
/// finds nothing, and reports which one fired.
#[test]
fn a_name_lookup_widens_only_when_it_has_to() {
    use store::{InMemoryGraph, Match};

    let mut module = node(
        "lfgs/lfg_post.ex",
        "FiveNoobs.Lfgs.LfgPost",
        NodeKind::Class,
    );
    module.qualified_name = "FiveNoobs.Lfgs.LfgPost".to_owned();
    let mut other = node(
        "lfgs/lfg_posts.ex",
        "FiveNoobs.Lfgs.LfgPosts",
        NodeKind::Class,
    );
    other.qualified_name = "FiveNoobs.Lfgs.LfgPosts".to_owned();
    let get_post = node("lfgs/lfg_posts.ex", "get_post", NodeKind::Function);
    let graph = InMemoryGraph::from_parts(vec![module, other, get_post], Vec::new());

    let (hit, how) = graph.lookup("get_post").expect("exact");
    assert_eq!((hit.len(), how), (1, Match::Exact));

    // the name a human types is the last segment
    let (hit, how) = graph.lookup("LfgPost").expect("suffix");
    assert_eq!(how, Match::QualifiedSuffix);
    assert_eq!(
        hit.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(),
        vec!["FiveNoobs.Lfgs.LfgPost"],
        "a suffix must land on a segment boundary, so LfgPosts is not a match"
    );

    // exact wins over the looser rules even when both could match
    let (hit, how) = graph.lookup("FiveNoobs.Lfgs.LfgPost").expect("exact");
    assert_eq!((hit.len(), how), (1, Match::Exact));

    // last resort, case-insensitive, and it says so
    let (hit, how) = graph.lookup("lfgpost").expect("substring");
    assert_eq!(how, Match::Substring);
    assert_eq!(hit.len(), 2, "both LfgPost and LfgPosts contain it");

    assert!(graph.lookup("nothing_like_this").is_none());
    assert!(
        graph.lookup("lfg_post").is_none(),
        "matching ignores case, not punctuation — a module path is not a symbol name"
    );
}

#[test]
fn redb_store_satisfies_contract() {
    let path = tmp("redb");
    let _ = std::fs::remove_file(&path);
    let mut s = RedbStore::open(&path);
    run_contract(&mut s);
    let _ = std::fs::remove_file(&path);
}
