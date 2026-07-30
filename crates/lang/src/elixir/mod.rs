//! Elixir adapter (v1, Tier 0). Elixir has no dedicated definition nodes —
//! `defmodule`/`def`/`defp` are macro `call`s — so tags.scm leans on predicates
//! (`#eq?`/`#any-of?` on the call target), which the parse layer now evaluates.

pub mod dsl;
pub mod macros;

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

    /// Public if the definition macro is `def`/`defmacro` (not `defp`/`defmacrop`).
    /// Modules (`defmodule`) are public.
    fn is_exported(&self, def: tree_sitter::Node, src: &[u8]) -> bool {
        def.child_by_field_name("target")
            .and_then(|t| t.utf8_text(src).ok())
            .is_none_or(|kw| kw != "defp" && kw != "defmacrop")
    }
}
