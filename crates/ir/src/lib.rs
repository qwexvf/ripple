//! Normalized, language-agnostic graph vocabulary.
//!
//! Every language adapter emits these types and nothing else — this is the
//! decoupling seam. Layers above `ir` never learn which language produced a node.
//! See docs/04-architecture.md.

use serde::{Deserialize, Serialize};

pub mod timing;

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
    /// A UI component that is a whole file, not a named declaration inside one — a
    /// Svelte/Vue single-file component. It has no defining AST node carrying its
    /// name (the name is the file), so it is synthesized by the adapter rather than
    /// captured by a `tags.scm` query. Rendering it (`<Child />`) is a call.
    Component,
    Route,
    Channel,
    /// A symbol that lives outside the indexed roots — a package or one of its
    /// exported names (`urql`, `urql.useQuery`). `module_path` is the dep-key,
    /// `qualified_name` is `dep[.symbol]`, `is_exported` is false. Created by the
    /// external-import binding pass so a project call to a dependency has a real
    /// target node instead of resolving to nothing.
    External,
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
            "def.component" => NodeKind::Component,
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
    /// Leading comment / docstring, if the adapter captured one. Searchable text
    /// so a task described in prose ("limit login attempts") can reach a symbol
    /// whose identifier says none of those words. See docs/07 + `query::locate`.
    #[serde(default)]
    pub doc: Option<String>,
    /// Endpoint path this symbol handles, joined from a matched `RouteKey`'s
    /// literal segments (`auth login`). Stamped by cross-service resolution so a
    /// task word like "login" reaches the handler, not just a call named `login`.
    #[serde(default)]
    pub route_path: Option<String>,
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
    /// `src` is a handler reachable only because `dst` declares the route that
    /// mounts it — a router, an endpoint, a schema type block.
    ///
    /// Direction follows the rest of this vocabulary: `src` is the dependent. A
    /// router calls nothing, so without this edge the file that governs every
    /// route in a service has a fanout of zero and sinks to the bottom of every
    /// review. See #54.
    Serves,
}

// ── cross-service vocabulary ────────────────────────────────────────────────
//
// A call in service A that reaches a handler in service B has no static call edge
// crossing the process boundary. Both sides are therefore normalized into one key
// space and matched by lookup — the fixed vocabulary every framework detector maps
// onto, the way every language maps onto NodeKind/EdgeKind above. See docs/10.

/// What kind of boundary a cross-service key crosses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Transport {
    Http,
    Graphql,
    Grpc,
    /// method-name-keyed RPC (JSON-RPC and kin)
    Rpc,
    PubSub,
    Db,
}

/// One normalized piece of a route/topic/selection path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Segment {
    Literal(String),
    /// a route parameter (`:id`, `{id}`) or a consumer interpolation (`${x}`) —
    /// both sides normalize to this, so a template matches a parameterized route
    Param,
    /// catch-all (`*`, `/**`): matches the rest of the path, including nothing
    Wildcard,
}

/// The normalized key both sides of a boundary reduce to, so matching is a lookup
/// rather than fuzzy guessing. Producers (routes, schema fields, topics) and
/// consumers (fetch calls, documents, publishes) emit the identical structure.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RouteKey {
    pub transport: Transport,
    /// `GET`/`POST` for HTTP; gRPC/RPC method name; `None` where the transport has
    /// no method axis (pub/sub topics, DB entities, GraphQL — its root scope is the
    /// first path segment instead).
    pub method: Option<String>,
    /// URL segments, GraphQL selection path, topic split on `.`, or a DB entity
    /// name. Wire-format spelling: each detector normalizes its own framework's
    /// conventions before emitting, so the matcher never learns a framework name.
    pub path: Vec<Segment>,
}

impl RouteKey {
    /// Does a consumer's key reach this producer key?
    ///
    /// `Literal` must equal `Literal`, `Param` matches exactly one segment from
    /// either side, `Wildcard` matches the whole rest. Transports and methods must
    /// agree exactly — a `player` mutation must never match a `player` query.
    pub fn matches(&self, consumer: &RouteKey) -> bool {
        self.transport == consumer.transport
            && self.method == consumer.method
            && segments_match(&self.path, &consumer.path)
    }
}

fn segments_match(producer: &[Segment], consumer: &[Segment]) -> bool {
    match (producer.first(), consumer.first()) {
        (None, None) => true,
        (Some(Segment::Wildcard), _) | (_, Some(Segment::Wildcard)) => true,
        (Some(a), Some(b)) => {
            let head_ok = match (a, b) {
                (Segment::Literal(x), Segment::Literal(y)) => x == y,
                // whatever is left pairs a `Param` with one segment, which matches;
                // `Wildcard` never reaches here, the arm above consumed it
                _ => true,
            };
            head_ok && segments_match(&producer[1..], &consumer[1..])
        }
        _ => false,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(s: &str) -> Segment {
        Segment::Literal(s.to_owned())
    }

    fn key(transport: Transport, method: Option<&str>, path: Vec<Segment>) -> RouteKey {
        RouteKey {
            transport,
            method: method.map(str::to_owned),
            path,
        }
    }

    /// The whole point of the normalized key: a parameterized route and an
    /// interpolated consumer template meet in one key space.
    #[test]
    fn a_param_matches_one_segment_from_either_side() {
        let route = key(
            Transport::Http,
            Some("GET"),
            vec![lit("users"), Segment::Param],
        );
        let call = key(
            Transport::Http,
            Some("GET"),
            vec![lit("users"), Segment::Param],
        );
        assert!(route.matches(&call));
        // literal on the consumer side still matches the route's param
        let literal_call = key(Transport::Http, Some("GET"), vec![lit("users"), lit("42")]);
        assert!(route.matches(&literal_call));
        // but a param never swallows two segments
        let deeper = key(
            Transport::Http,
            Some("GET"),
            vec![lit("users"), lit("42"), lit("posts")],
        );
        assert!(!route.matches(&deeper));
    }

    #[test]
    fn method_and_transport_are_exact() {
        let get = key(Transport::Http, Some("GET"), vec![lit("health")]);
        let post = key(Transport::Http, Some("POST"), vec![lit("health")]);
        assert!(
            !get.matches(&post),
            "a player mutation must not match a player query"
        );
        let topic = key(Transport::PubSub, None, vec![lit("health")]);
        assert!(!get.matches(&topic));
    }

    #[test]
    fn a_wildcard_matches_the_rest_including_nothing() {
        let route = key(
            Transport::Http,
            Some("GET"),
            vec![lit("assets"), Segment::Wildcard],
        );
        assert!(route.matches(&key(
            Transport::Http,
            Some("GET"),
            vec![lit("assets"), lit("a"), lit("b")]
        )));
        assert!(route.matches(&key(Transport::Http, Some("GET"), vec![lit("assets")])));
        assert!(!route.matches(&key(Transport::Http, Some("GET"), vec![lit("other")])));
    }
}
