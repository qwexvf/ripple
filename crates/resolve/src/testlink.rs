//! `Tests` edges: which symbols a repo's tests exercise.
//!
//! Runs after every other linker, because a call edge can come from any of them
//! (TypeScript from `link`, Elixir from cross-service, Go and Gleam only from the
//! language-server pass). Test convention is language knowledge, so it is read off
//! the adapter (`is_test_path`) and off the spans a tags query marked
//! (`FileExtract.test_scopes`) — nothing here knows what a language is.
//!
//! Before this existed nothing in the workspace ever constructed an
//! `EdgeKind::Tests`, so `review` printed `untested` on every row of every repo
//! (issue #36).

use ir::{Edge, EdgeKind, EdgeSource, SymbolId};
use lang::LanguageAdapter;
use parse::CachedFile;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

/// A `Tests` edge rests on a call whose own reliability is already priced in
/// `confidence`; this is the extra step — that a call from test code exercises
/// the callee. Strong, but not certain: fixtures and `support/` helpers are
/// called from tests too. Multiplied into the underlying edge, never replacing it.
const CONF_TESTS: f32 = 0.8;

/// The symbols that live on the test side of a repo.
pub struct TestScopes {
    ids: HashSet<SymbolId>,
    /// module nodes of files that are *entirely* tests. Kept apart from `ids`
    /// because a file whose tests are inline (Rust) is not one of these: its
    /// module node also holds production code.
    whole_files: HashSet<SymbolId>,
}

impl TestScopes {
    /// Mark every symbol in a test file, plus every symbol inside an in-file test
    /// scope (Rust's `#[cfg(test)] mod tests`).
    ///
    /// The path handed to the adapter is relative to its own root: `module_path`
    /// carries a tag prefix in a multi-root index (`api/test/foo_test.exs`), which
    /// would break every convention anchored at the start of the path.
    pub fn of(
        files: &[CachedFile],
        roots: &[(String, PathBuf)],
        registry: &[Box<dyn LanguageAdapter>],
    ) -> TestScopes {
        let mut ids = HashSet::new();
        let mut whole_files = HashSet::new();
        for f in files {
            let rel = roots
                .iter()
                .find_map(|(_, r)| f.canonical.strip_prefix(r).ok())
                .map_or_else(
                    || f.module_path.clone(),
                    |p| p.to_string_lossy().replace('\\', "/"),
                );
            let is_test_file =
                lang::adapter_for(registry, &f.canonical).is_some_and(|a| a.is_test_path(&rel));

            if is_test_file {
                let module = SymbolId::module(&f.module_path);
                ids.insert(module);
                whole_files.insert(module);
                ids.extend(f.extract.defs.iter().map(|d| d.id));
                continue;
            }
            for d in &f.extract.defs {
                let inside =
                    f.extract.test_scopes.iter().any(|s| {
                        d.span.start_line >= s.start_line && d.span.end_line <= s.end_line
                    });
                if inside {
                    ids.insert(d.id);
                }
            }
        }
        TestScopes { ids, whole_files }
    }

    pub fn contains(&self, id: SymbolId) -> bool {
        self.ids.contains(&id)
    }

    /// Is this the module node of a file that holds nothing but tests?
    fn is_test_file(&self, id: SymbolId) -> bool {
        self.whole_files.contains(&id)
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

/// One `Tests` edge per (test symbol, tested symbol) pair reachable by a call or
/// reference that leaves the test side.
///
/// Every emitted edge duplicates the endpoints of an edge that already exists, so
/// this adds no dependent that structural risk wasn't already counting — the flag
/// changes, the ranking does not.
pub fn link_tests(scopes: &TestScopes, edges: &[Edge]) -> Vec<Edge> {
    if scopes.is_empty() {
        return Vec::new();
    }
    // keyed for determinism: several call sites between one pair are one relationship
    let mut by_pair: BTreeMap<(u64, u64), Edge> = BTreeMap::new();
    for e in edges {
        // an import is a weaker claim than a call, and only counts from a file that
        // is nothing but tests: `import { getPath }` at the top of util.test.ts is
        // the test file exercising getPath, but an import in a file that merely
        // *contains* a `#[cfg(test)] mod` says nothing. Without this the test
        // module stayed a dependent and a well-tested symbol still scored fanout (#42).
        let convertible = match e.kind {
            EdgeKind::Calls | EdgeKind::References => true,
            EdgeKind::Imports => scopes.is_test_file(e.src),
            _ => false,
        };
        if !convertible {
            continue;
        }
        if !scopes.contains(e.src) || scopes.contains(e.dst) {
            continue;
        }
        let candidate = Edge {
            src: e.src,
            dst: e.dst,
            kind: EdgeKind::Tests,
            confidence: e.confidence * CONF_TESTS,
            site: e.site,
            // ripple's own inference, whatever the call underneath came from. A
            // server verified a *call*; nothing verified "this call is a test", and
            // verification keys off provenance — an LspVerified stamp here is a
            // claim the edge did not earn.
            source: EdgeSource::Extracted,
        };
        by_pair
            .entry((e.src.0, e.dst.0))
            .and_modify(|kept| {
                if candidate.confidence > kept.confidence {
                    *kept = candidate.clone();
                }
            })
            .or_insert(candidate);
    }
    by_pair.into_values().collect()
}
