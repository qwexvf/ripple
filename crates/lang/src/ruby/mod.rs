//! Ruby adapter — Tier 0 defs, Tier 1 imports, Tier 2 call *sites*.
//!
//! Like the PHP adapter, this started as reachability-engine parity with
//! aegis-reach's import-level Ruby coverage: every `require "x"` / `gem "x"`
//! mints an `External` module node keyed by the first path segment and an
//! `Imports` edge, so `engine::imports(dep)` is true. Ruby's `require` loads a
//! file for its side effects rather than binding a symbol, so that is still all
//! an import gives us.
//!
//! `refs.scm` adds the same floor Go has: it records where a call *happens*
//! (`helper(x)` → `@ref.call`, `obj.foo(x)` → `@ref.recv` + `@ref.member`) and
//! lets the resolver bind by name, splitting confidence across candidates. It
//! does not resolve dispatch — Ruby method lookup is fully dynamic, so member
//! calls stay weak, and a paren-less call is not captured at all because the
//! grammar parses it identically to a local variable read.

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
        "ruby"
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_ruby::LANGUAGE.into()
    }

    fn file_globs(&self) -> &'static [&'static str] {
        &["*.rb"]
    }

    /// RSpec/minitest conventions: specs live under `spec/` or `test/`, or in a
    /// `*_spec.rb` / `*_test.rb` file.
    fn is_test_path(&self, rel: &str) -> bool {
        let file = rel.rsplit('/').next().unwrap_or(rel);
        file.ends_with("_spec.rb")
            || file.ends_with("_test.rb")
            || rel.starts_with("spec/")
            || rel.contains("/spec/")
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

    /// The dep-key of a `require`/`gem` target is its first path segment
    /// (`active_record/base` → `active_record`).
    fn external_dep_key(&self, spec: &str) -> Option<String> {
        resolve_import::ruby_dep_key(spec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every ref capture as `(capture name, captured text)`, in document order —
    /// the same captures `parse::extract_refs` reads.
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
    fn a_bare_call_is_a_ref_call() {
        assert_eq!(
            refs("helper(x)\n"),
            [("ref.call".to_owned(), "helper".to_owned())]
        );
    }

    /// The receiver and the method name both land, and the bare-call pattern must
    /// *not* also fire — that is what `!receiver` buys.
    #[test]
    fn a_receiver_call_is_recv_plus_member() {
        assert_eq!(
            refs("obj.foo(x)\n"),
            [
                ("ref.member".to_owned(), "foo".to_owned()),
                ("ref.recv".to_owned(), "obj".to_owned()),
            ]
        );
        assert_eq!(
            refs("Foo::Bar.baz(1)\n"),
            [
                ("ref.member".to_owned(), "baz".to_owned()),
                ("ref.recv".to_owned(), "Foo::Bar".to_owned()),
            ]
        );
    }

    /// A paren-less call and a local variable read are the same node kind, so
    /// neither is captured — a call edge per variable mention would be worse than
    /// the missing edge.
    #[test]
    fn bare_identifiers_are_not_captured_as_calls() {
        assert_eq!(refs("a = 1\nb = a\n"), []);
        assert_eq!(refs("def m\n  other_method\nend\n"), []);
    }

    #[test]
    fn dep_key_is_the_first_path_segment() {
        let adapter = Adapter::new();
        assert_eq!(adapter.external_dep_key("json"), Some("json".to_owned()));
        assert_eq!(
            adapter.external_dep_key("active_record/base"),
            Some("active_record".to_owned())
        );
        // a relative require is a local file, not a dependency
        assert_eq!(adapter.external_dep_key("./helpers"), None);
    }
}
