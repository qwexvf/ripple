//! Normalized, language-agnostic graph vocabulary.
//!
//! Every language adapter emits these types and nothing else — this is the
//! decoupling seam. Layers above `ir` never learn which language produced a node.
//! See docs/04-architecture.md.

use serde::{Deserialize, Serialize};

/// Stable symbol identity. See docs/04-architecture.md "Symbol identity rules":
/// keyed on module-relative path + qualified name + signature discriminator so
/// overloads don't collide and file moves don't orphan history. v0 fills the
/// discriminator with the span until signature capture lands in M2.
/// `Ord` is derived so callers can put ids in a total order — the determinism
/// invariant needs a stable tie-break key, not a meaningful ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SymbolId(pub u64);

impl SymbolId {
    /// Stable id from module-relative path + qualified name (FNV-1a, so it's
    /// reproducible across runs/versions — unlike `DefaultHasher`). v0 keys on
    /// (path, qualified_name); a signature discriminator lands in M2 to separate
    /// overloads, and `git log --follow` rename reconciliation in v1.
    pub fn of(module_path: &str, qualified_name: &str) -> SymbolId {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut mix = |bytes: &[u8]| {
            for &b in bytes {
                h ^= b as u64;
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        mix(module_path.as_bytes());
        mix(b"#");
        mix(qualified_name.as_bytes());
        SymbolId(h)
    }

    /// Id of the file-level module node for `module_path`.
    pub fn module(module_path: &str) -> SymbolId {
        SymbolId::of(module_path, "<module>")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

/// The node vocabulary. Python `def`, Gleam `fn`, TS `function` all map here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    File,
    Module,
    Function,
    Method,
    Class,
    Interface,
    Type,
    Enum,
    Field,
    Variable,
    Route,
    Channel,
}

impl NodeKind {
    /// Map a `.scm` capture prefix (`def.function` → `Function`) to a node kind.
    /// The parse layer reads this generically — it does not know any language.
    pub fn from_capture(prefix: &str) -> Option<NodeKind> {
        Some(match prefix {
            "def.function" => NodeKind::Function,
            "def.method" => NodeKind::Method,
            "def.class" => NodeKind::Class,
            "def.interface" => NodeKind::Interface,
            "def.type" => NodeKind::Type,
            "def.enum" => NodeKind::Enum,
            "def.field" => NodeKind::Field,
            "def.variable" => NodeKind::Variable,
            _ => return None,
        })
    }
}

/// Per-symbol risk factors, normalized to [0,1] within a repo. Filled by the
/// git overlay (v3); zero until then. See docs/06-risk-and-queries.md.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct RiskScores {
    /// change frequency (git log, recency-weighted)
    pub churn: f32,
    /// AST complexity (parse-time; not yet populated)
    pub complexity: f32,
    /// heuristic: share of touching commits that look like fixes
    pub bug_density: f32,
    /// author dispersion; stored as (1 - dispersion) so higher = riskier
    pub ownership: f32,
    /// |static dependents ∪ co-change dependents| (query-time; not stored yet)
    pub fanout: f32,
    /// test-edge linkage; higher lowers risk
    pub test_proximity: f32,
    /// blended score (see docs/06)
    pub composite: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: SymbolId,
    pub kind: NodeKind,
    pub name: String,
    pub qualified_name: String,
    /// Module-relative path of the file the symbol lives in.
    pub module_path: String,
    pub span: Span,
    /// Further definition sites of the *same* symbol, in source order.
    ///
    /// One symbol is written more than once in plenty of languages — Elixir and
    /// Erlang function clauses, overloads that share a name, a class reopened in a
    /// second block — and identity is (path, qualified name), so they collapse to one
    /// id. Keeping only `span` meant every definition site but the first was lost,
    /// which silently breaks anything that asks "which symbol contains this line?".
    #[serde(default)]
    pub extra_spans: Vec<Span>,
    pub is_exported: bool,
    /// Overlay-derived risk; zero until the git overlay runs. See docs/06.
    #[serde(default)]
    pub risk: RiskScores,
}

impl Node {
    /// Does any of this symbol's definition sites contain `line` (1-based)?
    pub fn contains_line(&self, line: u32) -> bool {
        self.definition_spans()
            .any(|s| s.start_line <= line && line <= s.end_line)
    }

    /// The span containing `line`, if any — the caller needs its size to pick the
    /// innermost symbol when several contain the same line.
    pub fn containing_span(&self, line: u32) -> Option<Span> {
        self.definition_spans()
            .find(|s| s.start_line <= line && line <= s.end_line)
    }

    /// Every definition site, primary first.
    pub fn definition_spans(&self) -> impl Iterator<Item = Span> + '_ {
        std::iter::once(self.span).chain(self.extra_spans.iter().copied())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgeKind {
    Defines,
    Calls,
    References,
    Imports,
    Implements,
    Extends,
    HttpCall,
    GraphqlCall,
    AsyncCall,
    Emits,
    Tests,
    ChangesWith,
    /// a function reads/writes a persisted DB entity (e.g. an ORM schema/table)
    DbQuery,
}

/// Where an edge's claim came from.
///
/// Provenance is separate from `confidence` on purpose: confidence says how sure
/// we are, `source` says who said so. Determinism is an invariant and language
/// servers answer differently across versions and index states, so a server's
/// answer is persisted with its provenance rather than re-derived per query.
/// See docs/11-lsp-integration.md.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgeSource {
    /// Read out of the AST by a language adapter — the always-available base tier.
    #[default]
    Extracted,
    /// Confirmed or supplied by a language server.
    LspVerified,
    /// Mined from git history, not from code.
    CoChange,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub src: SymbolId,
    pub dst: SymbolId,
    pub kind: EdgeKind,
    /// 1.0 = EXTRACTED, <1.0 = INFERRED / AMBIGUOUS (e.g. 1/N over candidates).
    pub confidence: f32,
    pub site: Span,
    /// Who produced this claim. Defaults to `Extracted`, so graphs written before
    /// provenance existed load unchanged.
    #[serde(default)]
    pub source: EdgeSource,
}
