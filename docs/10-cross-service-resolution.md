# 10 — Cross-service resolution (call-site ↔ route matching)

This is the piece review flagged as hand-waved (finding #5). It's the hardest static-analysis problem in the spec *and* the highest-value one: a call in service A that reaches a handler in service B has **no static call edge** crossing the process boundary, yet that boundary is exactly where a change's blast radius escapes a single service. codebase-memory-mcp proves it's tractable — it ships `HTTP_CALLS`/`GRAPHQL_CALLS`/`ASYNC_CALLS`/`EMITS` edges with `url_path`, `confidence`, `strategy`, `via`, plus `Route`/`Channel` nodes carrying `method`/`key_path`/`broker`. This doc specifies how ripple does it.

## Shape of the problem

Two halves, matched by a normalized key:

```
producer side (service B)          consumer side (service A)
────────────────────────          ────────────────────────
@app.get("/users/:id")            fetch(`/api/users/${id}`)
grpc: rpc GetUser(...)            client.GetUser(...)
type Query { user(id): User }     useQuery(GET_USER)
@subscribe("orders.paid")         publish("orders.paid", ...)
        │                                  │
        ▼ Route / Channel node             ▼ pending CrossCall
        └──────────── match on RouteKey ───────────┘
                   → HttpCall / GraphqlCall / AsyncCall / Emits edge (+confidence)
```

**Separation of concerns that keeps it language-agnostic above the seam:** *extraction* (finding routes and calls) is framework-specific and lives in detectors; *normalization + matching* is a single language-agnostic pass. A TS frontend and a Go backend meet in the same `RouteKey` space.

## The normalized key

Both sides normalize to the identical structure so matching is a lookup, not fuzzy guessing:

```rust
// crate: ir
pub enum Transport { Http, Graphql, Grpc, PubSub }

pub struct RouteKey {
    pub transport: Transport,
    pub method: Option<String>,   // "GET"/"POST"; gRPC method name; None for pub/sub
    pub path: PathTemplate,       // normalized, see below
    pub service: Option<ServiceId>, // resolved from base-url/config when available
}

// PathTemplate normalizes BOTH sides identically:
//   "/users/:id"           → ["users", PARAM]
//   `/api/users/${id}`     → ["api","users", PARAM]   (prefix handled separately)
//   "/users/123"           → ["users", PARAM]         (literal-looking dynamic segment)
//   "orders.paid"          → ["orders","paid"]        (topic, dot-split)
pub struct PathTemplate(pub Vec<Segment>);  // Segment = Literal(String) | Param | Wildcard
```

Normalization rules (applied to both producer and consumer):
- Route params (`:id`, `{id}`, `<id>`) → `Param`.
- Consumer interpolations (`${x}`, `"+"` concat, f-strings) → `Param` (walk the template-literal / binary-expr AST to keep literal segments, replace expression segments).
- Trailing slashes stripped; case-normalized where the framework is case-insensitive.
- Catch-all (`*`, `/**`) → `Wildcard`.

## Extraction: the `FrameworkDetector` seam (Tier 3)

Referenced from `LanguageAdapter::detectors()` in [`04`](04-architecture.md). One detector per framework, not per language — matching is shared:

```rust
// crate: lang
pub trait FrameworkDetector: Send + Sync {
    fn id(&self) -> &str;                           // "express", "fastapi", "grpc-go", "gleam-wisp", "urql"
    fn transport(&self) -> Transport;
    // producer side → Route/Channel nodes with a RouteKey
    fn detect_routes(&self, file: &ParsedFile, out: &mut IrBuilder);
    // consumer side → pending CrossCall {RouteKey (best-effort), call site, confidence_hint}
    fn detect_calls(&self, file: &ParsedFile, out: &mut IrBuilder);
}
```

A detector is mostly a tree-sitter query + a small normalizer — e.g. Express: match `app.<method>(<path>, ...)` and `router.<method>(...)`; FastAPI: match `@app.get(...)` / `@router.get(...)` decorators; gRPC: parse `.proto` service/rpc; GraphQL: parse SDL `type Query/Mutation` fields + client operation documents.

## Prefix and base-URL resolution (the part that makes it hard)

Raw paths are almost never the full path. Two corrections, both explicit:

1. **Mount prefixes (producer).** `app.use("/api", router)` means every route in `router` is prefixed `/api`. Track mount points as edges and compose the full path when emitting the `Route` node. Nested mounts compose transitively.
2. **Base URL (consumer).** `fetch(API_BASE + "/users")` where `API_BASE` comes from env/config. Resolve via the existing `CONFIGURES` edge (codebase-memory has `CONFIGURES {config_key}`) to a `ServiceId`. When the base is unresolvable, the edge is still emitted but at **lower confidence** and without a `service` (path-only match).

## Matching algorithm (language-agnostic pass in a `linker` step)

Runs after all files are parsed (cross-file, cross-package, cross-language), and incrementally on change:

```
1. Index routes into buckets keyed by (transport, method, segment_count).
2. For each pending CrossCall:
     candidates = bucket[(transport, method, seg_count)]
     filter by segment match: Literal==Literal, Param matches anything, Wildcard matches rest
     if service known on both sides, require service match (raises confidence)
3. Emit edge(s):
     1 candidate  → single edge
     N candidates → N edges each at confidence/N (AMBIGUOUS; consistent with 04's candidate rule)
     0 candidates → no cross edge (fall back to co-change; see below)
```

### Confidence model

| Situation | confidence |
|---|---|
| exact literal path + method + resolved service | ~0.95 |
| path with normalized params, service resolved | ~0.8 |
| path match but base-URL/service unresolved (path-only) | ~0.6 |
| heavy dynamic construction (few literal segments) | ~0.4, `AMBIGUOUS` |
| multiple route candidates | split `1/N` across them |

Confidence flows into `impact_weight` ([`06`](06-risk-and-queries.md)) exactly like any other edge, so a shaky cross-service link contributes proportionally less to blast radius. `explain_edge` returns the matched `RouteKey`, the strategy used, and the candidate set for auditability.

## The co-change safety net (why partial resolution is fine)

Cross-service is precisely where static resolution is weakest (dynamic URLs, gateways, service meshes, reflection-based routing) — and precisely where the **git co-change signal is most valuable**. If `serviceA/client.ts` and `serviceB/handler.go` historically change together, the `ChangesWith` edge links them even when no `HttpCall` edge resolves. So cross-service blast radius **degrades gracefully to co-change** instead of going dark. This is the whole thesis applied to its hardest case: shallow static + git fusion beats deep-but-brittle static alone.

## Incremental behavior

Routes and calls are per-file artifacts. On a changed file: re-extract its routes/calls, remove its stale `Route`/`CrossCall` entries, and re-run the linker only for the affected `(transport, method, seg_count)` buckets — not a global re-link.

## Honest limits (stated, not hidden)

Unresolvable statically, by construction — these rely on co-change + explicit config, and are reported as such rather than silently missed:
- Reflection / config-table-driven routing (routes registered from a data structure at runtime).
- API gateways / service meshes that rewrite paths between caller and handler.
- Fully dynamic endpoints (path computed from non-literal, non-config runtime state).
- Cross-repo services not in the indexed workspace (a monorepo boundary; out of scope until multi-repo indexing).

## Roadmap fit

This is Tier-3 work ([`05`](05-language-support.md)), landing in **v3** ([`08`](08-roadmap.md)) after the single-service graph, git overlay, and impact/review queries exist. The `RouteKey`/`FrameworkDetector`/linker contracts are defined now so the IR and store schema reserve space for cross-service edges from the start (they're already in `EdgeKind`).
