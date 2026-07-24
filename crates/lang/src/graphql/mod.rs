//! GraphQL (`.gql`) adapter. Produces no code nodes (empty tags query) — it
//! exists so `.gql` operation documents flow through the single index parse and
//! contribute cross-service facts (operation → root field). See `cross`.

use crate::LanguageAdapter;

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

    /// No node kinds to capture — operations become cross-service facts, not nodes.
    fn tags_query(&self) -> &'static str {
        ""
    }

    fn extract_cross(&self, root: tree_sitter::Node, src: &[u8]) -> crate::cross::CrossFacts {
        crate::cross::graphql(root, src)
    }
}
