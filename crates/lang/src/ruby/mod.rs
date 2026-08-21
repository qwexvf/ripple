//! Ruby adapter — import-level only (Tier 0 defs + Tier 1 imports).
//!
//! Like the PHP adapter, this exists for reachability-engine parity with
//! aegis-reach's import-level Ruby coverage. Ruby's `require` loads a file for
//! its side effects rather than binding a symbol, and method dispatch is fully
//! dynamic, so call binding is deferred. The soundness floor is what lands here:
//! every `require "x"` / `gem "x"` mints an `External` module node keyed by the
//! first path segment and an `Imports` edge, so `engine::imports(dep)` is true.

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

    /// The dep-key of a `require`/`gem` target is its first path segment
    /// (`active_record/base` → `active_record`).
    fn external_dep_key(&self, spec: &str) -> Option<String> {
        resolve_import::ruby_dep_key(spec)
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
