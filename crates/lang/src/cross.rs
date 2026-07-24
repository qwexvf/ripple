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
    /// `import_fields(:other)` — (importing scope, included scope). Absinthe
    /// schemas normally declare root fields in `object :x_queries` blocks and
    /// pull them into `query do` this way, so the includes must be followed to
    /// know which fields are root fields. Resolved at link time because the
    /// included object usually lives in another file.
    pub scope_includes: Vec<(String, String)>,
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
            let mut sc = set.walk();
            for sel in set.named_children(&mut sc) {
                if sel.kind() != "selection" {
                    continue;
                }
                if let Some(field) = sel.named_child(0).filter(|f| f.kind() == "field") {
                    let mut fc = field.walk();
                    let name = field
                        .named_children(&mut fc)
                        .find(|n| n.kind() == "name")
                        .map(|n| text(n, src).to_owned());
                    if let Some(field) = name {
                        out.push(GqlOp {
                            name: op.clone(),
                            scope: scope.to_owned(),
                            field,
                        });
                    }
                }
            }
        }
    }
    let mut c = node.walk();
    for ch in node.children(&mut c) {
        collect_gql(ch, src, out);
    }
}

// ── Elixir: aliases, Absinthe fields, schema decls, remote calls, Ecto refs ──
pub fn elixir(root: TsNode, src: &[u8]) -> CrossFacts {
    let mut f = ElixirFacts::default();
    walk_elixir(root, src, None, &mut f);
    // Resolve module expressions to FQNs *here* (a second pass, once the whole
    // alias table is collected) so the `resolve` layer never touches Elixir
    // alias semantics — it just joins on FQNs.
    for field in &mut f.fields {
        field.module = resolve_module(&field.module, &f.aliases);
    }
    for (module, _, _) in &mut f.remote_calls {
        *module = resolve_module(module, &f.aliases);
    }
    for (module, _) in &mut f.schema_refs {
        *module = resolve_module(module, &f.aliases);
    }
    CrossFacts {
        elixir: Some(f),
        ..Default::default()
    }
}

/// `scope` is the Absinthe block enclosing `node` (see `AbsintheField::scope`),
/// `None` outside any such block.
fn walk_elixir(node: TsNode, src: &[u8], scope: Option<&str>, facts: &mut ElixirFacts) {
    match node.kind() {
        "call" => {
            if let Some(target) = node.child_by_field_name("target") {
                match target.kind() {
                    "identifier" => match text(target, src) {
                        "alias" => collect_alias(node, src, facts),
                        "schema" => {
                            if first_arg(node).is_some_and(|a| a.kind() == "string") {
                                facts.is_schema = true;
                            }
                        }
                        "field" => collect_field(node, src, scope, facts),
                        "import_fields" => collect_import_fields(node, src, scope, facts),
                        "from" => collect_from(node, src, facts),
                        _ => {}
                    },
                    "dot" => {
                        if let (Some(l), Some(r)) = (
                            target.child_by_field_name("left"),
                            target.child_by_field_name("right"),
                        ) {
                            if l.kind() == "alias" && r.kind() == "identifier" {
                                let module = text(l, src).to_owned();
                                let line = node.start_position().row as u32 + 1;
                                facts.remote_calls.push((
                                    module.clone(),
                                    text(r, src).to_owned(),
                                    line,
                                ));
                                if module == "Repo" {
                                    if let Some(a) = first_arg(node) {
                                        if a.kind() == "alias" {
                                            facts.schema_refs.push((text(a, src).to_owned(), line));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        "struct" => {
            if let Some(a) = node.named_child(0) {
                if a.kind() == "alias" {
                    facts.schema_refs.push((
                        text(a, src).to_owned(),
                        node.start_position().row as u32 + 1,
                    ));
                }
            }
        }
        _ => {}
    }
    let entered = absinthe_scope(node, src);
    let inner = entered.as_deref().or(scope);
    let mut c = node.walk();
    for child in node.children(&mut c) {
        walk_elixir(child, src, inner, facts);
    }
}

/// The Absinthe scope a `do`-block call opens, if any: `query do` / `mutation do` /
/// `subscription do` (no arguments) are the root scopes; `object :player do` and
/// friends open a type scope. Prefixed so a literal `object :query` can't be
/// mistaken for the root query.
fn absinthe_scope(node: TsNode, src: &[u8]) -> Option<String> {
    if node.kind() != "call" {
        return None;
    }
    let target = node.child_by_field_name("target")?;
    if target.kind() != "identifier" {
        return None;
    }
    let name = text(target, src);
    let mut c = node.walk();
    if !node.children(&mut c).any(|ch| ch.kind() == "do_block") {
        return None;
    }
    if GQL_ROOT_SCOPES.contains(&name) && args_node(node).is_none() {
        return Some(name.to_owned());
    }
    if matches!(name, "object" | "input_object" | "interface" | "union") {
        let atom = first_arg(node).filter(|a| a.kind() == "atom")?;
        return Some(format!(
            "object:{}",
            text(atom, src).trim_start_matches(':')
        ));
    }
    None
}

fn args_node(call: TsNode) -> Option<TsNode> {
    let mut c = call.walk();
    // bound to a local so the cursor's borrow ends before returning
    let found = call.children(&mut c).find(|n| n.kind() == "arguments");
    found
}

/// Resolve an Elixir module expression (possibly an alias local name) to a FQN,
/// using a file's alias table. Elixir-specific, so it lives in `lang`.
pub fn resolve_module(expr: &str, aliases: &HashMap<String, String>) -> String {
    if let Some(fqn) = aliases.get(expr) {
        return fqn.clone();
    }
    if let Some((head, rest)) = expr.split_once('.') {
        if let Some(fqn) = aliases.get(head) {
            return format!("{fqn}.{rest}");
        }
    }
    expr.to_owned()
}

fn first_arg(call: TsNode) -> Option<TsNode> {
    args_node(call)?.named_child(0)
}

fn collect_alias(call: TsNode, src: &[u8], facts: &mut ElixirFacts) {
    let Some(arg) = first_arg(call) else { return };
    match arg.kind() {
        "alias" => {
            let fqn = text(arg, src);
            if let Some(last) = fqn.rsplit('.').next() {
                facts.aliases.insert(last.to_owned(), fqn.to_owned());
            }
        }
        "dot" => {
            let (Some(l), Some(r)) = (
                arg.child_by_field_name("left"),
                arg.child_by_field_name("right"),
            ) else {
                return;
            };
            let prefix = text(l, src);
            if r.kind() == "tuple" {
                let mut c = r.walk();
                for t in r.named_children(&mut c) {
                    if t.kind() == "alias" {
                        let name = text(t, src);
                        facts
                            .aliases
                            .insert(name.to_owned(), format!("{prefix}.{name}"));
                    }
                }
            }
        }
        _ => {}
    }
}

fn collect_field(call: TsNode, src: &[u8], scope: Option<&str>, facts: &mut ElixirFacts) {
    // a `field` outside any Absinthe block is something else (e.g. an Ecto
    // `schema` column) — no scope, no join key, so don't guess one
    let Some(scope) = scope else { return };
    let Some(atom) = first_arg(call) else { return };
    if atom.kind() != "atom" {
        return;
    }
    let field = camelize(text(atom, src).trim_start_matches(':'));
    if let Some((module, func)) = find_resolve(call, src) {
        facts.fields.push(AbsintheField {
            scope: scope.to_owned(),
            field,
            module,
            func,
        });
    }
}

fn collect_import_fields(call: TsNode, src: &[u8], scope: Option<&str>, facts: &mut ElixirFacts) {
    let (Some(scope), Some(atom)) = (scope, first_arg(call)) else {
        return;
    };
    if atom.kind() != "atom" {
        return;
    }
    let included = format!("object:{}", text(atom, src).trim_start_matches(':'));
    facts.scope_includes.push((scope.to_owned(), included));
}

/// The resolver a `field` declares, in either Absinthe spelling:
/// `field :x, :t do resolve(&M.f/3) end` or `field :x, :t, resolve: &M.f/3`.
/// `None` when the resolver isn't a plain module function (`dataloader(...)`, an
/// inline `fn`) — under-linking beats inventing an edge.
fn find_resolve(call: TsNode, src: &[u8]) -> Option<(String, String)> {
    let mut stack = vec![call];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "call"
                if n.child_by_field_name("target")
                    .is_some_and(|t| t.kind() == "identifier" && text(t, src) == "resolve") =>
            {
                return capture_mod_func(n, src);
            }
            "pair"
                if n.child_by_field_name("key")
                    .is_some_and(|k| text(k, src).trim().trim_end_matches(':') == "resolve") =>
            {
                return capture_mod_func(n.child_by_field_name("value")?, src);
            }
            _ => {}
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    None
}

fn capture_mod_func(node: TsNode, src: &[u8]) -> Option<(String, String)> {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        // an inline `fn ... end` resolver has no single named resolver function;
        // whatever it calls is picked up as a remote call instead
        if n.kind() == "anonymous_function" {
            continue;
        }
        if n.kind() == "dot" {
            if let (Some(l), Some(r)) = (
                n.child_by_field_name("left"),
                n.child_by_field_name("right"),
            ) {
                if l.kind() == "alias" && r.kind() == "identifier" {
                    return Some((text(l, src).to_owned(), text(r, src).to_owned()));
                }
            }
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    None
}

fn collect_from(call: TsNode, src: &[u8], facts: &mut ElixirFacts) {
    let Some(arg) = first_arg(call) else { return };
    if arg.kind() == "binary_operator" {
        if let Some(r) = arg.child_by_field_name("right") {
            if r.kind() == "alias" {
                facts.schema_refs.push((
                    text(r, src).to_owned(),
                    call.start_position().row as u32 + 1,
                ));
            }
        }
    }
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

    #[test]
    fn resolve_module_via_alias() {
        let mut al = HashMap::new();
        al.insert(
            "PlayerResolver".to_string(),
            "App.Resolvers.PlayerResolver".to_string(),
        );
        assert_eq!(
            resolve_module("PlayerResolver", &al),
            "App.Resolvers.PlayerResolver"
        );
        assert_eq!(resolve_module("Unknown", &al), "Unknown");
    }

    fn elixir_facts_of(src: &str) -> ElixirFacts {
        let t = parse(crate::elixir::Adapter::new().grammar(), src);
        elixir(t.root_node(), src.as_bytes()).elixir.unwrap()
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
        assert_eq!(
            ops,
            vec![
                GqlOp {
                    name: "Player".into(),
                    scope: "query".into(),
                    field: "player".into()
                },
                GqlOp {
                    name: "Follow".into(),
                    scope: "mutation".into(),
                    field: "followPlayer".into()
                },
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
