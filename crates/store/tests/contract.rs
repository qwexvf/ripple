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
    store.write(&[caller], &[]).unwrap();
    assert_eq!(store.read_extracts().unwrap().len(), 1);
    assert_eq!(store.read_roots().unwrap(), roots);
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

#[test]
fn redb_store_satisfies_contract() {
    let path = tmp("redb");
    let _ = std::fs::remove_file(&path);
    let mut s = RedbStore::open(&path);
    run_contract(&mut s);
    let _ = std::fs::remove_file(&path);
}
