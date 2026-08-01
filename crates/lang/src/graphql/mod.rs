//! GraphQL (`.gql`, `.graphql`) adapter.
//!
//! Two jobs. The first is cross-service: an operation document contributes
//! `operation → root field` facts that the linker joins against the resolvers
//! serving them, with no node involved on either side (see `cross`). The second
//! is ordinary Tier-0 extraction — a schema declares types, fields, enum values
//! and input fields, and a document declares operations and fragments, all named,
//! all referenced from other files, and all invisible while the tags query was
//! empty.

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
        "graphql"
    }

    fn grammar(&self) -> tree_sitter::Language {
        crate::graphql_language()
    }

    fn file_globs(&self) -> &'static [&'static str] {
        &["*.gql", "*.graphql"]
    }

    fn tags_query(&self) -> &'static str {
        include_str!("queries/tags.scm")
    }

    /// A field is named by the type that declares it. Identity is (path,
    /// qualified name), and `id`/`name`/`createdAt` sit on nearly every type in a
    /// real schema — unqualified, one file's worth of them would collapse into a
    /// single symbol carrying a dozen definition sites.
    fn qualified_name(&self, kind: ir::NodeKind, name: &str, def: Node, src: &[u8]) -> String {
        if kind != ir::NodeKind::Field {
            return name.to_owned();
        }
        match owner_type_name(def, src) {
            Some(ty) => format!("{ty}.{name}"),
            None => name.to_owned(),
        }
    }

    fn extract_cross(&self, root: tree_sitter::Node, src: &[u8]) -> crate::cross::CrossFacts {
        crate::cross::graphql(root, src)
    }
}

/// Name of the type-ish definition a member sits in. `extend type User` declares
/// fields on `User` exactly as the original block does, so the extensions are
/// walked too.
fn owner_type_name(node: Node, src: &[u8]) -> Option<String> {
    const OWNERS: [&str; 8] = [
        "object_type_definition",
        "interface_type_definition",
        "input_object_type_definition",
        "enum_type_definition",
        "object_type_extension",
        "interface_type_extension",
        "input_object_type_extension",
        "enum_type_extension",
    ];
    let mut cur = node.parent();
    while let Some(n) = cur {
        if OWNERS.contains(&n.kind()) {
            let mut c = n.walk();
            let name = n.named_children(&mut c).find(|ch| ch.kind() == "name")?;
            return name.utf8_text(src).ok().map(str::to_owned);
        }
        cur = n.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> tree_sitter::Tree {
        let mut p = tree_sitter::Parser::new();
        p.set_language(&Adapter::new().grammar()).expect("grammar");
        p.parse(src, None).expect("parse")
    }

    /// `(kind, qualified name)` for every definition the tags query captures.
    /// Qualification is part of what a capture yields — two fields that forgot it
    /// would share a `SymbolId`.
    fn captured(src: &str) -> Vec<(String, String)> {
        let adapter = Adapter::new();
        let lang = adapter.grammar();
        let query = tree_sitter::Query::new(&lang, adapter.tags_query()).expect("tags.scm");
        let names = query.capture_names();
        let tree = parse(src);
        let bytes = src.as_bytes();
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), bytes);
        let mut out = Vec::new();
        while let Some(m) = streaming_iterator::StreamingIterator::next(&mut matches) {
            let mut kind = None;
            let mut name = None;
            let mut def = None;
            for cap in m.captures {
                let cap_name = names[cap.index as usize];
                if let Some(k) = ir::NodeKind::from_capture(cap_name) {
                    kind = Some(k);
                    def = Some(cap.node);
                } else if cap_name == "name" {
                    name = cap.node.utf8_text(bytes).ok().map(str::to_owned);
                }
            }
            if let (Some(k), Some(n), Some(d)) = (kind, name, def) {
                out.push((
                    format!("{k:?}").to_lowercase(),
                    adapter.qualified_name(k, &n, d, bytes),
                ));
            }
        }
        out.sort();
        out
    }

    #[test]
    fn queries_compile() {
        let adapter = Adapter::new();
        tree_sitter::Query::new(&adapter.grammar(), adapter.tags_query()).expect("tags.scm");
    }

    #[test]
    fn tier_two_queries_are_absent_on_purpose() {
        let adapter = Adapter::new();
        assert!(adapter.refs_query().is_none());
        assert!(adapter.imports_query().is_none());
        assert!(adapter.bindings_query().is_none());
    }

    /// Every kind of schema declaration, and — the half that was missing entirely
    /// — the members inside them. A field is the unit a resolver serves, so it is
    /// the unit an impact answer has to be able to name.
    #[test]
    fn schema_declarations_and_their_members_are_captured() {
        let caps = captured(
            "scalar DateTime\n\
             interface Node { id: ID! }\n\
             type User implements Node { id: ID! email: String }\n\
             enum Role { ADMIN MEMBER }\n\
             input UserFilter { role: Role }\n\
             union Actor = User | Bot\n",
        );
        assert_eq!(
            caps,
            [
                ("enum".to_owned(), "Role".to_owned()),
                ("field".to_owned(), "Node.id".to_owned()),
                ("field".to_owned(), "Role.ADMIN".to_owned()),
                ("field".to_owned(), "Role.MEMBER".to_owned()),
                ("field".to_owned(), "User.email".to_owned()),
                ("field".to_owned(), "User.id".to_owned()),
                ("field".to_owned(), "UserFilter.role".to_owned()),
                ("interface".to_owned(), "Node".to_owned()),
                ("type".to_owned(), "Actor".to_owned()),
                ("type".to_owned(), "DateTime".to_owned()),
                ("type".to_owned(), "User".to_owned()),
                ("type".to_owned(), "UserFilter".to_owned()),
            ]
        );
    }

    /// The type patterns match direct children only, so a name nested in
    /// `fields_definition` must not also read as the type's own name.
    #[test]
    fn a_types_own_name_is_captured_once() {
        let types: Vec<String> = captured("type User { id: ID name: String }\n")
            .into_iter()
            .filter(|(kind, _)| kind == "type")
            .map(|(_, name)| name)
            .collect();
        assert_eq!(types, ["User"]);
    }

    /// A field's arguments are parameters, not symbols anything references across
    /// files. They are `input_value_definition`s just like an input type's fields,
    /// so capturing that node unconditionally would have turned every argument in
    /// a schema into a node.
    #[test]
    fn field_arguments_are_not_symbols() {
        let caps = captured("type Query { user(id: ID!, first: Int): User }\n");
        assert_eq!(
            caps,
            [
                ("field".to_owned(), "Query.user".to_owned()),
                ("type".to_owned(), "Query".to_owned()),
            ]
        );
    }

    /// `extend type` declares fields on a type defined elsewhere; they belong to
    /// that type, not to a symbol named after the extension block.
    #[test]
    fn extension_fields_belong_to_the_type_they_extend() {
        let caps = captured("extend type User { nickname: String }\n");
        assert_eq!(caps, [("field".to_owned(), "User.nickname".to_owned())]);
    }

    /// The document half: a named operation is what codegen exports as
    /// `<Name>Document` and what cross-service linking matches on, and a fragment
    /// is what `...Name` spreads. An anonymous operation names nothing and so is
    /// no symbol.
    #[test]
    fn operations_and_fragments_are_captured() {
        let caps = captured(
            "query CurrentPlayer { player { ...PlayerFields } }\n\
             mutation FollowPlayer { followPlayer { id } }\n\
             fragment PlayerFields on Player { id }\n\
             { anonymous { id } }\n",
        );
        assert_eq!(
            caps,
            [
                ("function".to_owned(), "CurrentPlayer".to_owned()),
                ("function".to_owned(), "FollowPlayer".to_owned()),
                ("function".to_owned(), "PlayerFields".to_owned()),
            ]
        );
    }

    /// Selections inside an operation are *uses* of a schema field, not
    /// declarations — capturing them would invent a definition per query, and the
    /// `.gql` documents in this repo's fixtures are all selections.
    #[test]
    fn selections_are_not_definitions() {
        assert!(captured("query Q { user { id } }\n")
            .iter()
            .all(|(kind, _)| kind == "function"));
    }
}
