//! The shape of a cached extract, as a string the store can compare.
//!
//! A cache row keyed on the file's content hash stays valid forever, so a build
//! whose `FileExtract` gained a field reads back rows the current parser never
//! produced. Where the new field is `#[serde(default)]` that deserializes
//! *successfully*, and the graph is then built from facts nobody extracted — with
//! no warning and no failure. It cost two measurement runs on #54 before anyone
//! noticed the edges were landing on the wrong node. See #56.
//!
//! The defence is a canonical value with every field written out and no
//! `..Default::default()` anywhere: adding a field to any type in the tree stops
//! this module compiling until the field is named here, which changes the shape
//! string, which invalidates the cache. Nobody has to remember anything.

use crate::{BindRec, FileExtract, ImportRec, Receiver, ReexportRec, RefKind, RefRec};
use ir::{Node, NodeKind, RiskScores, RouteKey, Segment, Span, SymbolId, Transport};
use lang::cross::{
    Consumes, CrossFacts, GqlFragment, GqlOp, GqlSpread, GraphqlFacts, HandlerRef, Provides,
};

/// Every field of a fully populated extract, as `path:json-type` lines, sorted.
///
/// Compared verbatim rather than hashed: a few hundred bytes is cheaper than
/// explaining a hash collision, and a mismatch can be read.
pub fn extract_shape() -> String {
    let value = serde_json::to_value(canonical()).unwrap_or(serde_json::Value::Null);
    let mut fields = Vec::new();
    walk(&value, &mut String::new(), &mut fields);
    fields.sort_unstable();
    fields.dedup();
    fields.join("\n")
}

fn walk(v: &serde_json::Value, path: &mut String, out: &mut Vec<String>) {
    use serde_json::Value;
    let kind = match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    out.push(format!("{path}:{kind}"));
    match v {
        // one element is enough: the canonical value puts exactly one in every
        // collection precisely so the element's own shape is reachable
        Value::Array(items) => {
            for item in items {
                let len = path.len();
                path.push_str("[]");
                walk(item, path, out);
                path.truncate(len);
            }
        }
        Value::Object(map) => {
            for (k, item) in map {
                let len = path.len();
                path.push('.');
                path.push_str(k);
                walk(item, path, out);
                path.truncate(len);
            }
        }
        _ => {}
    }
}

fn span() -> Span {
    Span {
        start_line: 1,
        start_col: 1,
        end_line: 1,
        end_col: 1,
    }
}

/// One value with every field set and every collection non-empty.
///
/// Do not reach for `..Default::default()` here, and do not leave a `Vec` empty:
/// both hide the shape of what they contain, which is the one thing this file is
/// for.
fn canonical() -> FileExtract {
    FileExtract {
        defs: vec![Node {
            id: SymbolId::of("m", "n"),
            kind: NodeKind::Function,
            name: "n".to_owned(),
            qualified_name: "n".to_owned(),
            module_path: "m".to_owned(),
            span: span(),
            extra_spans: vec![span()],
            is_exported: true,
            risk: RiskScores {
                churn: 0.0,
                complexity: 0.0,
                bug_density: 0.0,
                ownership: 0.0,
                fanout: 0.0,
                test_proximity: 0.0,
                composite: 0.0,
            },
            doc: Some("d".to_owned()),
            route_path: Some("r".to_owned()),
        }],
        imports: vec![ImportRec {
            local_name: "l".to_owned(),
            imported_name: "i".to_owned(),
            specifier: "s".to_owned(),
            site: span(),
        }],
        reexports: vec![ReexportRec {
            name: "n".to_owned(),
            exposed_as: "e".to_owned(),
            specifier: "s".to_owned(),
            site: span(),
        }],
        refs: vec![RefRec {
            name: "n".to_owned(),
            kind: RefKind::Call,
            site: span(),
            receiver: Some(Receiver::Ident("r".to_owned())),
            qualifier: Some("q".to_owned()),
        }],
        bindings: vec![BindRec {
            name: "n".to_owned(),
            type_name: "T".to_owned(),
            site: span(),
        }],
        test_scopes: vec![span()],
        cross: CrossFacts {
            provides: vec![Provides {
                key: route_key(),
                handler: HandlerRef::Function {
                    module: "M".to_owned(),
                    name: "f".to_owned(),
                },
                line: 1,
                returns: Some("t".to_owned()),
            }],
            consumes: vec![Consumes {
                key: route_key(),
                line: 1,
                confidence_hint: 1.0,
            }],
            graphql: GraphqlFacts {
                operations: vec![GqlOp {
                    name: "N".to_owned(),
                    scope: "query".to_owned(),
                    field: "f".to_owned(),
                    path: vec!["f".to_owned()],
                }],
                fragments: vec![GqlFragment {
                    name: "F".to_owned(),
                    type_condition: "T".to_owned(),
                    paths: vec![vec!["f".to_owned()]],
                    spreads: vec![(vec!["f".to_owned()], "G".to_owned())],
                }],
                spreads: vec![GqlSpread {
                    op: "N".to_owned(),
                    scope: "query".to_owned(),
                    at: vec!["f".to_owned()],
                    fragment: "F".to_owned(),
                }],
                scope_includes: vec![("a".to_owned(), "b".to_owned())],
                op_refs: vec!["N".to_owned()],
            },
            star_imports: vec!["M".to_owned()],
            qualified_calls: vec![("M".to_owned(), "f".to_owned(), 1)],
            entity_refs: vec![("E".to_owned(), 1)],
            entity_def: true,
        },
    }
}

fn route_key() -> RouteKey {
    RouteKey {
        transport: Transport::Http,
        method: Some("GET".to_owned()),
        path: vec![Segment::Literal("a".to_owned())],
    }
}

#[cfg(test)]
mod tests {
    /// The shape has to reach the leaves, or a field added deep in `CrossFacts`
    /// changes nothing and the cache is not invalidated — the exact failure #56
    /// describes.
    #[test]
    fn the_shape_reaches_every_leaf() {
        let shape = super::extract_shape();
        for leaf in [
            ".cross.provides[].line:number",
            ".cross.graphql.fragments[].spreads[][]:array",
            ".refs[].qualifier:string",
            ".defs[].risk.composite:number",
            ".cross.consumes[].confidence_hint:number",
        ] {
            assert!(shape.contains(leaf), "{leaf} missing from:\n{shape}");
        }
    }

    /// Two builds of the same code must agree, or the cache is discarded on every
    /// run and the incremental index stops being incremental.
    #[test]
    fn the_shape_is_stable_across_calls() {
        assert_eq!(super::extract_shape(), super::extract_shape());
    }
}
