//! Cross-service facts extracted from a file's AST at parse time (tree-sitter,
//! no regex). Stored on `FileExtract` so the index parses each file once; the
//! `resolve` layer only matches/links these facts. See docs/10-cross-service-resolution.md.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tree_sitter::Node as TsNode;

/// Per-file cross-service facts. Each language fills the parts it produces.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CrossFacts {
    pub elixir: Option<ElixirFacts>,
    /// GraphQL root fields requested by the operations in a `.gql` file.
    pub gql_ops: Vec<GqlOp>,
    /// TypeScript `<Name>Document` operation names referenced in the file.
    pub ts_docs: Vec<String>,
}

impl CrossFacts {
    pub fn is_empty(&self) -> bool {
        self.elixir.is_none() && self.gql_ops.is_empty() && self.ts_docs.is_empty()
    }
}

/// One root field of one GraphQL operation — the consumer side of the join.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GqlOp {
    /// Operation name (`query GetPlayer` → `GetPlayer`); codegen turns this into
    /// `GetPlayerDocument`, which is what `ts_docs` sees.
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

/// A field served by a *context module* rather than a named function —
/// `resolve: dataloader(App.Teams)`. 138 of 142 dataloader resolvers on one real
/// schema, so leaving them out left most type-level fields unreachable; but no single
/// function is named, so the honest target is the module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbsintheContextField {
    pub scope: String,
    pub field: String,
    /// Context module FQN — alias-resolved at extraction.
    pub module: String,
    pub returns: Option<String>,
}

/// An Absinthe `field` with a resolver — the producer side of the join.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbsintheField {
    /// Enclosing block: `query`/`mutation`/`subscription` for a root operation
    /// field, `object:<name>` for a field on a named type. Field names are only
    /// unique *within* a type, so the scope must be part of the join key —
    /// keying on the field name alone lets `Player.name` and `Team.name` collide.
    pub scope: String,
    /// camelCase field name (Absinthe `LanguageConventions` default).
    pub field: String,
    /// Resolver module FQN — alias-resolved at extraction.
    pub module: String,
    pub func: String,
    /// The type this field returns, as the schema names it (`:player`,
    /// `list_of(:lfg_post)` → `lfg_post`). What makes descending a nested selection
    /// possible: the parent field's type is the scope its children are declared in.
    /// `None` when the type isn't a plain atom — under-link rather than guess.
    pub returns: Option<String>,
}

/// Absinthe root scopes. A GraphQL document's root field can only name a field
/// declared in (or imported into) one of these.
pub const GQL_ROOT_SCOPES: [&str; 3] = ["query", "mutation", "subscription"];

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ElixirFacts {
    /// local alias name → module FQN
    pub aliases: HashMap<String, String>,
    /// the file declares a DB entity (`schema "table"`)
    pub is_schema: bool,
    /// Absinthe fields that declare a resolver.
    pub fields: Vec<AbsintheField>,
    /// Absinthe fields served by a context module (`resolve: dataloader(Mod)`).
    pub context_fields: Vec<AbsintheContextField>,
    /// `import_fields(:other)` — (importing scope, included scope). Absinthe
    /// schemas normally declare root fields in `object :x_queries` blocks and
    /// pull them into `query do` this way, so the includes must be followed to
    /// know which fields are root fields. Resolved at link time because the
    /// included object usually lives in another file.
    pub scope_includes: Vec<(String, String)>,
    /// `import Mod` — module FQNs whose functions this file may call *unqualified*.
    /// Elixir's `import` is why a bare call can cross a module boundary at all.
    pub imports: Vec<String>,
    /// remote calls (target module **FQN**, func, line)
    pub remote_calls: Vec<(String, String, u32)>,
    /// DB entity references (entity module **FQN**, line)
    pub schema_refs: Vec<(String, u32)>,
}

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
                    docs.insert(op.to_owned());
                }
            }
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    let mut ts_docs: Vec<String> = docs.into_iter().collect();
    ts_docs.sort();
    CrossFacts {
        ts_docs,
        ..Default::default()
    }
}

// ── GraphQL: operation → root fields ──
pub fn graphql(root: TsNode, src: &[u8]) -> CrossFacts {
    let mut gql_ops = Vec::new();
    collect_gql(root, src, &mut gql_ops);
    CrossFacts {
        gql_ops,
        ..Default::default()
    }
}

/// Walk a selection set, emitting one `GqlOp` per selected field with its full path.
///
/// Recursive because a nested selection is a field on another type, with its own
/// resolver — flattening to the root field is what made those resolvers unreachable.
fn collect_selections(
    set: TsNode,
    src: &[u8],
    op: &str,
    scope: &str,
    path: &mut Vec<String>,
    out: &mut Vec<GqlOp>,
) {
    let mut sc = set.walk();
    for sel in set.named_children(&mut sc) {
        if sel.kind() != "selection" {
            continue;
        }
        let Some(field) = sel.named_child(0).filter(|f| f.kind() == "field") else {
            continue; // a fragment spread names no field of its own
        };
        let mut fc = field.walk();
        let children: Vec<TsNode> = field.named_children(&mut fc).collect();
        let Some(name) = children
            .iter()
            .find(|n| n.kind() == "name")
            .map(|n| text(*n, src).to_owned())
        else {
            continue;
        };
        path.push(name.clone());
        out.push(GqlOp {
            name: op.to_owned(),
            scope: scope.to_owned(),
            field: name,
            path: path.clone(),
        });
        for nested in children.iter().filter(|n| n.kind() == "selection_set") {
            collect_selections(*nested, src, op, scope, path, out);
        }
        path.pop();
    }
}

fn collect_gql(node: TsNode, src: &[u8], out: &mut Vec<GqlOp>) {
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
            collect_selections(set, src, &op, scope, &mut Vec::new(), out);
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

    fn elixir_facts_of(src: &str) -> ElixirFacts {
        let t = parse(crate::elixir::Adapter::new().grammar(), src);
        elixir(t.root_node(), src.as_bytes()).elixir.unwrap()
    }

    /// Every entity argument counts as a reference, not just the first: a joined
    /// table and a preloaded association are as much a dependency as the primary
    /// one. Non-entity modules ride along harmlessly — the link step keeps only
    /// modules that actually declared a `schema "table"`.
    #[test]
    fn data_refs_cover_every_entity_argument() {
        let f = elixir_facts_of("defmodule S do\n  def q(id) do\n    from p in Player, join: t in Team, where: p.id == ^id\n    Repo.preload(p, Game)\n    Repo.get(Player, id)\n  end\nend\n");
        let refs: Vec<&str> = f.schema_refs.iter().map(|(m, _)| m.as_str()).collect();
        for entity in ["Player", "Team", "Game"] {
            assert!(refs.contains(&entity), "missing {entity} in {refs:?}");
        }
    }

    #[test]
    fn elixir_facts() {
        let f = elixir_facts_of("defmodule S do\n  alias App.Resolvers.PlayerResolver\n  query do\n    field :current_player, :player do\n      resolve(&PlayerResolver.me/3)\n    end\n  end\n  def show(a) do\n    Players.get_player(a)\n    Repo.get(Player, a)\n    from p in Team\n    %Role{}\n  end\nend\n");
        // fields carry the RESOLVED FQN (alias expanded at extraction), not the alias
        assert_eq!(
            f.fields,
            vec![AbsintheField {
                scope: "query".into(),
                field: "currentPlayer".into(),
                module: "App.Resolvers.PlayerResolver".into(),
                func: "me".into(),
                returns: Some("player".into()),
            }]
        );
        assert!(f
            .remote_calls
            .iter()
            .any(|(m, fu, _)| m == "Players" && fu == "get_player"));
        let schemas: Vec<&str> = f.schema_refs.iter().map(|(m, _)| m.as_str()).collect();
        assert!(
            schemas.contains(&"Player") && schemas.contains(&"Team") && schemas.contains(&"Role")
        );
    }

    /// `field :x, :t, resolve: &M.f/3` — the keyword spelling, previously missed.
    #[test]
    fn absinthe_keyword_form_resolve() {
        let f = elixir_facts_of("defmodule S do\n  mutation do\n    field :follow_player, :player, resolve: &PlayerResolver.follow/3\n  end\nend\n");
        assert_eq!(
            f.fields,
            vec![AbsintheField {
                scope: "mutation".into(),
                field: "followPlayer".into(),
                module: "PlayerResolver".into(),
                func: "follow".into(),
                returns: Some("player".into()),
            }]
        );
    }

    /// Same field name on two types must stay distinguishable by scope.
    #[test]
    fn absinthe_scope_separates_same_named_fields() {
        let f = elixir_facts_of("defmodule S do\n  query do\n    field :name, :string, resolve: &Root.name/3\n  end\n  object :player do\n    field :name, :string, resolve: &Player.name/3\n  end\nend\n");
        let scopes: Vec<(&str, &str)> = f
            .fields
            .iter()
            .map(|a| (a.scope.as_str(), a.module.as_str()))
            .collect();
        assert_eq!(scopes, vec![("query", "Root"), ("object:player", "Player")]);
    }

    #[test]
    fn absinthe_import_fields() {
        let f = elixir_facts_of("defmodule S do\n  query do\n    import_fields(:player_queries)\n  end\n  mutation do\n    import_fields :player_mutations\n  end\nend\n");
        assert_eq!(
            f.scope_includes,
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
        assert!(f.fields.is_empty(), "unexpected fields: {:?}", f.fields);
        // the inline fn's body is still visible as a remote call
        assert!(f
            .remote_calls
            .iter()
            .any(|(m, fu, _)| m == "Stats" && fu == "rank"));
    }

    #[test]
    fn elixir_schema_decl() {
        let src = "defmodule Player do\n  use Ecto.Schema\n  schema \"players\" do\n  end\nend\n";
        let t = parse(crate::elixir::Adapter::new().grammar(), src);
        assert!(
            elixir(t.root_node(), src.as_bytes())
                .elixir
                .unwrap()
                .is_schema
        );
    }

    #[test]
    fn gql_facts() {
        let src = "query Player($id: ID) { player(playerId: $id) { name } }\nmutation Follow { followPlayer { id } }\n";
        let t = parse(crate::graphql_language(), src);
        let ops = graphql(t.root_node(), src.as_bytes()).gql_ops;
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
        let docs = typescript(t.root_node(), src.as_bytes()).ts_docs;
        assert!(docs.contains(&"Player".to_string()) && docs.contains(&"Team".to_string()));
    }
}
