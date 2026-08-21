//! PHP adapter — import-level only (Tier 0 defs + Tier 1 imports).
//!
//! This exists for reachability-engine parity: aegis-reach covers PHP at the
//! import level (does the project `use` this dependency at all), and the engine
//! must match that before aegis-reach can be deleted. Full call binding is
//! deferred — a PHP call's target is Composer-autoload-dependent and needs
//! namespace→package resolution the engine does not model yet. What lands here
//! is the soundness floor: every `use A\B\C;` mints an `External` module node and
//! an `Imports` edge, so `engine::imports(dep)` answers true.

use crate::{resolve_import, LanguageAdapter};

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
        "php"
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_php::LANGUAGE_PHP.into()
    }

    fn file_globs(&self) -> &'static [&'static str] {
        &["*.php"]
    }

    fn tags_query(&self) -> &'static str {
        include_str!("queries/tags.scm")
    }

    fn imports_query(&self) -> Option<&'static str> {
        Some(include_str!("queries/imports.scm"))
    }

    /// The dep-key of a `use` path is its top namespace segment
    /// (`GuzzleHttp\Client` → `GuzzleHttp`). Composer maps namespaces to packages
    /// out of band, so this is the import-level floor, not a package identity.
    fn external_dep_key(&self, spec: &str) -> Option<String> {
        resolve_import::php_dep_key(spec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_compile() {
        let adapter = Adapter::new();
        let lang = adapter.grammar();
        tree_sitter::Query::new(&lang, adapter.tags_query()).expect("tags.scm");
        tree_sitter::Query::new(&lang, adapter.imports_query().expect("imports.scm present"))
            .expect("imports.scm");
    }

    #[test]
    fn dep_key_is_the_top_namespace_segment() {
        let adapter = Adapter::new();
        assert_eq!(
            adapter.external_dep_key("GuzzleHttp\\Client"),
            Some("GuzzleHttp".to_owned())
        );
        // a leading `\` marks a fully-qualified name and is not part of the key
        assert_eq!(
            adapter.external_dep_key("\\Symfony\\Component\\Console"),
            Some("Symfony".to_owned())
        );
        assert_eq!(adapter.external_dep_key("Foo"), Some("Foo".to_owned()));
    }
}
