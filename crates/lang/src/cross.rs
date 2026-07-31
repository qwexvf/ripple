//! Cross-service facts extracted from a file's AST at parse time (tree-sitter,
//! no regex). Stored on `FileExtract` so the index parses each file once; the
//! `resolve` layer only matches/links these facts. See docs/10-cross-service-resolution.md.

use ir::{RouteKey, Segment, Transport};
use serde::{Deserialize, Serialize};
use tree_sitter::Node as TsNode;

/// Per-file cross-service facts, in the vocabulary every detector maps onto.
///
/// Nothing here names a framework. A detector reads its own framework's shapes and
/// emits `Provides`/`Consumes` keyed by `RouteKey`; the linker matches keys and
/// never learns what produced them. That is the whole point of #32 — Absinthe used
/// to be spelled out in `resolve`, which made adding a second framework a core
/// change rather than a file.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CrossFacts {
    /// Boundary endpoints this file serves: a resolver, a route handler, a topic
    /// subscriber.
    pub provides: Vec<Provides>,
    /// Boundary endpoints this file calls. Empty until an HTTP/RPC consumer
    /// detector lands (#32 phase 3) — GraphQL consumers travel as `graphql`
    /// because a document is a protocol object, not a single key.
    pub consumes: Vec<Consumes>,
    /// GraphQL protocol facts: documents, fragments, scope includes, references.
    pub graphql: GraphqlFacts,
    /// Modules whose functions this file may call *unqualified* (Elixir's
    /// `import`). Generic because the shape is: "names from over there are in
    /// scope here".
    pub star_imports: Vec<String>,
    /// Calls that name their target module: (module FQN, function, line).
    pub qualified_calls: Vec<(String, String, u32)>,
    /// References to a persisted entity: (entity module FQN, line).
    pub entity_refs: Vec<(String, u32)>,
    /// This file declares a persisted entity (an Ecto `schema`, an ORM model).
    pub entity_def: bool,
}

impl CrossFacts {
    pub fn is_empty(&self) -> bool {
        self.provides.is_empty()
            && self.consumes.is_empty()
            && self.graphql.is_empty()
            && self.star_imports.is_empty()
            && self.qualified_calls.is_empty()
            && self.entity_refs.is_empty()
            && !self.entity_def
    }
}

/// What serves a boundary key: a named function, or — when the framework names no
/// single function — the module that answers for it. Module granularity is worth
/// less than a function and is priced that way, but it is not nothing: 138 of 142
/// dataloader resolvers on one real schema name only a module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HandlerRef {
    Function { module: String, name: String },
    Module(String),
}

/// One endpoint this file serves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provides {
    pub key: RouteKey,
    pub handler: HandlerRef,
    /// For transports with a type graph (GraphQL): what this field returns, as the
    /// schema spells it, so a nested selection can be descended. `None` elsewhere.
    pub returns: Option<String>,
}

/// One endpoint this file calls.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Consumes {
    pub key: RouteKey,
    pub line: u32,
    /// How much of the key was literal. A `fetch(`/api/${id}`)` pins less than a
    /// fully spelled path, and the linker prices the edge accordingly.
    pub confidence_hint: f32,
}

/// GraphQL travels as a protocol rather than as bare keys: a document names
/// operations, operations spread fragments, and a schema pulls fields between
/// scopes. All of it is wire-format — no framework names.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct GraphqlFacts {
    /// Root fields requested by the operations in a document.
    pub operations: Vec<GqlOp>,
    /// Fragment definitions.
    pub fragments: Vec<GqlFragment>,
    /// Fragment spreads inside operations. Most nested selections in a
    /// codegen-based app are written in fragments, so an unexpanded spread hides
    /// them all.
    pub spreads: Vec<GqlSpread>,
    /// `(importing scope, included scope)` — a schema declaring root fields in one
    /// block and pulling them into another. Resolved at link time because the
    /// included block usually lives in another file.
    pub scope_includes: Vec<(String, String)>,
    /// Operation names referenced from code (codegen's `<Name>Document`).
    pub op_refs: Vec<String>,
}

impl GraphqlFacts {
    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
            && self.fragments.is_empty()
            && self.spreads.is_empty()
            && self.scope_includes.is_empty()
            && self.op_refs.is_empty()
    }
}

/// The wire spelling of an operation name.
///
/// Codegen writes `query currentPlayer` as `CurrentPlayerDocument`, so the document
/// and the code that references it disagree on the first letter. Both sides
/// normalize here, at extraction — the linker compares wire names and never learns
/// that a casing convention exists (#32). Keying on the raw name lost 11 of 242
/// operations on one real frontend.
pub fn operation_key(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// The key a GraphQL field is served and requested under: its scope, then its
/// name. One function so the producer and the consumer cannot spell it differently
/// — they used to be two tuple literals in two crates.
pub fn graphql_field_key(scope: &str, field: &str) -> RouteKey {
    RouteKey {
        transport: Transport::Graphql,
        method: None,
        path: vec![
            Segment::Literal(scope.to_owned()),
            Segment::Literal(field.to_owned()),
        ],
    }
}

/// Read a GraphQL key back as `(scope, field)`. `None` for any other shape.
pub fn graphql_scope_field(key: &RouteKey) -> Option<(&str, &str)> {
    match (key.transport, key.path.as_slice()) {
        (Transport::Graphql, [Segment::Literal(scope), Segment::Literal(field)]) => {
            Some((scope, field))
        }
        _ => None,
    }
}

/// One root field of one GraphQL operation — the consumer side of the join.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GqlOp {
    /// Operation name in wire spelling (`operation_key`), so it matches the name
    /// the consuming code references however the document spelled it.
    pub name: String,
    /// Root scope the field is selected on: `query` | `mutation` | `subscription`.
    /// Part of the join key — a `player` mutation must not match a `player` query.
    pub scope: String,
    /// Root field name, as written in the document (camelCase).
    pub field: String,
    /// Field names from the root down to this selection, root first. `["lfgPosts",
    /// "author"]` for `lfgPosts { author { … } }`. A nested selection has a resolver
    /// too, and matching only the root field missed every one of them.
    pub path: Vec<String>,
}

/// A fragment definition: a named, reusable selection on one type.
///
/// `fragment LfgPostFields on LfgPost { author { name } }`. The type condition names
/// the scope its fields live in directly, so an expanded spread needs no descent —
/// which is why fragments are cheaper to follow than nested selections were.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GqlFragment {
    pub name: String,
    /// The type it applies to, as written (`LfgPost`).
    pub type_condition: String,
    /// Field paths selected inside it, relative to the type condition.
    pub paths: Vec<Vec<String>>,
    /// Fragments it spreads in turn, with the path each occurs at.
    pub spreads: Vec<(Vec<String>, String)>,
}

/// A `...FragmentName` in an operation, and where in the selection it appears.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GqlSpread {
    /// Operation name the spread belongs to.
    pub op: String,
    /// Root scope of that operation: `query` | `mutation` | `subscription`.
    pub scope: String,
    /// Selection path the spread sits at, root first. Empty = spread at the top level.
    pub at: Vec<String>,
    pub fragment: String,
}

/// Root scopes a GraphQL document's operation can name. A root field must be
/// declared in (or imported into) one of these.
pub const GQL_ROOT_SCOPES: [&str; 3] = ["query", "mutation", "subscription"];

fn text<'a>(n: TsNode, src: &'a [u8]) -> &'a str {
    n.utf8_text(src).unwrap_or("")
}

// ── TypeScript: <Name>Document usages ──
pub fn typescript(root: TsNode, src: &[u8]) -> CrossFacts {
    let mut docs = std::collections::HashSet::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "identifier" {
            if let Some(op) = text(n, src).strip_suffix("Document") {
                if !op.is_empty() {
                    docs.insert(operation_key(op));
                }
            }
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    let mut op_refs: Vec<String> = docs.into_iter().collect();
    op_refs.sort();
    CrossFacts {
        graphql: GraphqlFacts {
            op_refs,
            ..Default::default()
        },
        ..Default::default()
    }
}

// ── GraphQL: operation → root fields ──
pub fn graphql(root: TsNode, src: &[u8]) -> CrossFacts {
    let mut facts = CrossFacts::default();
    collect_gql(root, src, &mut facts);
    facts
}

/// Where a selection walk puts what it finds. Operations and fragment bodies are the
/// same shape of walk with different destinations, so they share one walker.
enum Sink<'a> {
    Operation {
        op: &'a str,
        scope: &'a str,
        ops: &'a mut Vec<GqlOp>,
        spreads: &'a mut Vec<GqlSpread>,
    },
    Fragment {
        paths: &'a mut Vec<Vec<String>>,
        spreads: &'a mut Vec<(Vec<String>, String)>,
    },
}

impl Sink<'_> {
    fn field(&mut self, path: &[String], name: &str) {
        match self {
            Sink::Operation { op, scope, ops, .. } => ops.push(GqlOp {
                name: operation_key(op),
                scope: (*scope).to_owned(),
                field: name.to_owned(),
                path: path.to_vec(),
            }),
            Sink::Fragment { paths, .. } => paths.push(path.to_vec()),
        }
    }

    fn spread(&mut self, path: &[String], fragment: &str) {
        match self {
            Sink::Operation {
                op, scope, spreads, ..
            } => spreads.push(GqlSpread {
                op: operation_key(op),
                scope: (*scope).to_owned(),
                at: path.to_vec(),
                fragment: fragment.to_owned(),
            }),
            Sink::Fragment { spreads, .. } => spreads.push((path.to_vec(), fragment.to_owned())),
        }
    }
}

/// Walk a selection set, emitting one `GqlOp` per selected field with its full path.
///
/// Recursive because a nested selection is a field on another type, with its own
/// resolver — flattening to the root field is what made those resolvers unreachable.
fn collect_selections(set: TsNode, src: &[u8], path: &mut Vec<String>, sink: &mut Sink) {
    let mut sc = set.walk();
    for sel in set.named_children(&mut sc) {
        if sel.kind() != "selection" {
            continue;
        }
        let Some(inner) = sel.named_child(0) else {
            continue;
        };
        match inner.kind() {
            // `...Name` — the fields are in the fragment, resolved at link time
            "fragment_spread" => {
                if let Some(name) = named_child_text(inner, "fragment_name", src) {
                    sink.spread(path, &name);
                }
            }
            "field" => {
                let mut fc = inner.walk();
                let children: Vec<TsNode> = inner.named_children(&mut fc).collect();
                let Some(name) = children
                    .iter()
                    .find(|n| n.kind() == "name")
                    .map(|n| text(*n, src).to_owned())
                else {
                    continue;
                };
                path.push(name.clone());
                sink.field(path, &name);
                for nested in children.iter().filter(|n| n.kind() == "selection_set") {
                    collect_selections(*nested, src, path, sink);
                }
                path.pop();
            }
            // an inline fragment (`... on Type { … }`) selects on a different type;
            // not followed yet, and skipped rather than mis-attributed to this one
            _ => {}
        }
    }
}

/// Text of the first named child of `kind`, one level down (`fragment_name (name)`).
fn named_child_text(node: TsNode, kind: &str, src: &[u8]) -> Option<String> {
    let mut c = node.walk();
    let child = node.named_children(&mut c).find(|n| n.kind() == kind)?;
    let mut cc = child.walk();
    let name = child
        .named_children(&mut cc)
        .find(|n| n.kind() == "name")
        .unwrap_or(child);
    Some(text(name, src).to_owned())
}

fn collect_gql(node: TsNode, src: &[u8], out: &mut CrossFacts) {
    if node.kind() == "fragment_definition" {
        let name = named_child_text(node, "fragment_name", src);
        let mut c = node.walk();
        let children: Vec<TsNode> = node.named_children(&mut c).collect();
        let type_condition = children
            .iter()
            .find(|n| n.kind() == "type_condition")
            .and_then(|n| named_child_text(*n, "named_type", src));
        let set = children.iter().find(|n| n.kind() == "selection_set");
        if let (Some(name), Some(type_condition), Some(set)) = (name, type_condition, set) {
            let mut fragment = GqlFragment {
                name,
                type_condition,
                paths: Vec::new(),
                spreads: Vec::new(),
            };
            let mut sink = Sink::Fragment {
                paths: &mut fragment.paths,
                spreads: &mut fragment.spreads,
            };
            collect_selections(*set, src, &mut Vec::new(), &mut sink);
            out.graphql.fragments.push(fragment);
        }
    }
    if node.kind() == "operation_definition" {
        let mut op_name = None;
        let mut sel_set = None;
        // shorthand (`{ field }`) has no operation_type and means query
        let mut scope = "query";
        let mut c = node.walk();
        for ch in node.children(&mut c) {
            match ch.kind() {
                "operation_type" => scope = text(ch, src),
                "name" if op_name.is_none() => op_name = Some(text(ch, src).to_owned()),
                "selection_set" => sel_set = Some(ch),
                _ => {}
            }
        }
        if let (Some(op), Some(set)) = (op_name, sel_set) {
            let mut sink = Sink::Operation {
                op: &op,
                scope,
                ops: &mut out.graphql.operations,
                spreads: &mut out.graphql.spreads,
            };
            collect_selections(set, src, &mut Vec::new(), &mut sink);
        }
    }
    let mut c = node.walk();
    for ch in node.children(&mut c) {
        collect_gql(ch, src, out);
    }
}

// ── Elixir: aliases, schema fields, entity decls, remote calls, data refs ──
/// Reads the file's macro shapes generically (`elixir::macros`), then projects
/// them onto these facts with the per-framework tables in `elixir::dsl` — no
/// framework name appears in the walker itself.
pub fn elixir(root: TsNode, src: &[u8]) -> CrossFacts {
    crate::elixir::dsl::cross_facts(&crate::elixir::macros::scan(root, src))
}

/// snake_case atom → camelCase (Absinthe LanguageConventions default).
/// The inverse of `camelize`: a GraphQL type name to the atom Absinthe declares it
/// under (`LfgPost` → `lfg_post`). A fragment's type condition is written the first
/// way and the schema's object scope the second, so a spread cannot be followed
/// without it.
pub fn decamelize(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

pub fn camelize(snake: &str) -> String {
    let mut out = String::with_capacity(snake.len());
    let mut upper = false;
    for (i, ch) in snake.chars().enumerate() {
        if ch == '_' {
            upper = true;
        } else if upper && i != 0 {
            out.extend(ch.to_uppercase());
            upper = false;
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::LanguageAdapter;
    use tree_sitter::Parser;

    fn parse(lang: tree_sitter::Language, src: &str) -> tree_sitter::Tree {
        let mut p = Parser::new();
        p.set_language(&lang).unwrap();
        p.parse(src, None).unwrap()
    }

    #[test]
    fn camelize_matches_absinthe() {
        assert_eq!(camelize("current_player"), "currentPlayer");
        assert_eq!(camelize("player"), "player");
    }

    fn elixir_facts_of(src: &str) -> CrossFacts {
        let t = parse(crate::elixir::Adapter::new().grammar(), src);
        elixir(t.root_node(), src.as_bytes())
    }

    /// `(scope, field, handler)` for every GraphQL field a file provides.
    fn provided(f: &CrossFacts) -> Vec<(&str, &str, &HandlerRef)> {
        f.provides
            .iter()
            .filter_map(|p| {
                let (scope, field) = graphql_scope_field(&p.key)?;
                Some((scope, field, &p.handler))
            })
            .collect()
    }

    fn function(module: &str, name: &str) -> HandlerRef {
        HandlerRef::Function {
            module: module.into(),
            name: name.into(),
        }
    }

    /// Every entity argument counts as a reference, not just the first: a joined
    /// table and a preloaded association are as much a dependency as the primary
    /// one. Non-entity modules ride along harmlessly — the link step keeps only
    /// modules that actually declared a `schema "table"`.
    #[test]
    fn data_refs_cover_every_entity_argument() {
        let f = elixir_facts_of("defmodule S do\n  def q(id) do\n    from p in Player, join: t in Team, where: p.id == ^id\n    Repo.preload(p, Game)\n    Repo.get(Player, id)\n  end\nend\n");
        let refs: Vec<&str> = f.entity_refs.iter().map(|(m, _)| m.as_str()).collect();
        for entity in ["Player", "Team", "Game"] {
            assert!(refs.contains(&entity), "missing {entity} in {refs:?}");
        }
    }

    #[test]
    fn elixir_facts() {
        let f = elixir_facts_of("defmodule S do\n  alias App.Resolvers.PlayerResolver\n  query do\n    field :current_player, :player do\n      resolve(&PlayerResolver.me/3)\n    end\n  end\n  def show(a) do\n    Players.get_player(a)\n    Repo.get(Player, a)\n    from p in Team\n    %Role{}\n  end\nend\n");
        // a provider carries the RESOLVED FQN (alias expanded at extraction)
        assert_eq!(
            provided(&f),
            vec![(
                "query",
                "currentPlayer",
                &function("App.Resolvers.PlayerResolver", "me")
            )]
        );
        assert_eq!(f.provides[0].returns.as_deref(), Some("player"));
        assert!(f
            .qualified_calls
            .iter()
            .any(|(m, fu, _)| m == "Players" && fu == "get_player"));
        let schemas: Vec<&str> = f.entity_refs.iter().map(|(m, _)| m.as_str()).collect();
        assert!(
            schemas.contains(&"Player") && schemas.contains(&"Team") && schemas.contains(&"Role")
        );
    }

    /// `field :x, :t, resolve: &M.f/3` — the keyword spelling, previously missed.
    #[test]
    fn absinthe_keyword_form_resolve() {
        let f = elixir_facts_of("defmodule S do\n  mutation do\n    field :follow_player, :player, resolve: &PlayerResolver.follow/3\n  end\nend\n");
        assert_eq!(
            provided(&f),
            vec![(
                "mutation",
                "followPlayer",
                &function("PlayerResolver", "follow")
            )]
        );
    }

    /// Same field name on two types must stay distinguishable by scope.
    #[test]
    fn absinthe_scope_separates_same_named_fields() {
        let f = elixir_facts_of("defmodule S do\n  query do\n    field :name, :string, resolve: &Root.name/3\n  end\n  object :player do\n    field :name, :string, resolve: &Player.name/3\n  end\nend\n");
        let scopes: Vec<(&str, &HandlerRef)> = provided(&f)
            .into_iter()
            .map(|(scope, _, handler)| (scope, handler))
            .collect();
        assert_eq!(
            scopes,
            vec![
                ("query", &function("Root", "name")),
                ("object:player", &function("Player", "name")),
            ]
        );
    }

    #[test]
    fn absinthe_import_fields() {
        let f = elixir_facts_of("defmodule S do\n  query do\n    import_fields(:player_queries)\n  end\n  mutation do\n    import_fields :player_mutations\n  end\nend\n");
        assert_eq!(
            f.graphql.scope_includes,
            vec![
                ("query".into(), "object:player_queries".into()),
                ("mutation".into(), "object:player_mutations".into()),
            ]
        );
    }

    /// `resolve: dataloader(M)` / `resolve: fn -> ... end` name no resolver
    /// function — under-link rather than invent one. Ecto columns aren't fields.
    #[test]
    fn absinthe_non_function_resolvers_are_dropped() {
        let f = elixir_facts_of("defmodule S do\n  object :player do\n    field :team, :team, resolve: dataloader(App.Teams)\n    field :rank, :integer, resolve: fn p, _, _ -> Stats.rank(p) end\n  end\n  schema \"players\" do\n    field :name, :string\n  end\nend\n");
        let named: Vec<_> = provided(&f)
            .into_iter()
            .filter(|(_, _, h)| matches!(h, HandlerRef::Function { .. }))
            .collect();
        assert!(named.is_empty(), "unexpected named resolvers: {named:?}");
        // the dataloader field is still served, one level coarser
        assert_eq!(
            provided(&f),
            vec![(
                "object:player",
                "team",
                &HandlerRef::Module("App.Teams".into())
            )]
        );
        // the inline fn's body is still visible as a qualified call
        assert!(f
            .qualified_calls
            .iter()
            .any(|(m, fu, _)| m == "Stats" && fu == "rank"));
    }

    #[test]
    fn elixir_schema_decl() {
        let src = "defmodule Player do\n  use Ecto.Schema\n  schema \"players\" do\n  end\nend\n";
        let t = parse(crate::elixir::Adapter::new().grammar(), src);
        assert!(elixir(t.root_node(), src.as_bytes()).entity_def);
    }

    #[test]
    fn gql_facts() {
        let src = "query Player($id: ID) { player(playerId: $id) { name } }\nmutation Follow { followPlayer { id } }\n";
        let t = parse(crate::graphql_language(), src);
        let ops = graphql(t.root_node(), src.as_bytes()).graphql.operations;
        // a nested selection is a field on another type with a resolver of its own, so
        // it is reported with the path that reaches it (issue #22)
        let seen: Vec<(&str, &str, Vec<&str>)> = ops
            .iter()
            .map(|o| {
                (
                    o.scope.as_str(),
                    o.field.as_str(),
                    o.path.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(
            seen,
            vec![
                ("query", "player", vec!["player"]),
                ("query", "name", vec!["player", "name"]),
                ("mutation", "followPlayer", vec!["followPlayer"]),
                ("mutation", "id", vec!["followPlayer", "id"]),
            ]
        );
    }

    #[test]
    fn ts_facts() {
        let src = "import { PlayerDocument, TeamDocument } from \"@/g\";\nuseQuery({query: PlayerDocument});\n";
        let t = parse(crate::typescript::Adapter::new().grammar(), src);
        let docs = typescript(t.root_node(), src.as_bytes()).graphql.op_refs;
        assert!(docs.contains(&"Player".to_string()) && docs.contains(&"Team".to_string()));
    }
}
