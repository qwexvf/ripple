//! Which Elixir macro shapes carry cross-service meaning.
//!
//! [`super::macros`] reads macro *shape* without knowing any framework; this
//! module is the only place framework names appear. Supporting another DSL
//! (Ash, Phoenix router, LiveView) means adding a table here and projecting it —
//! not touching the walker. See docs/05-language-support.md.

use super::macros::{FunRef, MacroCall, Scan};
use crate::cross::{
    camelize, graphql_field_key, http_key, CrossFacts, HandlerRef, Provides, GQL_ROOT_SCOPES,
};
use std::collections::HashMap;

/// A GraphQL-schema DSL: nested type blocks whose members declare a resolver.
struct SchemaDsl {
    /// macros opening a named type block (`object :player do`)
    type_blocks: &'static [&'static str],
    /// blocks whose members are the schema's root fields
    root_blocks: &'static [&'static str],
    /// macro declaring a member of a block
    member: &'static str,
    /// macro pulling another block's members in (`import_fields(:player_queries)`)
    include: &'static str,
    /// macro (block form) or keyword (inline form) naming the resolver function
    resolver: &'static str,
}

const ABSINTHE: SchemaDsl = SchemaDsl {
    type_blocks: &["object", "input_object", "interface", "union"],
    root_blocks: &GQL_ROOT_SCOPES,
    member: "field",
    include: "import_fields",
    resolver: "resolve",
};

/// A data-layer DSL: what declares a persisted entity and what references one.
struct DataDsl {
    /// macro declaring an entity, taking the table name as a string
    entity: &'static str,
    /// last module segment whose calls take entities as arguments (`Repo.get(Player, id)`)
    repo: &'static str,
    /// query macros taking an entity (`from p in Player`)
    queries: &'static [&'static str],
}

/// A router DSL: verb macros declaring a path, a controller and an action, nested
/// in blocks that prefix the path.
struct RouterDsl {
    /// macros opening a block whose string argument prefixes the paths inside it
    prefix_blocks: &'static [&'static str],
    /// verb macros, spelled as the method they declare
    verbs: &'static [&'static str],
}

const PHOENIX: RouterDsl = RouterDsl {
    prefix_blocks: &["scope"],
    verbs: &["get", "post", "put", "patch", "delete", "head", "options"],
};

const ECTO: DataDsl = DataDsl {
    entity: "schema",
    repo: "Repo",
    queries: &["from"],
};

/// Project a generic macro scan onto the cross-service facts the resolve layer
/// joins on.
pub fn cross_facts(scan: &Scan) -> CrossFacts {
    let mut f = CrossFacts::default();
    schema_facts(scan, &ABSINTHE, &mut f);
    router_facts(scan, &PHOENIX, &mut f);
    data_facts(scan, &ECTO, &mut f);
    f.star_imports = scan
        .calls
        .iter()
        .filter(|c| c.name == "import")
        .flat_map(|c| c.modules.iter().cloned())
        .collect();
    f.qualified_calls = scan
        .remote_calls
        .iter()
        .map(|rc| (rc.module.clone(), rc.func.clone(), rc.line))
        .collect();
    f
}

fn schema_facts(scan: &Scan, dsl: &SchemaDsl, out: &mut CrossFacts) {
    // block-form resolvers are their own macro call inside the member's block, so
    // index them by the scope chain they sit in. A scope can hold more than one
    // (two `resolve` calls in one field): keep them all, because last-write-wins
    // silently dropped one and then reported the survivor as certain.
    let mut block_resolvers: HashMap<Vec<String>, Vec<&FunRef>> = HashMap::new();
    for call in scan.calls.iter().filter(|c| c.name == dsl.resolver) {
        if let Some(r) = call.fun_refs.first() {
            let found = block_resolvers.entry(call.scope.clone()).or_default();
            if !found.contains(&r) {
                found.push(r);
            }
        }
    }

    for call in &scan.calls {
        let Some(scope) = call.scope.last().and_then(|s| block_scope(s, dsl)) else {
            continue;
        };
        if call.name == dsl.include {
            if let Some(atom) = call.atoms.first() {
                out.graphql.scope_includes.push((scope, type_scope(atom)));
            }
            continue;
        }
        if call.name != dsl.member {
            continue;
        }
        let Some(atom) = call.atoms.first() else {
            continue;
        };
        // wire spelling, same reason as `type_scope`: the linker uses this as the
        // scope a nested selection descends into
        let returns = call
            .atoms
            .get(1)
            .or_else(|| call.wrapped_atoms.first())
            .map(|atom| type_scope(atom));
        let Some((module, func)) = resolver_of(call, dsl, &block_resolvers) else {
            // no named function, but a context module is still an answer: a
            // `dataloader(Mod)` field is served by Mod, one level coarser
            if let Some(module) = context_of(call, dsl) {
                out.provides.push(Provides {
                    key: graphql_field_key(&scope, &camelize(atom)),
                    handler: HandlerRef::Module(module),
                    returns,
                });
            }
            continue;
        };
        out.provides.push(Provides {
            key: graphql_field_key(&scope, &camelize(atom)),
            handler: HandlerRef::Function { module, name: func },
            // `field :author, :player` → the second atom; `list_of(:lfg_post)` puts it
            // one level in. Anything else stays None rather than being guessed at.
            returns,
        });
    }
}

/// The resolver a member declares, in either spelling: nested `resolve(&M.f/3)`
/// or inline `resolve: &M.f/3`. `None` when it names no single function
/// (`dataloader(...)`, an inline `fn`, or two different `resolve` calls in one
/// block) — under-link rather than invent an edge.
fn resolver_of(
    call: &MacroCall,
    dsl: &SchemaDsl,
    block_resolvers: &HashMap<Vec<String>, Vec<&FunRef>>,
) -> Option<FunRef> {
    match block_resolvers.get(&call.inner_scope()).map(Vec::as_slice) {
        // ambiguous: naming one of them would be a coin flip presented as a fact
        Some([]) | Some([_, _, ..]) => return None,
        Some([r]) => return Some((*r).clone()),
        None => {}
    }
    call.keyword_fun_refs
        .iter()
        .find(|(key, _)| key == dsl.resolver)
        .map(|(_, r)| r.clone())
}

/// The context module a member's resolver names, when it names a module rather than a
/// function (`resolve: dataloader(App.Teams)`).
fn context_of(call: &MacroCall, dsl: &SchemaDsl) -> Option<String> {
    call.keyword_modules
        .iter()
        .find(|(key, _)| key == dsl.resolver)
        .map(|(_, module)| module.clone())
}

/// Normalize a scope-chain entry to the join key the resolve layer uses: a root
/// block keeps its own name, any other type block becomes `object:<name>`.
/// Anything else (a `def`, an Ecto `schema`) isn't a schema block at all.
fn block_scope(entry: &str, dsl: &SchemaDsl) -> Option<String> {
    if dsl.root_blocks.contains(&entry) {
        return Some(entry.to_owned());
    }
    let (macro_name, atom) = entry.split_once(':')?;
    dsl.type_blocks
        .contains(&macro_name)
        .then(|| type_scope(atom))
}

/// The wire spelling of a type scope: the name a GraphQL document writes.
///
/// The schema declares `object :lfg_post`; a document says `... on LfgPost`. The
/// detector owns that translation — the linker compares wire names and never
/// learns that this framework spells types in snake case (#32).
fn type_scope(atom: &str) -> String {
    crate::cross::operation_key(&camelize(atom))
}

/// Routes a router file declares: `get "/users/:id", UserController, :show`,
/// with every enclosing `scope "/api"` prefixed onto the path.
///
/// Under-link rather than guess: a route whose controller or action is computed
/// rather than written produces nothing, and `resources` — which expands into
/// seven routes by convention rather than by syntax — is deliberately not read.
/// Both show up in the unmatched-provider count instead of as invented edges.
fn router_facts(scan: &Scan, dsl: &RouterDsl, out: &mut CrossFacts) {
    for call in &scan.calls {
        if !dsl.verbs.contains(&&*call.name) {
            continue;
        }
        let (Some(path), Some(controller), Some(action)) = (
            call.strings.first(),
            call.modules.first(),
            call.atoms.first(),
        ) else {
            continue;
        };
        let full = format!("{}/{}", prefix_of(&call.scope, dsl), unquote(path));
        out.provides.push(Provides {
            key: http_key(&call.name, &full),
            handler: HandlerRef::Function {
                module: controller.clone(),
                name: action.clone(),
            },
            returns: None,
        });
    }
}

/// The path every enclosing prefix block contributes, outermost first.
fn prefix_of(scope: &[String], dsl: &RouterDsl) -> String {
    scope
        .iter()
        .filter_map(|entry| {
            let (macro_name, arg) = entry.split_once(':')?;
            dsl.prefix_blocks
                .contains(&macro_name)
                .then(|| unquote(arg).to_owned())
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn unquote(s: &str) -> &str {
    s.trim_matches('"')
}

fn data_facts(scan: &Scan, dsl: &DataDsl, out: &mut CrossFacts) {
    out.entity_def = scan
        .calls
        .iter()
        .any(|c| c.name == dsl.entity && !c.strings.is_empty());

    out.entity_refs = scan.struct_refs.clone();
    for call in scan
        .calls
        .iter()
        .filter(|c| dsl.queries.contains(&&*c.name))
    {
        out.entity_refs
            .extend(call.modules.iter().map(|m| (m.clone(), call.line)));
    }
    for rc in &scan.remote_calls {
        if rc.module.rsplit('.').next() == Some(dsl.repo) {
            out.entity_refs
                .extend(rc.modules.iter().map(|m| (m.clone(), rc.line)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LanguageAdapter;

    /// `(field name, module, function)` for every field a named resolver serves.
    fn fields(src: &str) -> Vec<(String, String, String)> {
        let mut p = tree_sitter::Parser::new();
        p.set_language(&crate::elixir::Adapter::new().grammar())
            .expect("elixir grammar");
        let tree = p.parse(src, None).expect("parse");
        let scan = super::super::macros::scan(tree.root_node(), src.as_bytes());
        cross_facts(&scan)
            .provides
            .iter()
            .filter_map(|p| {
                let (_, field) = crate::cross::graphql_scope_field(&p.key)?;
                match &p.handler {
                    HandlerRef::Function { module, name } => {
                        Some((field.to_owned(), module.clone(), name.clone()))
                    }
                    HandlerRef::Module(_) => None,
                }
            })
            .collect()
    }

    #[test]
    fn a_block_resolver_names_its_field() {
        let f = fields(
            "defmodule S do\n  alias App.PlayerResolver\n  query do\n    field :current_player, :player do\n      resolve(&PlayerResolver.current/3)\n    end\n  end\nend\n",
        );
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].0, "currentPlayer");
        assert_eq!(f[0].1, "App.PlayerResolver");
        assert_eq!(f[0].2, "current");
    }

    #[test]
    fn two_resolvers_in_one_field_yield_no_edge() {
        // picking one of them was a coin flip reported as a fact: the map was
        // last-write-wins, so `first` vanished silently
        let f = fields(
            "defmodule S do\n  alias App.PlayerResolver\n  query do\n    field :current_player, :player do\n      resolve(&PlayerResolver.first/3)\n      resolve(&PlayerResolver.second/3)\n    end\n  end\nend\n",
        );
        assert!(
            f.is_empty(),
            "ambiguous resolver must under-link, got {f:?}"
        );
    }

    fn routes(src: &str) -> Vec<(Option<String>, Vec<ir::Segment>, HandlerRef)> {
        let mut p = tree_sitter::Parser::new();
        p.set_language(&crate::elixir::Adapter::new().grammar())
            .expect("elixir grammar");
        let tree = p.parse(src, None).expect("parse");
        let scan = super::super::macros::scan(tree.root_node(), src.as_bytes());
        cross_facts(&scan)
            .provides
            .into_iter()
            .filter(|p| p.key.transport == ir::Transport::Http)
            .map(|p| (p.key.method, p.key.path, p.handler))
            .collect()
    }

    /// A router's scope prefixes compose, `:id` is a parameter, and the action is
    /// the handler — the shape every HTTP consumer has to match against.
    #[test]
    fn a_scoped_route_carries_its_prefix_and_its_parameter() {
        let r = routes(
            "defmodule Router do\n  scope \"/api\", AppWeb do\n    scope \"/v1\" do\n      get \"/users/:id\", UserController, :show\n      post \"/users\", UserController, :create\n    end\n  end\nend\n",
        );
        use ir::Segment::{Literal, Param};
        let lit = |s: &str| Literal(s.to_owned());
        assert_eq!(
            r,
            vec![
                (
                    Some("GET".into()),
                    vec![lit("api"), lit("v1"), lit("users"), Param],
                    HandlerRef::Function {
                        module: "UserController".into(),
                        name: "show".into()
                    }
                ),
                (
                    Some("POST".into()),
                    vec![lit("api"), lit("v1"), lit("users")],
                    HandlerRef::Function {
                        module: "UserController".into(),
                        name: "create".into()
                    }
                ),
            ]
        );
    }

    /// `resources` expands by convention, not by syntax. Reading it would mean
    /// inventing seven routes nobody wrote; it is left to the unmatched counter.
    #[test]
    fn a_conventional_route_macro_is_not_guessed_at() {
        assert!(routes(
            "defmodule Router do\n  scope \"/api\" do\n    resources \"/users\", UserController\n  end\nend\n"
        )
        .is_empty());
    }

    #[test]
    fn the_same_resolver_twice_is_not_ambiguous() {
        let f = fields(
            "defmodule S do\n  alias App.PlayerResolver\n  query do\n    field :current_player, :player do\n      resolve(&PlayerResolver.current/3)\n      resolve(&PlayerResolver.current/3)\n    end\n  end\nend\n",
        );
        assert_eq!(f.len(), 1, "one distinct target is still one answer");
        assert_eq!(f[0].2, "current");
    }
}
