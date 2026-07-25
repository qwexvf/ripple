//! Language-agnostic tree-sitter driver: parse a file once, run the adapter's
//! queries, emit IR nodes + pre-resolution records (imports, refs, type
//! bindings). Reads capture names generically. See docs/04-architecture.md, v0-plan.md.

use anyhow::{Context, Result};
use ir::{Node, NodeKind, Span, SymbolId};
use lang::LanguageAdapter;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use streaming_iterator::StreamingIterator;
use tree_sitter::{
    Node as TsNode, Parser, Query, QueryCursor, QueryMatch, QueryPredicateArg, Tree,
};

/// A file's extraction plus its content hash — the unit of incremental caching.
/// Unchanged files reuse this across `index` runs, skipping the parse. See
/// docs/v0-plan.md "incremental".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedFile {
    pub canonical: PathBuf,
    pub module_path: String,
    pub hash: String,
    pub extract: FileExtract,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportRec {
    pub local_name: String,
    pub imported_name: String, // "default" for default imports
    pub specifier: String,
    pub site: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefKind {
    Call,   // foo()
    Member, // obj.foo()
}

/// The receiver of a member call, classified syntactically (M2, no type inference).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Receiver {
    This,          // this.foo()
    Ident(String), // b.foo()  — resolved via local type bindings
    New(String),   // new Bar().foo()
    Other,         // chained / computed — falls back to candidates
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefRec {
    pub name: String,
    pub kind: RefKind,
    pub site: Span,
    pub receiver: Option<Receiver>, // Some for Member
    /// What preceded the name in a qualified call — `Client` in `Client::new(x)`,
    /// `resolve` in `resolve::link(x)`. Lets resolution prefer a definition that
    /// belongs to the qualifier instead of every same-named definition anywhere.
    /// `default` so a cache written before this field still loads.
    #[serde(default)]
    pub qualifier: Option<String>,
}

/// A local identifier → type-name binding (`const b = new Bar()`, `(b: Bar)`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindRec {
    pub name: String,
    pub type_name: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct FileExtract {
    pub defs: Vec<Node>,
    pub imports: Vec<ImportRec>,
    pub refs: Vec<RefRec>,
    pub bindings: Vec<BindRec>,
    /// Cross-service facts (Absinthe fields, GraphQL ops, TS Document usage, …),
    /// extracted from the same parse so files aren't parsed twice.
    pub cross: lang::cross::CrossFacts,
}

/// An adapter's queries, compiled once and reused across all files. Compiling a
/// tree-sitter `Query` is expensive (builds automata); doing it per file per
/// query dominated indexing before this. `Query` is `Sync`, so a shared `&Queries`
/// is safe across rayon threads.
pub struct Queries {
    tags: Query,
    imports: Option<Query>,
    refs: Option<Query>,
    bindings: Option<Query>,
}

impl Queries {
    pub fn compile(adapter: &dyn LanguageAdapter) -> Result<Queries> {
        let lang = adapter.grammar();
        let compile = |src: &str, what: &str| {
            Query::new(&lang, src)
                .with_context(|| format!("invalid {what} query for {}", adapter.id()))
        };
        Ok(Queries {
            tags: compile(adapter.tags_query(), "tags.scm")?,
            imports: adapter
                .imports_query()
                .map(|q| compile(q, "imports.scm"))
                .transpose()?,
            refs: adapter
                .refs_query()
                .map(|q| compile(q, "refs.scm"))
                .transpose()?,
            bindings: adapter
                .bindings_query()
                .map(|q| compile(q, "bindings.scm"))
                .transpose()?,
        })
    }
}

pub fn extract_file(
    source: &str,
    adapter: &dyn LanguageAdapter,
    module_path: &str,
    queries: &Queries,
) -> Result<FileExtract> {
    let language = adapter.grammar();
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .context("failed to set tree-sitter language")?;
    let tree = parser
        .parse(source, None)
        .context("tree-sitter returned no tree")?;
    let src = source.as_bytes();

    let defs = extract_defs(&tree, &queries.tags, src, module_path, adapter)?;
    let imports = queries
        .imports
        .as_ref()
        .map(|q| extract_imports(&tree, q, src))
        .transpose()?
        .unwrap_or_default();
    let refs = queries
        .refs
        .as_ref()
        .map(|q| extract_refs(&tree, q, src))
        .transpose()?
        .unwrap_or_default();
    let bindings = queries
        .bindings
        .as_ref()
        .map(|q| extract_bindings(&tree, q, src))
        .transpose()?
        .unwrap_or_default();
    let cross = adapter.extract_cross(tree.root_node(), src);

    Ok(FileExtract {
        defs,
        imports,
        refs,
        bindings,
        cross,
    })
}

/// Convenience for one-off use (compiles queries each call). Not for hot loops.
pub fn extract(source: &str, adapter: &dyn LanguageAdapter) -> Result<Vec<Node>> {
    let queries = Queries::compile(adapter)?;
    Ok(extract_file(source, adapter, "<file>", &queries)?.defs)
}

fn extract_defs(
    tree: &Tree,
    query: &Query,
    src: &[u8],
    module_path: &str,
    adapter: &dyn LanguageAdapter,
) -> Result<Vec<Node>> {
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), src);

    let mut nodes = Vec::new();
    while let Some(m) = matches.next() {
        if !predicates_hold(query, m, src) {
            continue;
        }
        let mut kind = None;
        let mut def_node = None;
        let mut name = None;
        for cap in m.captures {
            let cap_name = names[cap.index as usize];
            if cap_name == "name" {
                name = cap.node.utf8_text(src).ok().map(str::to_owned);
            } else if let Some(k) = NodeKind::from_capture(cap_name) {
                kind = Some(k);
                def_node = Some(cap.node);
            }
        }
        if let (Some(kind), Some(def_node), Some(name)) = (kind, def_node, name) {
            // export/private + name qualification are language-specific → adapter.
            let qualified_name = adapter.qualified_name(kind, &name, def_node, src);
            nodes.push(Node {
                id: SymbolId::of(module_path, &qualified_name),
                kind,
                name,
                qualified_name,
                module_path: module_path.to_owned(),
                span: span_of(def_node),
                is_exported: adapter.is_exported(def_node, src),
                risk: ir::RiskScores::default(),
            });
        }
    }
    Ok(nodes)
}

fn extract_imports(tree: &Tree, query: &Query, src: &[u8]) -> Result<Vec<ImportRec>> {
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), src);

    let mut out = Vec::new();
    while let Some(m) = matches.next() {
        if !predicates_hold(query, m, src) {
            continue;
        }
        let mut specifier = None;
        let mut named: Vec<(String, Span)> = Vec::new();
        let mut default: Option<(String, Span)> = None;
        for cap in m.captures {
            let text = cap.node.utf8_text(src).unwrap_or("").to_owned();
            match names[cap.index as usize] {
                "import.source" => specifier = Some(text),
                "import.name" => named.push((text, span_of(cap.node))),
                "import.default" => default = Some((text, span_of(cap.node))),
                _ => {}
            }
        }
        let Some(specifier) = specifier else { continue };
        for (n, site) in named {
            out.push(ImportRec {
                local_name: n.clone(),
                imported_name: n,
                specifier: specifier.clone(),
                site,
            });
        }
        if let Some((n, site)) = default {
            out.push(ImportRec {
                local_name: n,
                imported_name: "default".to_owned(),
                specifier: specifier.clone(),
                site,
            });
        }
    }
    Ok(out)
}

/// Reference sites, from `ref.call` / `ref.member` / `ref.recv` captures.
///
/// A `ref.ignore` capture marks a region where references don't count. Patterns
/// in a tree-sitter query match independently, so a broad pattern can't be
/// narrowed by a more specific one — an adapter marks the exceptions instead
/// (Elixir: everything inside `@spec get(id :: String.t()) :: t()` parses as
/// calls, but names types).
fn extract_refs(tree: &Tree, query: &Query, src: &[u8]) -> Result<Vec<RefRec>> {
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), src);

    let mut out = Vec::new();
    let mut ignored: Vec<((u32, u32), (u32, u32))> = Vec::new();
    while let Some(m) = matches.next() {
        if !predicates_hold(query, m, src) {
            continue;
        }
        let mut call: Option<TsNode> = None;
        let mut member: Option<TsNode> = None;
        let mut recv: Option<TsNode> = None;
        let mut qualifier: Option<TsNode> = None;
        for cap in m.captures {
            match names[cap.index as usize] {
                "ref.call" => call = Some(cap.node),
                "ref.member" => member = Some(cap.node),
                "ref.recv" => recv = Some(cap.node),
                "ref.qualifier" => qualifier = Some(cap.node),
                "ref.ignore" => {
                    let s = span_of(cap.node);
                    ignored.push(((s.start_line, s.start_col), (s.end_line, s.end_col)));
                }
                _ => {}
            }
        }
        if let Some(n) = call {
            if let Ok(name) = n.utf8_text(src) {
                out.push(RefRec {
                    name: name.to_owned(),
                    kind: RefKind::Call,
                    site: span_of(n),
                    receiver: None,
                    qualifier: qualifier
                        .and_then(|q| q.utf8_text(src).ok())
                        .map(str::to_owned),
                });
            }
        } else if let Some(n) = member {
            if let Ok(name) = n.utf8_text(src) {
                out.push(RefRec {
                    name: name.to_owned(),
                    kind: RefKind::Member,
                    site: span_of(n),
                    receiver: Some(receiver_of(recv, src)),
                    qualifier: None,
                });
            }
        }
    }
    out.retain(|r| {
        let at = (r.site.start_line, r.site.start_col);
        !ignored.iter().any(|&(start, end)| at >= start && at <= end)
    });
    Ok(out)
}

fn extract_bindings(tree: &Tree, query: &Query, src: &[u8]) -> Result<Vec<BindRec>> {
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), src);

    let mut out = Vec::new();
    while let Some(m) = matches.next() {
        if !predicates_hold(query, m, src) {
            continue;
        }
        let mut name = None;
        let mut ty = None;
        for cap in m.captures {
            let text = cap.node.utf8_text(src).unwrap_or("").to_owned();
            match names[cap.index as usize] {
                "bind.name" => name = Some(text),
                "bind.ctor" | "bind.type" => ty = Some(text),
                _ => {}
            }
        }
        if let (Some(name), Some(type_name)) = (name, ty) {
            out.push(BindRec { name, type_name });
        }
    }
    Ok(out)
}

fn receiver_of(node: Option<TsNode>, src: &[u8]) -> Receiver {
    let Some(node) = node else {
        return Receiver::Other;
    };
    match node.kind() {
        "this" => Receiver::This,
        "identifier" => Receiver::Ident(node.utf8_text(src).unwrap_or("").to_owned()),
        "new_expression" => match node
            .child_by_field_name("constructor")
            .and_then(|c| c.utf8_text(src).ok())
        {
            Some(name) => Receiver::New(name.to_owned()),
            None => Receiver::Other,
        },
        _ => Receiver::Other,
    }
}

/// Evaluate a match's general predicates (`#eq?`, `#not-eq?`, `#any-of?`,
/// `#not-any-of?`). Required for grammars like Elixir where a `def`/`defmodule`
/// is a plain `call` node distinguished only by a predicate on the target.
/// `#match?`/regex predicates are not yet supported and are treated as passing.
fn predicates_hold(query: &Query, m: &QueryMatch, src: &[u8]) -> bool {
    let arg_text = |arg: &QueryPredicateArg| -> Option<String> {
        match arg {
            QueryPredicateArg::String(s) => Some(s.to_string()),
            QueryPredicateArg::Capture(id) => m
                .captures
                .iter()
                .find(|c| c.index == *id)
                .and_then(|c| c.node.utf8_text(src).ok())
                .map(str::to_owned),
        }
    };
    for pred in query.general_predicates(m.pattern_index) {
        match &*pred.operator {
            "eq?" | "not-eq?" => {
                if let [a, b] = &pred.args[..] {
                    let eq = arg_text(a).is_some() && arg_text(a) == arg_text(b);
                    if eq != (&*pred.operator == "eq?") {
                        return false;
                    }
                }
            }
            "any-of?" | "not-any-of?" => {
                if let [QueryPredicateArg::Capture(id), rest @ ..] = &pred.args[..] {
                    let v = m
                        .captures
                        .iter()
                        .find(|c| c.index == *id)
                        .and_then(|c| c.node.utf8_text(src).ok());
                    let found = rest.iter().any(
                        |a| matches!(a, QueryPredicateArg::String(s) if Some(s.as_ref()) == v),
                    );
                    if found != (&*pred.operator == "any-of?") {
                        return false;
                    }
                }
            }
            _ => {} // match?/not-match? (regex) — treat as passing for now
        }
    }
    true
}

fn span_of(node: TsNode) -> Span {
    let s = node.start_position();
    let e = node.end_position();
    Span {
        start_line: s.row as u32 + 1,
        start_col: s.column as u32 + 1,
        end_line: e.row as u32 + 1,
        end_col: e.column as u32 + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ir::NodeKind;

    fn ts() -> lang::typescript::Adapter {
        lang::typescript::Adapter::new()
    }

    #[test]
    fn extracts_definition_kinds() {
        let nodes = extract(
            "export function greet() {}\n\
             const helper = (x: number) => x;\n\
             export class Widget { increment() {} }\n\
             export interface Options {}\n\
             export type Id = string;\n\
             export enum Color { Red }\n",
            &ts(),
        )
        .unwrap();
        let names: Vec<_> = nodes.iter().map(|n| (n.kind, n.name.as_str())).collect();
        assert!(names.contains(&(NodeKind::Function, "greet")));
        assert!(names.contains(&(NodeKind::Variable, "helper")));
        assert!(names.contains(&(NodeKind::Class, "Widget")));
        assert!(names.contains(&(NodeKind::Method, "increment")));
        assert!(names.contains(&(NodeKind::Interface, "Options")));
        assert!(names.contains(&(NodeKind::Type, "Id")));
        assert!(names.contains(&(NodeKind::Enum, "Color")));
    }

    #[test]
    fn methods_are_qualified_by_class() {
        let nodes = extract("class A { foo() {} }\nclass B { foo() {} }\n", &ts()).unwrap();
        let qns: Vec<_> = nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Method)
            .map(|n| n.qualified_name.as_str())
            .collect();
        assert!(qns.contains(&"A.foo"));
        assert!(qns.contains(&"B.foo"));
        // distinct ids despite same method name
        let a = nodes.iter().find(|n| n.qualified_name == "A.foo").unwrap();
        let b = nodes.iter().find(|n| n.qualified_name == "B.foo").unwrap();
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn extracts_member_receivers_and_bindings() {
        let adapter = ts();
        let queries = Queries::compile(&adapter).unwrap();
        let fx = extract_file(
            "class Bar { foo() {} }\n\
             function run(b: Bar) {\n\
               const c = new Bar();\n\
               b.foo();\n\
               c.foo();\n\
               new Bar().foo();\n\
               this.foo();\n\
             }\n",
            &adapter,
            "m.ts",
            &queries,
        )
        .unwrap();
        let members: Vec<_> = fx
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::Member)
            .map(|r| (r.name.as_str(), r.receiver.clone().unwrap()))
            .collect();
        assert!(members.contains(&("foo", Receiver::Ident("b".into()))));
        assert!(members.contains(&("foo", Receiver::New("Bar".into()))));
        assert!(members.contains(&("foo", Receiver::This)));
        assert!(fx
            .bindings
            .iter()
            .any(|b| b.name == "b" && b.type_name == "Bar"));
        assert!(fx
            .bindings
            .iter()
            .any(|b| b.name == "c" && b.type_name == "Bar"));
    }
}
