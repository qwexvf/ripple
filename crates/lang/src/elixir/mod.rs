//! Elixir adapter (v1, Tier 0). Elixir has no dedicated definition nodes —
//! `defmodule`/`def`/`defp` are macro `call`s — so tags.scm leans on predicates
//! (`#eq?`/`#any-of?` on the call target), which the parse layer now evaluates.

pub mod dsl;
pub mod macros;

use crate::LanguageAdapter;
use tree_sitter::Node;

pub struct Adapter;

impl Adapter {
    pub fn new() -> Self {
        Adapter
    }
}

impl Default for Adapter {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAdapter for Adapter {
    fn id(&self) -> &'static str {
        "elixir"
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_elixir::LANGUAGE.into()
    }

    fn file_globs(&self) -> &'static [&'static str] {
        &["*.ex", "*.exs"]
    }

    fn is_test_path(&self, rel: &str) -> bool {
        rel.ends_with("_test.exs") || rel.starts_with("test/") || rel.contains("/test/")
    }

    fn tags_query(&self) -> &'static str {
        include_str!("queries/tags.scm")
    }

    fn refs_query(&self) -> Option<&'static str> {
        Some(include_str!("queries/refs.scm"))
    }

    fn extract_cross(&self, root: tree_sitter::Node, src: &[u8]) -> crate::cross::CrossFacts {
        crate::cross::elixir(root, src)
    }

    /// Private iff the definition macro says so: `defp`, `defmacrop`, `defguardp`.
    /// Everything else the tags query captures is reachable from outside the
    /// module — `defmodule`, `defdelegate`, and a struct field (whose capture is
    /// the key atom, so it has no `target` to read and falls through to public).
    fn is_exported(&self, def: Node, src: &[u8]) -> bool {
        def.child_by_field_name("target")
            .and_then(|t| t.utf8_text(src).ok())
            .is_none_or(|kw| !matches!(kw, "defp" | "defmacrop" | "defguardp"))
    }

    /// Struct fields are qualified by their module, with the key's punctuation
    /// stripped: `defstruct [:name]` inside `User` → `User.name`.
    ///
    /// Two reasons, both about identity. A field's captured name is a punctuated
    /// token — `:name` in the list spelling, `name: ` (colon and trailing space
    /// included) in the keyword one, because the grammar has no bare identifier
    /// inside either — so unqualified the same field would be two different
    /// symbols depending on how it was written. And bare `name` would share a
    /// `SymbolId` with a `def name` in the same file, which collapses a function
    /// and a field into one node whose kind is whoever the query reached first.
    ///
    /// Functions stay unqualified: Elixir resolution keys on the bare name, and
    /// qualifying them is a separate change with its own fixtures to move.
    fn qualified_name(&self, kind: ir::NodeKind, name: &str, def: Node, src: &[u8]) -> String {
        if kind != ir::NodeKind::Field {
            return name.to_owned();
        }
        let field = name.trim().trim_start_matches(':').trim_end_matches(':');
        match enclosing_module(def, src) {
            Some(module) => format!("{module}.{field}"),
            None => field.to_owned(),
        }
    }
}

/// The module a node sits in, innermost first. `defmodule` is an ordinary `call`,
/// so this is a walk up the tree rather than a field lookup.
fn enclosing_module<'a>(node: Node, src: &'a [u8]) -> Option<&'a str> {
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "call" && call_target(n, src) == Some("defmodule") {
            return module_alias(n, src);
        }
        current = n.parent();
    }
    None
}

fn call_target<'a>(call: Node, src: &'a [u8]) -> Option<&'a str> {
    call.child_by_field_name("target")?.utf8_text(src).ok()
}

/// The alias a `defmodule` names: `defmodule Foo.Bar do` → `Foo.Bar`.
fn module_alias<'a>(call: Node, src: &'a [u8]) -> Option<&'a str> {
    let mut c = call.walk();
    let args = call.children(&mut c).find(|n| n.kind() == "arguments")?;
    let mut ac = args.walk();
    let alias = args.named_children(&mut ac).find(|n| n.kind() == "alias")?;
    alias.utf8_text(src).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ir::NodeKind;
    use streaming_iterator::StreamingIterator;
    use tree_sitter::{Query, QueryMatch, QueryPredicateArg};

    fn parse(src: &str) -> tree_sitter::Tree {
        let mut p = tree_sitter::Parser::new();
        p.set_language(&Adapter::new().grammar())
            .expect("elixir grammar");
        p.parse(src, None).expect("parse")
    }

    /// Every Elixir definition pattern is selected by a predicate on the call
    /// target, so a test that ignores predicates would report every call in the
    /// file as a definition. The parse layer evaluates them; this mirrors the
    /// capture-vs-strings forms tags.scm actually uses.
    fn predicates_hold(query: &Query, m: &QueryMatch, src: &[u8]) -> bool {
        for pred in query.general_predicates(m.pattern_index) {
            let [QueryPredicateArg::Capture(id), rest @ ..] = &pred.args[..] else {
                continue;
            };
            let text = m
                .captures
                .iter()
                .find(|c| c.index == *id)
                .and_then(|c| c.node.utf8_text(src).ok());
            let found = rest
                .iter()
                .any(|a| matches!(a, QueryPredicateArg::String(s) if Some(&**s) == text));
            if found != matches!(&*pred.operator, "eq?" | "any-of?") {
                return false;
            }
        }
        true
    }

    /// What the parse layer would build from tags.scm: (kind, name, qualified
    /// name, exported) per definition, sorted so a test pins a set rather than an
    /// arbitrary match order.
    fn defs(src: &str) -> Vec<(NodeKind, String, String, bool)> {
        let adapter = Adapter::new();
        let lang = adapter.grammar();
        let query = Query::new(&lang, adapter.tags_query()).expect("tags.scm");
        let tree = parse(src);
        let bytes = src.as_bytes();
        let names = query.capture_names();
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), bytes);
        let mut out = Vec::new();
        while let Some(m) = matches.next() {
            if !predicates_hold(&query, m, bytes) {
                continue;
            }
            let mut kind = None;
            let mut def_node = None;
            let mut name = None;
            for cap in m.captures {
                let cap_name = names[cap.index as usize];
                if cap_name == "name" {
                    name = cap.node.utf8_text(bytes).ok().map(str::to_owned);
                } else if let Some(k) = NodeKind::from_capture(cap_name) {
                    kind = Some(k);
                    def_node = Some(cap.node);
                }
            }
            let (Some(kind), Some(def_node), Some(name)) = (kind, def_node, name) else {
                continue;
            };
            let qualified = adapter.qualified_name(kind, &name, def_node, bytes);
            out.push((kind, name, qualified, adapter.is_exported(def_node, bytes)));
        }
        out.sort_by(|a, b| (a.1.as_str(), a.2.as_str()).cmp(&(b.1.as_str(), b.2.as_str())));
        out
    }

    fn callables(src: &str) -> Vec<(String, bool)> {
        defs(src)
            .into_iter()
            .filter(|(k, ..)| *k == NodeKind::Function)
            .map(|(_, name, _, exported)| (name, exported))
            .collect()
    }

    /// Qualified field names, sorted — the two spellings sort differently as raw
    /// captures (`:name` vs `name: `), and that order is not what is under test.
    fn fields(src: &str) -> Vec<String> {
        let mut out: Vec<String> = defs(src)
            .into_iter()
            .filter(|(k, ..)| *k == NodeKind::Field)
            .map(|(_, _, qualified, _)| qualified)
            .collect();
        out.sort();
        out
    }

    #[test]
    fn queries_compile() {
        let adapter = Adapter::new();
        let lang = adapter.grammar();
        Query::new(&lang, adapter.tags_query()).expect("tags.scm");
        Query::new(&lang, adapter.refs_query().expect("refs.scm")).expect("refs.scm");
    }

    /// Every macro that introduces something callable has to land somewhere. A
    /// `defdelegate`/`defguard` that produced no symbol left the function it
    /// declares invisible: callers resolved to nothing and a change to it had no
    /// blast radius at all.
    #[test]
    fn every_callable_definition_macro_is_captured() {
        let c = callables(
            "defmodule M do\n\
             \x20 def pub(x), do: x\n\
             \x20 defp priv(x), do: x\n\
             \x20 def bare, do: 1\n\
             \x20 defmacro mac(x) do\n\
             \x20 end\n\
             \x20 defmacrop macp(x), do: x\n\
             \x20 defdelegate reverse(list), to: Enum\n\
             \x20 defguard is_even(n) when rem(n, 2) == 0\n\
             \x20 defguardp is_odd(n) when rem(n, 2) == 1\n\
             end\n",
        );
        assert_eq!(
            c,
            [
                ("bare".to_owned(), true),
                ("is_even".to_owned(), true),
                ("is_odd".to_owned(), false),
                ("mac".to_owned(), true),
                ("macp".to_owned(), false),
                ("priv".to_owned(), false),
                ("pub".to_owned(), true),
                ("reverse".to_owned(), true),
            ]
        );
    }

    /// `defp`/`defmacrop`/`defguardp` are captured *and* private. `defguardp` was
    /// the odd one out: `is_exported` only knew the first two, so a private guard
    /// was published as part of the module's surface.
    #[test]
    fn the_private_spellings_are_captured_and_not_exported() {
        let private: Vec<String> = callables(
            "defmodule M do\n\
             \x20 defp a(x), do: x\n\
             \x20 defmacrop b(x), do: x\n\
             \x20 defguardp c(x) when is_integer(x)\n\
             end\n",
        )
        .into_iter()
        .filter(|(_, exported)| !exported)
        .map(|(name, _)| name)
        .collect();
        assert_eq!(private, ["a", "b", "c"]);
    }

    /// A delegation declares the function *in this module*. `as:` renames the
    /// target in the other module, so reading it as the local name would file the
    /// symbol under a name no caller ever writes.
    #[test]
    fn a_delegation_is_named_by_its_own_header() {
        let c = callables(
            "defmodule M do\n\
             \x20 defdelegate size(map), to: Map, as: :map_size\n\
             end\n",
        );
        assert_eq!(c, [("size".to_owned(), true)]);
    }

    /// A guard's header is `name(args) when body`. The body is made of calls too,
    /// and capturing one of those instead would name the symbol `is_integer`.
    #[test]
    fn a_guard_is_named_by_its_header_not_its_body() {
        let c = callables(
            "defmodule M do\n\
             \x20 defguard is_even(n) when is_integer(n) and rem(n, 2) == 0\n\
             end\n",
        );
        assert_eq!(c, [("is_even".to_owned(), true)]);
    }

    /// Clauses of one function are one symbol. Identity is (path, qualified name),
    /// so the clauses have to agree on the qualified name or `extra_spans` never
    /// gets the chance to collapse them and the function becomes N symbols.
    #[test]
    fn multi_clause_functions_share_one_qualified_name() {
        let q: Vec<String> = defs(
            "defmodule M do\n\
             \x20 def go([]), do: :empty\n\
             \x20 def go([h | _]), do: h\n\
             \x20 def go(x) when is_integer(x), do: x\n\
             end\n",
        )
        .into_iter()
        .filter(|(k, ..)| *k == NodeKind::Function)
        .map(|(_, _, qualified, _)| qualified)
        .collect();
        assert_eq!(q, ["go", "go", "go"]);
    }

    /// Both struct spellings, and the mixed one, name the same kind of thing.
    /// Ripple had no field symbol for any Elixir repo before this.
    #[test]
    fn struct_fields_are_captured_in_every_spelling() {
        assert_eq!(
            fields("defmodule User do\n  defstruct [:name, :email]\nend\n"),
            ["User.email", "User.name"]
        );
        assert_eq!(
            fields("defmodule User do\n  defstruct name: nil, age: 0\nend\n"),
            ["User.age", "User.name"]
        );
        assert_eq!(
            fields("defmodule User do\n  defstruct [:name, age: 0]\nend\n"),
            ["User.age", "User.name"],
            "a list may hold bare keys and defaulted ones at once"
        );
    }

    /// The qualifier is what keeps a field and a same-named function apart. Bare
    /// `name` for both means one `SymbolId`, and the loser of the race stops
    /// existing — the function would be reported as a struct field.
    #[test]
    fn a_field_does_not_collide_with_a_function_of_the_same_name() {
        let d = defs(
            "defmodule User do\n\
             \x20 defstruct [:name]\n\
             \x20 def name(u), do: u.name\n\
             end\n",
        );
        let qualified: Vec<(NodeKind, &str)> = d
            .iter()
            .filter(|(k, ..)| *k != NodeKind::Class)
            .map(|(k, _, q, _)| (*k, q.as_str()))
            .collect();
        assert_eq!(
            qualified,
            [(NodeKind::Field, "User.name"), (NodeKind::Function, "name")]
        );
    }

    /// `defexception [:message]` has the same shape as a `defstruct` and does
    /// declare a struct — it is left out until it is asked for, so the capture
    /// count stays exactly what the predicate says.
    #[test]
    fn only_defstruct_declares_fields() {
        assert!(fields("defmodule E do\n  defexception [:message]\nend\n").is_empty());
        assert!(
            fields("defmodule U do\n  @enforce_keys [:name]\n  defstruct [:name]\nend\n").len()
                == 1,
            "@enforce_keys carries the same list and must not double the field"
        );
    }

    /// Module attributes are not definitions, on purpose. Nothing separates
    /// `@timeout 5_000` from `@moduledoc "…"` by shape, so capturing the shape
    /// would add a `doc`, a `spec` and an `impl` symbol to every module.
    #[test]
    fn module_attributes_are_not_definitions() {
        let d = defs(
            "defmodule M do\n\
             \x20 @moduledoc \"hi\"\n\
             \x20 @behaviour GenServer\n\
             \x20 @timeout 5_000\n\
             \x20 @spec go(integer) :: integer\n\
             \x20 def go(x), do: x\n\
             end\n",
        );
        let non_module: Vec<&str> = d
            .iter()
            .filter(|(k, ..)| *k != NodeKind::Class)
            .map(|(_, _, q, _)| q.as_str())
            .collect();
        assert_eq!(non_module, ["go"]);
    }

    /// The qualifier is the alias as written on the nearest `defmodule` — an
    /// outer module does not own a struct declared two levels down.
    #[test]
    fn a_nested_module_qualifies_the_fields_inside_it() {
        assert_eq!(
            fields("defmodule App do\n  defmodule Inner do\n    defstruct [:id]\n  end\nend\n"),
            ["Inner.id"],
            "the innermost defmodule owns the struct"
        );
        assert_eq!(
            fields("defmodule App.Inner do\n  defstruct [:id]\nend\n"),
            ["App.Inner.id"],
            "a dotted alias survives whole"
        );
        assert_eq!(
            fields("defstruct [:loose]\n"),
            ["loose"],
            "no enclosing module still yields a usable name"
        );
    }
}
