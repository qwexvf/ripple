//! PHP adapter — Tier 0 defs, Tier 1 imports, Tier 2 call *sites*.
//!
//! This started as reachability-engine parity: aegis-reach covers PHP at the
//! import level (does the project `use` this dependency at all), and the engine
//! must match that before aegis-reach can be deleted. Every `use A\B\C;` mints an
//! `External` module node and an `Imports` edge, so `engine::imports(dep)`
//! answers true.
//!
//! `refs.scm` adds the same floor Go has: it records where a call *happens*
//! (`helper($x)` → `@ref.call`, `$obj->m()` / `Foo::m()` → `@ref.recv` +
//! `@ref.member`) and lets the resolver bind by name, splitting confidence across
//! candidates. Full call binding is still deferred — a PHP call's target is
//! Composer-autoload-dependent and needs the namespace→package resolution the
//! engine does not model yet, so member and static calls stay weak and a dynamic
//! callee (`$fn()`, `$obj->$m()`) is not captured at all.

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

    /// PHPUnit conventions: tests live under a `tests/` or `test/` directory, or
    /// in a `*Test.php` file.
    fn is_test_path(&self, rel: &str) -> bool {
        let file = rel.rsplit('/').next().unwrap_or(rel);
        file.ends_with("Test.php")
            || rel.starts_with("tests/")
            || rel.contains("/tests/")
            || rel.starts_with("test/")
            || rel.contains("/test/")
    }

    fn tags_query(&self) -> &'static str {
        include_str!("queries/tags.scm")
    }

    fn imports_query(&self) -> Option<&'static str> {
        Some(include_str!("queries/imports.scm"))
    }

    fn refs_query(&self) -> Option<&'static str> {
        Some(include_str!("queries/refs.scm"))
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

    /// Every ref capture as `(capture name, captured text)` — the same captures
    /// `parse::extract_refs` reads.
    fn refs(src: &str) -> Vec<(String, String)> {
        let adapter = Adapter::new();
        let lang = adapter.grammar();
        let query = tree_sitter::Query::new(&lang, adapter.refs_query().expect("refs.scm present"))
            .expect("refs.scm");
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang).expect("grammar");
        let tree = parser.parse(src, None).expect("parse");
        let bytes = src.as_bytes();
        let names = query.capture_names();
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), bytes);
        let mut out = Vec::new();
        while let Some(m) = streaming_iterator::StreamingIterator::next(&mut matches) {
            for cap in m.captures {
                let text = cap.node.utf8_text(bytes).unwrap_or_default();
                out.push((names[cap.index as usize].to_owned(), text.to_owned()));
            }
        }
        out.sort();
        out
    }

    #[test]
    fn queries_compile() {
        let adapter = Adapter::new();
        let lang = adapter.grammar();
        tree_sitter::Query::new(&lang, adapter.tags_query()).expect("tags.scm");
        tree_sitter::Query::new(&lang, adapter.imports_query().expect("imports.scm present"))
            .expect("imports.scm");
        tree_sitter::Query::new(&lang, adapter.refs_query().expect("refs.scm present"))
            .expect("refs.scm");
    }

    #[test]
    fn a_function_call_is_a_ref_call() {
        assert_eq!(
            refs("<?php helper($x);\n"),
            [("ref.call".to_owned(), "helper".to_owned())]
        );
    }

    #[test]
    fn a_member_call_is_recv_plus_member() {
        assert_eq!(
            refs("<?php $obj->method($x);\n"),
            [
                ("ref.member".to_owned(), "method".to_owned()),
                ("ref.recv".to_owned(), "$obj".to_owned()),
            ]
        );
        assert_eq!(
            refs("<?php $obj?->method($x);\n"),
            [
                ("ref.member".to_owned(), "method".to_owned()),
                ("ref.recv".to_owned(), "$obj".to_owned()),
            ]
        );
    }

    #[test]
    fn a_static_call_is_recv_plus_member() {
        assert_eq!(
            refs("<?php Foo::bar($x);\n"),
            [
                ("ref.member".to_owned(), "bar".to_owned()),
                ("ref.recv".to_owned(), "Foo".to_owned()),
            ]
        );
        assert_eq!(
            refs("<?php self::bar($x);\n"),
            [
                ("ref.member".to_owned(), "bar".to_owned()),
                ("ref.recv".to_owned(), "self".to_owned()),
            ]
        );
    }

    /// A callee whose name is not in the source cannot be bound to anything, and
    /// `new Thing()` is a construction, not a call to a named function.
    #[test]
    fn dynamic_callees_and_constructions_are_not_captured() {
        assert_eq!(refs("<?php $fn($x);\n"), []);
        assert_eq!(refs("<?php $obj->$m($x);\n"), []);
        assert_eq!(refs("<?php new Thing($x);\n"), []);
    }

    #[test]
    fn phpunit_paths_are_tests() {
        let adapter = Adapter::new();
        assert!(adapter.is_test_path("tests/FooTest.php"));
        assert!(adapter.is_test_path("src/FooTest.php"));
        assert!(adapter.is_test_path("test/Unit/Foo.php"));
        assert!(adapter.is_test_path("pkg/tests/Bar.php"));
        assert!(!adapter.is_test_path("src/Foo.php"));
        assert!(!adapter.is_test_path("src/Testing.php"));
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
