//! Which Elixir macro shapes carry cross-service meaning.
//!
//! [`super::macros`] reads macro *shape* without knowing any framework; this
//! module is the only place framework names appear. Supporting another DSL
//! (Ash, Phoenix router, LiveView) means adding a table here and projecting it —
//! not touching the walker. See docs/05-language-support.md.

use super::macros::{FunRef, MacroCall, Scan};
use crate::cross::{camelize, AbsintheField, CrossFacts, ElixirFacts, GQL_ROOT_SCOPES};
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

const ECTO: DataDsl = DataDsl {
    entity: "schema",
    repo: "Repo",
    queries: &["from"],
};

/// Project a generic macro scan onto the cross-service facts the resolve layer
/// joins on.
pub fn cross_facts(scan: &Scan) -> CrossFacts {
    let mut f = ElixirFacts {
        aliases: scan.aliases.clone(),
        ..Default::default()
    };
    schema_facts(scan, &ABSINTHE, &mut f);
    data_facts(scan, &ECTO, &mut f);
    f.imports = scan
        .calls
        .iter()
        .filter(|c| c.name == "import")
        .flat_map(|c| c.modules.iter().cloned())
        .collect();
    f.remote_calls = scan
        .remote_calls
        .iter()
        .map(|rc| (rc.module.clone(), rc.func.clone(), rc.line))
        .collect();
    CrossFacts {
        elixir: Some(f),
        ..Default::default()
    }
}

fn schema_facts(scan: &Scan, dsl: &SchemaDsl, out: &mut ElixirFacts) {
    // block-form resolvers are their own macro call inside the member's block,
    // so index them by the scope chain they sit in
    let mut block_resolvers: HashMap<Vec<String>, &FunRef> = HashMap::new();
    for call in scan.calls.iter().filter(|c| c.name == dsl.resolver) {
        if let Some(r) = call.fun_refs.first() {
            block_resolvers.insert(call.scope.clone(), r);
        }
    }

    for call in &scan.calls {
        let Some(scope) = call.scope.last().and_then(|s| block_scope(s, dsl)) else {
            continue;
        };
        if call.name == dsl.include {
            if let Some(atom) = call.atoms.first() {
                out.scope_includes.push((scope, type_scope(atom)));
            }
            continue;
        }
        if call.name != dsl.member {
            continue;
        }
        let Some(atom) = call.atoms.first() else {
            continue;
        };
        let Some((module, func)) = resolver_of(call, dsl, &block_resolvers) else {
            continue;
        };
        out.fields.push(AbsintheField {
            scope,
            field: camelize(atom),
            module,
            func,
        });
    }
}

/// The resolver a member declares, in either spelling: nested `resolve(&M.f/3)`
/// or inline `resolve: &M.f/3`. `None` when it names no single function
/// (`dataloader(...)`, an inline `fn`) — under-link rather than invent an edge.
fn resolver_of(
    call: &MacroCall,
    dsl: &SchemaDsl,
    block_resolvers: &HashMap<Vec<String>, &FunRef>,
) -> Option<FunRef> {
    if let Some(r) = block_resolvers.get(&call.inner_scope()) {
        return Some((*r).clone());
    }
    call.keyword_fun_refs
        .iter()
        .find(|(key, _)| key == dsl.resolver)
        .map(|(_, r)| r.clone())
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

fn type_scope(atom: &str) -> String {
    format!("object:{atom}")
}

fn data_facts(scan: &Scan, dsl: &DataDsl, out: &mut ElixirFacts) {
    out.is_schema = scan
        .calls
        .iter()
        .any(|c| c.name == dsl.entity && !c.strings.is_empty());

    out.schema_refs = scan.struct_refs.clone();
    for call in scan
        .calls
        .iter()
        .filter(|c| dsl.queries.contains(&&*c.name))
    {
        out.schema_refs
            .extend(call.modules.iter().map(|m| (m.clone(), call.line)));
    }
    for rc in &scan.remote_calls {
        if rc.module.rsplit('.').next() == Some(dsl.repo) {
            out.schema_refs
                .extend(rc.modules.iter().map(|m| (m.clone(), rc.line)));
        }
    }
}
