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
    /// GraphQL (operation name, root field) pairs from a `.gql` file.
    pub gql_ops: Vec<(String, String)>,
    /// TypeScript `<Name>Document` operation names referenced in the file.
    pub ts_docs: Vec<String>,
}

impl CrossFacts {
    pub fn is_empty(&self) -> bool {
        self.elixir.is_none() && self.gql_ops.is_empty() && self.ts_docs.is_empty()
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ElixirFacts {
    /// local alias name → module FQN
    pub aliases: HashMap<String, String>,
    /// the file declares a DB entity (`schema "table"`)
    pub is_schema: bool,
    /// Absinthe (camelCase root field, resolver module **FQN**, resolver func).
    /// The FQN is alias-resolved at extraction so `resolve` needs no alias logic.
    pub fields: Vec<(String, String, String)>,
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
    CrossFacts { ts_docs, ..Default::default() }
}

// ── GraphQL: operation → root fields ──
pub fn graphql(root: TsNode, src: &[u8]) -> CrossFacts {
    let mut gql_ops = Vec::new();
    collect_gql(root, src, &mut gql_ops);
    CrossFacts { gql_ops, ..Default::default() }
}

fn collect_gql(node: TsNode, src: &[u8], out: &mut Vec<(String, String)>) {
    if node.kind() == "operation_definition" {
        let mut op_name = None;
        let mut sel_set = None;
        let mut c = node.walk();
        for ch in node.children(&mut c) {
            match ch.kind() {
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
                    if let Some(name) = name {
                        out.push((op.clone(), name));
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
    walk_elixir(root, src, &mut f);
    // Resolve module expressions to FQNs *here* (a second pass, once the whole
    // alias table is collected) so the `resolve` layer never touches Elixir
    // alias semantics — it just joins on FQNs.
    for (_, module, _) in &mut f.fields {
        *module = resolve_module(module, &f.aliases);
    }
    for (module, _, _) in &mut f.remote_calls {
        *module = resolve_module(module, &f.aliases);
    }
    for (module, _) in &mut f.schema_refs {
        *module = resolve_module(module, &f.aliases);
    }
    CrossFacts { elixir: Some(f), ..Default::default() }
}

fn walk_elixir(node: TsNode, src: &[u8], facts: &mut ElixirFacts) {
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
                        "field" => collect_field(node, src, facts),
                        "from" => collect_from(node, src, facts),
                        _ => {}
                    },
                    "dot" => {
                        if let (Some(l), Some(r)) =
                            (target.child_by_field_name("left"), target.child_by_field_name("right"))
                        {
                            if l.kind() == "alias" && r.kind() == "identifier" {
                                let module = text(l, src).to_owned();
                                let line = node.start_position().row as u32 + 1;
                                facts.remote_calls.push((module.clone(), text(r, src).to_owned(), line));
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
                    facts.schema_refs
                        .push((text(a, src).to_owned(), node.start_position().row as u32 + 1));
                }
            }
        }
        _ => {}
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        walk_elixir(child, src, facts);
    }
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
            let (Some(l), Some(r)) =
                (arg.child_by_field_name("left"), arg.child_by_field_name("right"))
            else {
                return;
            };
            let prefix = text(l, src);
            if r.kind() == "tuple" {
                let mut c = r.walk();
                for t in r.named_children(&mut c) {
                    if t.kind() == "alias" {
                        let name = text(t, src);
                        facts.aliases.insert(name.to_owned(), format!("{prefix}.{name}"));
                    }
                }
            }
        }
        _ => {}
    }
}

fn collect_field(call: TsNode, src: &[u8], facts: &mut ElixirFacts) {
    let Some(atom) = first_arg(call) else { return };
    if atom.kind() != "atom" {
        return;
    }
    let field = camelize(text(atom, src).trim_start_matches(':'));
    if let Some((module, func)) = find_resolve(call, src) {
        facts.fields.push((field, module, func));
    }
}

fn find_resolve(call: TsNode, src: &[u8]) -> Option<(String, String)> {
    let mut stack = vec![call];
    while let Some(n) = stack.pop() {
        if n.kind() == "call" {
            if let Some(t) = n.child_by_field_name("target") {
                if t.kind() == "identifier" && text(t, src) == "resolve" {
                    return capture_mod_func(n, src);
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

fn capture_mod_func(node: TsNode, src: &[u8]) -> Option<(String, String)> {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "dot" {
            if let (Some(l), Some(r)) =
                (n.child_by_field_name("left"), n.child_by_field_name("right"))
            {
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
                facts.schema_refs
                    .push((text(r, src).to_owned(), call.start_position().row as u32 + 1));
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
        al.insert("PlayerResolver".to_string(), "App.Resolvers.PlayerResolver".to_string());
        assert_eq!(resolve_module("PlayerResolver", &al), "App.Resolvers.PlayerResolver");
        assert_eq!(resolve_module("Unknown", &al), "Unknown");
    }

    #[test]
    fn elixir_facts() {
        let src = "defmodule S do\n  alias App.Resolvers.PlayerResolver\n  field :current_player, :player do\n    resolve(&PlayerResolver.me/3)\n  end\n  def show(a) do\n    Players.get_player(a)\n    Repo.get(Player, a)\n    from p in Team\n    %Role{}\n  end\nend\n";
        let t = parse(crate::elixir::Adapter::new().grammar(), src);
        let f = elixir(t.root_node(), src.as_bytes()).elixir.unwrap();
        // fields carry the RESOLVED FQN (alias expanded at extraction), not the alias
        assert_eq!(f.fields, vec![("currentPlayer".into(), "App.Resolvers.PlayerResolver".into(), "me".into())]);
        assert!(f.remote_calls.iter().any(|(m, fu, _)| m == "Players" && fu == "get_player"));
        let schemas: Vec<&str> = f.schema_refs.iter().map(|(m, _)| m.as_str()).collect();
        assert!(schemas.contains(&"Player") && schemas.contains(&"Team") && schemas.contains(&"Role"));
    }

    #[test]
    fn elixir_schema_decl() {
        let src = "defmodule Player do\n  use Ecto.Schema\n  schema \"players\" do\n  end\nend\n";
        let t = parse(crate::elixir::Adapter::new().grammar(), src);
        assert!(elixir(t.root_node(), src.as_bytes()).elixir.unwrap().is_schema);
    }

    #[test]
    fn gql_facts() {
        let src = "query Player($id: ID) { player(playerId: $id) { name } }\nmutation Follow { followPlayer { id } }\n";
        let t = parse(crate::graphql_language(), src);
        let ops = graphql(t.root_node(), src.as_bytes()).gql_ops;
        assert!(ops.contains(&("Player".into(), "player".into())));
        assert!(ops.contains(&("Follow".into(), "followPlayer".into())));
    }

    #[test]
    fn ts_facts() {
        let src = "import { PlayerDocument, TeamDocument } from \"@/g\";\nuseQuery({query: PlayerDocument});\n";
        let t = parse(crate::typescript::Adapter::new().grammar(), src);
        let docs = typescript(t.root_node(), src.as_bytes()).ts_docs;
        assert!(docs.contains(&"Player".to_string()) && docs.contains(&"Team".to_string()));
    }
}
