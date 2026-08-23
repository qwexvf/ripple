//! Cross-service facts extracted from a file's AST at parse time (tree-sitter,
//! no regex). Stored on `FileExtract` so the index parses each file once; the
//! `resolve` layer only matches/links these facts. See docs/10-cross-service-resolution.md.

use ir::{RouteKey, Segment, Transport};
use serde::{Deserialize, Serialize};
use tree_sitter::Node as TsNode;

/// Per-file cross-service facts, in the vocabulary every detector maps onto.
///
/// Nothing here names a framework. A detector reads its own framework's shapes and
/// emits `Provides`/`Consumes` keyed by `RouteKey`; the linker matches keys and
/// never learns what produced them. That is the whole point of #32 — Absinthe used
/// to be spelled out in `resolve`, which made adding a second framework a core
/// change rather than a file.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CrossFacts {
    /// Boundary endpoints this file serves: a resolver, a route handler, a topic
    /// subscriber.
    pub provides: Vec<Provides>,
    /// Boundary endpoints this file calls. Empty until an HTTP/RPC consumer
    /// detector lands (#32 phase 3) — GraphQL consumers travel as `graphql`
    /// because a document is a protocol object, not a single key.
    pub consumes: Vec<Consumes>,
    /// GraphQL protocol facts: documents, fragments, scope includes, references.
    pub graphql: GraphqlFacts,
    /// Modules whose functions this file may call *unqualified* (Elixir's
    /// `import`). Generic because the shape is: "names from over there are in
    /// scope here".
    pub star_imports: Vec<String>,
    /// Calls that name their target module: (module FQN, function, line).
    pub qualified_calls: Vec<(String, String, u32)>,
    /// References to a persisted entity: (entity module FQN, line).
    pub entity_refs: Vec<(String, u32)>,
    /// This file declares a persisted entity (an Ecto `schema`, an ORM model).
    pub entity_def: bool,
}

impl CrossFacts {
    pub fn is_empty(&self) -> bool {
        self.provides.is_empty()
            && self.consumes.is_empty()
            && self.graphql.is_empty()
            && self.star_imports.is_empty()
            && self.qualified_calls.is_empty()
            && self.entity_refs.is_empty()
            && !self.entity_def
    }

    /// Fold another file-region's facts in. Used when a file carries an embedded
    /// language (a `.vue`/`.svelte` `<script>` block): the host and each region
    /// are extracted separately, then their facts join into the one file's set.
    pub fn merge(&mut self, other: CrossFacts) {
        self.provides.extend(other.provides);
        self.consumes.extend(other.consumes);
        self.graphql.merge(other.graphql);
        self.star_imports.extend(other.star_imports);
        self.qualified_calls.extend(other.qualified_calls);
        self.entity_refs.extend(other.entity_refs);
        self.entity_def |= other.entity_def;
    }
}

/// What serves a boundary key: a named function, or — when the framework names no
/// single function — the module that answers for it. Module granularity is worth
/// less than a function and is priced that way, but it is not nothing: 138 of 142
/// dataloader resolvers on one real schema name only a module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HandlerRef {
    Function {
        module: String,
        name: String,
    },
    Module(String),
    /// The file that declared it. Some frameworks put the handler in an anonymous
    /// position — a closure in a config object — where there is no symbol to name;
    /// the declaring file is then the honest granularity, the same answer a call
    /// outside every function gets.
    Here,
}

/// One endpoint this file serves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provides {
    pub key: RouteKey,
    pub handler: HandlerRef,
    /// Where the declaration is written, so the linker can attribute it to the
    /// symbol that encloses it rather than to the whole file (#54). 0 when the
    /// detector has no line to give — a spec file has no symbols anyway.
    ///
    /// No `serde(default)`, the same convention as `FileExtract::reexports`: a row
    /// written before this field existed must be a cache miss rather than a silent
    /// zero. Adding the default here is what broke #56.
    pub line: u32,
    /// For transports with a type graph (GraphQL): what this field returns, as the
    /// schema spells it, so a nested selection can be descended. `None` elsewhere.
    pub returns: Option<String>,
}

/// One endpoint this file calls.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Consumes {
    pub key: RouteKey,
    pub line: u32,
    /// How much of the key was literal. A `fetch(`/api/${id}`)` pins less than a
    /// fully spelled path, and the linker prices the edge accordingly.
    pub confidence_hint: f32,
}

/// GraphQL travels as a protocol rather than as bare keys: a document names
/// operations, operations spread fragments, and a schema pulls fields between
/// scopes. All of it is wire-format — no framework names.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct GraphqlFacts {
    /// Root fields requested by the operations in a document.
    pub operations: Vec<GqlOp>,
    /// Fragment definitions.
    pub fragments: Vec<GqlFragment>,
    /// Fragment spreads inside operations. Most nested selections in a
    /// codegen-based app are written in fragments, so an unexpanded spread hides
    /// them all.
    pub spreads: Vec<GqlSpread>,
    /// `(importing scope, included scope)` — a schema declaring root fields in one
    /// block and pulling them into another. Resolved at link time because the
    /// included block usually lives in another file.
    pub scope_includes: Vec<(String, String)>,
    /// Operation names referenced from code (codegen's `<Name>Document`).
    pub op_refs: Vec<String>,
}

impl GraphqlFacts {
    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
            && self.fragments.is_empty()
            && self.spreads.is_empty()
            && self.scope_includes.is_empty()
            && self.op_refs.is_empty()
    }

    /// Fold another region's GraphQL facts in (see `CrossFacts::merge`).
    pub fn merge(&mut self, other: GraphqlFacts) {
        self.operations.extend(other.operations);
        self.fragments.extend(other.fragments);
        self.spreads.extend(other.spreads);
        self.scope_includes.extend(other.scope_includes);
        self.op_refs.extend(other.op_refs);
    }
}

/// The wire spelling of an operation name.
///
/// Codegen writes `query currentPlayer` as `CurrentPlayerDocument`, so the document
/// and the code that references it disagree on the first letter. Both sides
/// normalize here, at extraction — the linker compares wire names and never learns
/// that a casing convention exists (#32). Keying on the raw name lost 11 of 242
/// operations on one real frontend.
pub fn operation_key(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Split a URL path into wire segments: `/users/:id/posts` → `users`, `Param`,
/// `posts`. Both `:id` (Phoenix, Rails) and `{id}` (OpenAPI) are parameters, and
/// `*rest` is a catch-all — a detector normalizes its own framework's spelling
/// here rather than teaching the matcher three of them.
pub fn http_path(path: &str) -> Vec<Segment> {
    path.split('/')
        .filter(|s| !s.is_empty())
        .map(|seg| {
            if seg.starts_with(':') || (seg.starts_with('{') && seg.ends_with('}')) {
                Segment::Param
            } else if seg.starts_with('*') {
                Segment::Wildcard
            } else {
                Segment::Literal(seg.to_owned())
            }
        })
        .collect()
}

/// An HTTP endpoint key. The method is upper-cased so `get` and `GET` are one axis.
pub fn http_key(method: &str, path: &str) -> RouteKey {
    RouteKey {
        transport: Transport::Http,
        method: Some(method.to_uppercase()),
        path: http_path(path),
    }
}

/// The key a whole module mounted at a path answers under (`socket "/socket", Mod`,
/// `forward "/graphql", Absinthe.Plug`). No method: a mount answers every verb, and
/// everything below the path. See #54.
pub fn mount_key(path: &str) -> RouteKey {
    let mut path = http_path(path);
    path.push(Segment::Wildcard);
    RouteKey {
        transport: Transport::Http,
        method: None,
        path,
    }
}

/// The key a persisted table is declared and read under. A migration creating
/// `table(:games)` and a schema declaring `schema "games"` are the two sides. See #54.
pub fn db_key(table: &str) -> RouteKey {
    RouteKey {
        transport: Transport::Db,
        method: None,
        path: vec![Segment::Literal(table.to_owned())],
    }
}

/// The key a GraphQL field is served and requested under: its scope, then its
/// name. One function so the producer and the consumer cannot spell it differently
/// — they used to be two tuple literals in two crates.
pub fn graphql_field_key(scope: &str, field: &str) -> RouteKey {
    RouteKey {
        transport: Transport::Graphql,
        method: None,
        path: vec![
            Segment::Literal(scope.to_owned()),
            Segment::Literal(field.to_owned()),
        ],
    }
}

/// Read a GraphQL key back as `(scope, field)`. `None` for any other shape.
pub fn graphql_scope_field(key: &RouteKey) -> Option<(&str, &str)> {
    match (key.transport, key.path.as_slice()) {
        (Transport::Graphql, [Segment::Literal(scope), Segment::Literal(field)]) => {
            Some((scope, field))
        }
        _ => None,
    }
}

/// One root field of one GraphQL operation — the consumer side of the join.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GqlOp {
    /// Operation name in wire spelling (`operation_key`), so it matches the name
    /// the consuming code references however the document spelled it.
    pub name: String,
    /// Root scope the field is selected on: `query` | `mutation` | `subscription`.
    /// Part of the join key — a `player` mutation must not match a `player` query.
    pub scope: String,
    /// Root field name, as written in the document (camelCase).
    pub field: String,
    /// Field names from the root down to this selection, root first. `["lfgPosts",
    /// "author"]` for `lfgPosts { author { … } }`. A nested selection has a resolver
    /// too, and matching only the root field missed every one of them.
    pub path: Vec<String>,
}

/// A fragment definition: a named, reusable selection on one type.
///
/// `fragment LfgPostFields on LfgPost { author { name } }`. The type condition names
/// the scope its fields live in directly, so an expanded spread needs no descent —
/// which is why fragments are cheaper to follow than nested selections were.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GqlFragment {
    pub name: String,
    /// The type it applies to, as written (`LfgPost`).
    pub type_condition: String,
    /// Field paths selected inside it, relative to the type condition.
    pub paths: Vec<Vec<String>>,
    /// Fragments it spreads in turn, with the path each occurs at.
    pub spreads: Vec<(Vec<String>, String)>,
}

/// A `...FragmentName` in an operation, and where in the selection it appears.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GqlSpread {
    /// Operation name the spread belongs to.
    pub op: String,
    /// Root scope of that operation: `query` | `mutation` | `subscription`.
    pub scope: String,
    /// Selection path the spread sits at, root first. Empty = spread at the top level.
    pub at: Vec<String>,
    pub fragment: String,
}

/// Root scopes a GraphQL document's operation can name. A root field must be
/// declared in (or imported into) one of these.
pub const GQL_ROOT_SCOPES: [&str; 3] = ["query", "mutation", "subscription"];

fn text<'a>(n: TsNode, src: &'a [u8]) -> &'a str {
    n.utf8_text(src).unwrap_or("")
}

/// The GraphQL source inside a `graphql(`…`)` / `gql(`…`)` call — the
/// graphql-codegen client-preset shape. `None` unless the callee is one of those
/// tags and its first argument is a template string; then the template's inner text
/// (without the backticks) is returned so it can be parsed as GraphQL.
fn gql_call_source(call: TsNode, src: &[u8]) -> Option<String> {
    let func = call.child_by_field_name("function")?;
    if !matches!(text(func, src), "graphql" | "gql") {
        return None;
    }
    let args = call.child_by_field_name("arguments")?;
    let mut c = args.walk();
    let tmpl = args
        .named_children(&mut c)
        .find(|a| a.kind() == "template_string")?;
    Some(text(tmpl, src).trim_matches('`').to_owned())
}

/// Parse a chunk of GraphQL source (an inline template body) with the GraphQL
/// grammar and return its operation/fragment facts. Empty on any parse error — a
/// template with `${…}` interpolation may not parse cleanly, and a partial answer
/// is better than none. Reuses the same collector `.gql` files go through, so an
/// inline operation and a codegen `.gql` one produce identical facts.
fn parse_gql_source(source: &str) -> GraphqlFacts {
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&crate::graphql_language()).is_err() {
        return GraphqlFacts::default();
    }
    let Some(tree) = parser.parse(source, None) else {
        return GraphqlFacts::default();
    };
    graphql(tree.root_node(), source.as_bytes()).graphql
}

// ── TypeScript: <Name>Document usages ──
pub fn typescript(root: TsNode, src: &[u8]) -> CrossFacts {
    let mut docs = std::collections::HashSet::new();
    let mut consumes = Vec::new();
    let mut provides = Vec::new();
    let mut gql_sources: Vec<String> = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "identifier" {
            if let Some(op) = text(n, src).strip_suffix("Document") {
                if !op.is_empty() {
                    docs.insert(operation_key(op));
                }
            }
        }
        if n.kind() == "call_expression" {
            if let Some(call) = http_call(n, src) {
                consumes.push(call);
            }
            provides.extend(file_route(n, src));
            // graphql-codegen client-preset: `graphql(`query Posts { … }`)`. The
            // operation is inline with no `<Name>Document` identifier, so it must be
            // parsed out of the template — otherwise the file that actually runs the
            // query contributes nothing, and a backend change flags the generated
            // `graphql.ts` instead of the real caller.
            if let Some(gql) = gql_call_source(n, src) {
                gql_sources.push(gql);
            }
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    // inline `graphql(`…`)` operations, parsed with the GraphQL grammar. Merged as
    // real operations (name + selected fields), and their names added to op_refs, so
    // this file both *supplies* the operation's fields and *consumes* it — the
    // linker keys op_refs to op_fields, and a codegen `.gql` file would have given
    // both halves across two files (#32, client-preset dogfood finding).
    let mut gql = GraphqlFacts {
        op_refs: docs.into_iter().collect(),
        ..Default::default()
    };
    for source in &gql_sources {
        let facts = parse_gql_source(source);
        for op in &facts.operations {
            // an anonymous operation has no name to key a cross-service match on
            if !op.name.is_empty() {
                gql.op_refs.push(op.name.clone());
            }
        }
        gql.merge(facts);
    }
    gql.op_refs.sort();
    gql.op_refs.dedup();
    consumes.sort_by_key(|c: &Consumes| c.line);
    provides
        .sort_by(|a: &Provides, b: &Provides| format!("{:?}", a.key).cmp(&format!("{:?}", b.key)));
    CrossFacts {
        provides,
        consumes,
        graphql: gql,
        ..Default::default()
    }
}

/// `fetch("/api/users")`, `axios.get(`/api/users/${id}`)` — the consumer side of an
/// HTTP boundary.
///
/// The method is read, never guessed: `axios.<verb>` spells it, and a bare `fetch`
/// is GET by specification. A `fetch` whose options name a method ripple cannot
/// read statically produces nothing rather than a wrong verb.
/// Routes a file-based router declares in source: TanStack Start's
/// `createFileRoute("/auth/session")({ server: { handlers: { GET: … } } })`.
///
/// The path is written down, so this reads syntax rather than guessing from where
/// the file sits — and the methods are the keys of `handlers`, so they are read
/// too. A route whose path is computed produces nothing.
fn file_route(call: TsNode, src: &[u8]) -> Vec<Provides> {
    let Some(callee) = call.child_by_field_name("function") else {
        return Vec::new();
    };
    // `createFileRoute("/p")(config)`: the callee is itself the call that names the path
    if callee.kind() != "call_expression" {
        return Vec::new();
    }
    let named = callee
        .child_by_field_name("function")
        .map(|f| text(f, src) == "createFileRoute")
        .unwrap_or(false);
    if !named {
        return Vec::new();
    }
    let Some(path) = first_string_arg(callee, src) else {
        return Vec::new();
    };
    let Some(args) = call.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut arg_cursor = args.walk();
    let Some(config) = args.named_children(&mut arg_cursor).next() else {
        return Vec::new();
    };
    let Some(handlers) =
        object_value(config, "server", src).and_then(|s| object_value(s, "handlers", src))
    else {
        return Vec::new();
    };
    let mut cursor = handlers.walk();
    handlers
        .named_children(&mut cursor)
        .filter_map(|pair| {
            let key = pair.child_by_field_name("key")?;
            let method = text(key, src).trim_matches(['"', '\'']);
            HTTP_VERBS
                .contains(&method.to_lowercase().as_str())
                .then(|| Provides {
                    key: http_key(method, &path),
                    handler: HandlerRef::Here,
                    line: pair.start_position().row as u32 + 1,
                    returns: None,
                })
        })
        .collect()
}

/// The first string argument of a call, unquoted.
fn first_string_arg(call: TsNode, src: &[u8]) -> Option<String> {
    let args = call.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let arg = args.named_children(&mut cursor).next()?;
    (arg.kind() == "string").then(|| text(arg, src).trim_matches(['"', '\'']).to_owned())
}

/// The value of `name:` in an object literal, if it is itself an object.
fn object_value<'a>(object: TsNode<'a>, name: &str, src: &[u8]) -> Option<TsNode<'a>> {
    if object.kind() != "object" {
        return None;
    }
    let mut cursor = object.walk();
    // bound to a local so the child iterator (which borrows `cursor`) drops before
    // `cursor` does — a bare tail expression would extend its borrow past the block
    let found = object.named_children(&mut cursor).find_map(|pair| {
        let key = pair.child_by_field_name("key")?;
        (text(key, src).trim_matches(['"', '\'']) == name)
            .then(|| pair.child_by_field_name("value"))?
    });
    found
}

fn http_call(call: TsNode, src: &[u8]) -> Option<Consumes> {
    let callee = call.child_by_field_name("function")?;
    let args = call.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let mut arg_list = args.named_children(&mut cursor);
    let first = arg_list.next()?;

    // a request described by an options object: `client({ url: `/x`, method: "GET" })`.
    // Every generated client spells it this way (orval, openapi-codegen with a custom
    // instance), and it is a shape rather than a library name.
    if first.kind() == "object" {
        return request_object(call, first, src);
    }

    let path = url_of(first, src)?;

    let method = match callee.kind() {
        "identifier" if text(callee, src) == "fetch" => fetch_method(arg_list.next(), src)?,
        "member_expression" => {
            let verb = text(callee.child_by_field_name("property")?, src);
            let object = text(callee.child_by_field_name("object")?, src);
            // an axios instance, however it was named. Deliberately narrow: `http.get`
            // is also how MSW spells a *mock handler*, and reading those would invent
            // a client for every stubbed endpoint
            if !object.to_lowercase().contains("axios") {
                return None;
            }
            if !HTTP_VERBS.contains(&verb) {
                return None;
            }
            verb.to_owned()
        }
        _ => return None,
    };

    let key = http_key(&method, &path);
    Some(Consumes {
        confidence_hint: literal_share(&key),
        key,
        line: call.start_position().row as u32 + 1,
    })
}

const HTTP_VERBS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];

/// `client({ url: "/x", method: "GET" })` — both written down, or nothing.
fn request_object(call: TsNode, options: TsNode, src: &[u8]) -> Option<Consumes> {
    let url = object_string(options, "url", src)?;
    let method_node = object_string(options, "method", src)?;
    let method = text(method_node, src)
        .trim_matches(['"', '\'', '`'])
        .to_owned();
    if !HTTP_VERBS.contains(&method.to_lowercase().as_str()) {
        return None;
    }
    let key = http_key(&method, &url_of(url, src)?);
    Some(Consumes {
        confidence_hint: literal_share(&key),
        key,
        line: call.start_position().row as u32 + 1,
    })
}

/// The node behind `name:` in an object literal.
fn object_string<'a>(object: TsNode<'a>, name: &str, src: &[u8]) -> Option<TsNode<'a>> {
    if object.kind() != "object" {
        return None;
    }
    let mut cursor = object.walk();
    // bound to a local so the child iterator (which borrows `cursor`) drops before
    // `cursor` does — a bare tail expression would extend its borrow past the block
    let found = object.named_children(&mut cursor).find_map(|pair| {
        let key = pair.child_by_field_name("key")?;
        (text(key, src).trim_matches(['"', '\'']) == name)
            .then(|| pair.child_by_field_name("value"))?
    });
    found
}

/// A `fetch`'s method: GET unless its options object says otherwise in a literal.
/// `None` means the options were dynamic — under-link rather than assume GET.
fn fetch_method(options: Option<TsNode>, src: &[u8]) -> Option<String> {
    let Some(options) = options else {
        return Some("GET".to_owned()); // one argument: GET, per the fetch spec
    };
    if options.kind() != "object" {
        return None;
    }
    let mut cursor = options.walk();
    for pair in options.named_children(&mut cursor) {
        let (Some(key), Some(value)) = (
            pair.child_by_field_name("key"),
            pair.child_by_field_name("value"),
        ) else {
            continue;
        };
        if text(key, src).trim_matches(['"', '\'']) != "method" {
            continue;
        }
        return match value.kind() {
            "string" => Some(text(value, src).trim_matches(['"', '\'', '`']).to_owned()),
            _ => None, // a computed method is not a method we know
        };
    }
    Some("GET".to_owned()) // options that say nothing about the method
}

/// How much of a key the consumer actually spelled. Counted over the *segments the
/// key ends up with*, not over the pieces the template was written in: a
/// two-fragment template producing four segments is three-quarters literal, and
/// saying one half would understate what the call pins.
fn literal_share(key: &RouteKey) -> f32 {
    if key.path.is_empty() {
        return 1.0;
    }
    let literal = key
        .path
        .iter()
        .filter(|s| matches!(s, Segment::Literal(_)))
        .count();
    literal as f32 / key.path.len() as f32
}

/// The URL a call's first argument names.
///
/// A template literal keeps its literal text and turns each interpolation into a
/// placeholder, which `http_path` normalizes to `Param` — the same shape a route
/// declares with `:id`.
fn url_of(arg: TsNode, src: &[u8]) -> Option<String> {
    match arg.kind() {
        "string" => {
            let path = text(arg, src).trim_matches(['"', '\'']).to_owned();
            path.starts_with('/').then_some(path)
        }
        "template_string" => {
            let mut path = String::new();
            let mut parts = 0usize;
            let mut cursor = arg.walk();
            for part in arg.children(&mut cursor) {
                match part.kind() {
                    "string_fragment" => {
                        path.push_str(text(part, src));
                        parts += 1;
                    }
                    "template_substitution" => {
                        path.push_str("/:param/");
                        parts += 1;
                    }
                    _ => {}
                }
            }
            let path = path.replace("//", "/");
            (path.starts_with('/') && parts > 0).then_some(path)
        }
        _ => None,
    }
}

// ── GraphQL: operation → root fields ──
pub fn graphql(root: TsNode, src: &[u8]) -> CrossFacts {
    let mut facts = CrossFacts::default();
    collect_gql(root, src, &mut facts);
    facts
}

/// Where a selection walk puts what it finds. Operations and fragment bodies are the
/// same shape of walk with different destinations, so they share one walker.
enum Sink<'a> {
    Operation {
        op: &'a str,
        scope: &'a str,
        ops: &'a mut Vec<GqlOp>,
        spreads: &'a mut Vec<GqlSpread>,
    },
    Fragment {
        paths: &'a mut Vec<Vec<String>>,
        spreads: &'a mut Vec<(Vec<String>, String)>,
    },
}

impl Sink<'_> {
    fn field(&mut self, path: &[String], name: &str) {
        match self {
            Sink::Operation { op, scope, ops, .. } => ops.push(GqlOp {
                name: operation_key(op),
                scope: (*scope).to_owned(),
                field: name.to_owned(),
                path: path.to_vec(),
            }),
            Sink::Fragment { paths, .. } => paths.push(path.to_vec()),
        }
    }

    fn spread(&mut self, path: &[String], fragment: &str) {
        match self {
            Sink::Operation {
                op, scope, spreads, ..
            } => spreads.push(GqlSpread {
                op: operation_key(op),
                scope: (*scope).to_owned(),
                at: path.to_vec(),
                fragment: fragment.to_owned(),
            }),
            Sink::Fragment { spreads, .. } => spreads.push((path.to_vec(), fragment.to_owned())),
        }
    }
}

/// Walk a selection set, emitting one `GqlOp` per selected field with its full path.
///
/// Recursive because a nested selection is a field on another type, with its own
/// resolver — flattening to the root field is what made those resolvers unreachable.
fn collect_selections(set: TsNode, src: &[u8], path: &mut Vec<String>, sink: &mut Sink) {
    let mut sc = set.walk();
    for sel in set.named_children(&mut sc) {
        if sel.kind() != "selection" {
            continue;
        }
        let Some(inner) = sel.named_child(0) else {
            continue;
        };
        match inner.kind() {
            // `...Name` — the fields are in the fragment, resolved at link time
            "fragment_spread" => {
                if let Some(name) = named_child_text(inner, "fragment_name", src) {
                    sink.spread(path, &name);
                }
            }
            "field" => {
                let mut fc = inner.walk();
                let children: Vec<TsNode> = inner.named_children(&mut fc).collect();
                let Some(name) = children
                    .iter()
                    .find(|n| n.kind() == "name")
                    .map(|n| text(*n, src).to_owned())
                else {
                    continue;
                };
                path.push(name.clone());
                sink.field(path, &name);
                for nested in children.iter().filter(|n| n.kind() == "selection_set") {
                    collect_selections(*nested, src, path, sink);
                }
                path.pop();
            }
            // an inline fragment (`... on Type { … }`) selects on a different type;
            // not followed yet, and skipped rather than mis-attributed to this one
            _ => {}
        }
    }
}

/// Text of the first named child of `kind`, one level down (`fragment_name (name)`).
fn named_child_text(node: TsNode, kind: &str, src: &[u8]) -> Option<String> {
    let mut c = node.walk();
    let child = node.named_children(&mut c).find(|n| n.kind() == kind)?;
    let mut cc = child.walk();
    let name = child
        .named_children(&mut cc)
        .find(|n| n.kind() == "name")
        .unwrap_or(child);
    Some(text(name, src).to_owned())
}

fn collect_gql(node: TsNode, src: &[u8], out: &mut CrossFacts) {
    if node.kind() == "fragment_definition" {
        let name = named_child_text(node, "fragment_name", src);
        let mut c = node.walk();
        let children: Vec<TsNode> = node.named_children(&mut c).collect();
        let type_condition = children
            .iter()
            .find(|n| n.kind() == "type_condition")
            .and_then(|n| named_child_text(*n, "named_type", src));
        let set = children.iter().find(|n| n.kind() == "selection_set");
        if let (Some(name), Some(type_condition), Some(set)) = (name, type_condition, set) {
            let mut fragment = GqlFragment {
                name,
                type_condition,
                paths: Vec::new(),
                spreads: Vec::new(),
            };
            let mut sink = Sink::Fragment {
                paths: &mut fragment.paths,
                spreads: &mut fragment.spreads,
            };
            collect_selections(*set, src, &mut Vec::new(), &mut sink);
            out.graphql.fragments.push(fragment);
        }
    }
    if node.kind() == "operation_definition" {
        let mut op_name = None;
        let mut sel_set = None;
        // shorthand (`{ field }`) has no operation_type and means query
        let mut scope = "query";
        let mut c = node.walk();
        for ch in node.children(&mut c) {
            match ch.kind() {
                "operation_type" => scope = text(ch, src),
                "name" if op_name.is_none() => op_name = Some(text(ch, src).to_owned()),
                "selection_set" => sel_set = Some(ch),
                _ => {}
            }
        }
        if let (Some(op), Some(set)) = (op_name, sel_set) {
            let mut sink = Sink::Operation {
                op: &op,
                scope,
                ops: &mut out.graphql.operations,
                spreads: &mut out.graphql.spreads,
            };
            collect_selections(set, src, &mut Vec::new(), &mut sink);
        }
    }
    let mut c = node.walk();
    for ch in node.children(&mut c) {
        collect_gql(ch, src, out);
    }
}

// ── Elixir: aliases, schema fields, entity decls, remote calls, data refs ──
/// Reads the file's macro shapes generically (`elixir::macros`), then projects
/// them onto these facts with the per-framework tables in `elixir::dsl` — no
/// framework name appears in the walker itself.
pub fn elixir(root: TsNode, src: &[u8]) -> CrossFacts {
    crate::elixir::dsl::cross_facts(&crate::elixir::macros::scan(root, src))
}

/// snake_case atom → camelCase (Absinthe LanguageConventions default).
/// The inverse of `camelize`: a GraphQL type name to the atom Absinthe declares it
/// under (`LfgPost` → `lfg_post`). A fragment's type condition is written the first
/// way and the schema's object scope the second, so a spread cannot be followed
/// without it.
pub fn decamelize(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

pub fn camelize(snake: &str) -> String {
    let mut out = String::with_capacity(snake.len());
    let mut upper = false;
    for (i, ch) in snake.chars().enumerate() {
        if ch == '_' {
            upper = true;
        } else if upper && i != 0 {
            out.extend(ch.to_uppercase());
            upper = false;
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::LanguageAdapter;
    use tree_sitter::Parser;

    fn parse(lang: tree_sitter::Language, src: &str) -> tree_sitter::Tree {
        let mut p = Parser::new();
        p.set_language(&lang).unwrap();
        p.parse(src, None).unwrap()
    }

    #[test]
    fn camelize_matches_absinthe() {
        assert_eq!(camelize("current_player"), "currentPlayer");
        assert_eq!(camelize("player"), "player");
    }

    fn elixir_facts_of(src: &str) -> CrossFacts {
        let t = parse(crate::elixir::Adapter::new().grammar(), src);
        elixir(t.root_node(), src.as_bytes())
    }

    /// `(scope, field, handler)` for every GraphQL field a file provides.
    fn provided(f: &CrossFacts) -> Vec<(&str, &str, &HandlerRef)> {
        f.provides
            .iter()
            .filter_map(|p| {
                let (scope, field) = graphql_scope_field(&p.key)?;
                Some((scope, field, &p.handler))
            })
            .collect()
    }

    fn function(module: &str, name: &str) -> HandlerRef {
        HandlerRef::Function {
            module: module.into(),
            name: name.into(),
        }
    }

    /// Every entity argument counts as a reference, not just the first: a joined
    /// table and a preloaded association are as much a dependency as the primary
    /// one. Non-entity modules ride along harmlessly — the link step keeps only
    /// modules that actually declared a `schema "table"`.
    #[test]
    fn data_refs_cover_every_entity_argument() {
        let f = elixir_facts_of("defmodule S do\n  def q(id) do\n    from p in Player, join: t in Team, where: p.id == ^id\n    Repo.preload(p, Game)\n    Repo.get(Player, id)\n  end\nend\n");
        let refs: Vec<&str> = f.entity_refs.iter().map(|(m, _)| m.as_str()).collect();
        for entity in ["Player", "Team", "Game"] {
            assert!(refs.contains(&entity), "missing {entity} in {refs:?}");
        }
    }

    #[test]
    fn elixir_facts() {
        let f = elixir_facts_of("defmodule S do\n  alias App.Resolvers.PlayerResolver\n  query do\n    field :current_player, :player do\n      resolve(&PlayerResolver.me/3)\n    end\n  end\n  def show(a) do\n    Players.get_player(a)\n    Repo.get(Player, a)\n    from p in Team\n    %Role{}\n  end\nend\n");
        // a provider carries the RESOLVED FQN (alias expanded at extraction)
        assert_eq!(
            provided(&f),
            vec![(
                "query",
                "currentPlayer",
                &function("App.Resolvers.PlayerResolver", "me")
            )]
        );
        // wire spelling: the name a document writes, not the schema's atom
        assert_eq!(f.provides[0].returns.as_deref(), Some("Player"));
        assert!(f
            .qualified_calls
            .iter()
            .any(|(m, fu, _)| m == "Players" && fu == "get_player"));
        let schemas: Vec<&str> = f.entity_refs.iter().map(|(m, _)| m.as_str()).collect();
        assert!(
            schemas.contains(&"Player") && schemas.contains(&"Team") && schemas.contains(&"Role")
        );
    }

    /// `field :x, :t, resolve: &M.f/3` — the keyword spelling, previously missed.
    #[test]
    fn absinthe_keyword_form_resolve() {
        let f = elixir_facts_of("defmodule S do\n  mutation do\n    field :follow_player, :player, resolve: &PlayerResolver.follow/3\n  end\nend\n");
        assert_eq!(
            provided(&f),
            vec![(
                "mutation",
                "followPlayer",
                &function("PlayerResolver", "follow")
            )]
        );
    }

    /// Same field name on two types must stay distinguishable by scope.
    #[test]
    fn absinthe_scope_separates_same_named_fields() {
        let f = elixir_facts_of("defmodule S do\n  query do\n    field :name, :string, resolve: &Root.name/3\n  end\n  object :player do\n    field :name, :string, resolve: &Player.name/3\n  end\nend\n");
        let scopes: Vec<(&str, &HandlerRef)> = provided(&f)
            .into_iter()
            .map(|(scope, _, handler)| (scope, handler))
            .collect();
        assert_eq!(
            scopes,
            vec![
                ("query", &function("Root", "name")),
                ("Player", &function("Player", "name")),
            ]
        );
    }

    #[test]
    fn absinthe_import_fields() {
        let f = elixir_facts_of("defmodule S do\n  query do\n    import_fields(:player_queries)\n  end\n  mutation do\n    import_fields :player_mutations\n  end\nend\n");
        assert_eq!(
            f.graphql.scope_includes,
            vec![
                ("query".into(), "PlayerQueries".into()),
                ("mutation".into(), "PlayerMutations".into()),
            ]
        );
    }

    /// `resolve: dataloader(M)` / `resolve: fn -> ... end` name no resolver
    /// function — under-link rather than invent one. Ecto columns aren't fields.
    #[test]
    fn absinthe_non_function_resolvers_are_dropped() {
        let f = elixir_facts_of("defmodule S do\n  object :player do\n    field :team, :team, resolve: dataloader(App.Teams)\n    field :rank, :integer, resolve: fn p, _, _ -> Stats.rank(p) end\n  end\n  schema \"players\" do\n    field :name, :string\n  end\nend\n");
        let named: Vec<_> = provided(&f)
            .into_iter()
            .filter(|(_, _, h)| matches!(h, HandlerRef::Function { .. }))
            .collect();
        assert!(named.is_empty(), "unexpected named resolvers: {named:?}");
        // the dataloader field is still served, one level coarser
        assert_eq!(
            provided(&f),
            vec![("Player", "team", &HandlerRef::Module("App.Teams".into()))]
        );
        // the inline fn's body is still visible as a qualified call
        assert!(f
            .qualified_calls
            .iter()
            .any(|(m, fu, _)| m == "Stats" && fu == "rank"));
    }

    #[test]
    fn elixir_schema_decl() {
        let src = "defmodule Player do\n  use Ecto.Schema\n  schema \"players\" do\n  end\nend\n";
        let t = parse(crate::elixir::Adapter::new().grammar(), src);
        assert!(elixir(t.root_node(), src.as_bytes()).entity_def);
    }

    #[test]
    fn gql_facts() {
        let src = "query Player($id: ID) { player(playerId: $id) { name } }\nmutation Follow { followPlayer { id } }\n";
        let t = parse(crate::graphql_language(), src);
        let ops = graphql(t.root_node(), src.as_bytes()).graphql.operations;
        // a nested selection is a field on another type with a resolver of its own, so
        // it is reported with the path that reaches it (issue #22)
        let seen: Vec<(&str, &str, Vec<&str>)> = ops
            .iter()
            .map(|o| {
                (
                    o.scope.as_str(),
                    o.field.as_str(),
                    o.path.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(
            seen,
            vec![
                ("query", "player", vec!["player"]),
                ("query", "name", vec!["player", "name"]),
                ("mutation", "followPlayer", vec!["followPlayer"]),
                ("mutation", "id", vec!["followPlayer", "id"]),
            ]
        );
    }

    fn ts_consumes(src: &str) -> Vec<(Option<String>, Vec<Segment>, f32)> {
        let t = parse(crate::typescript::Adapter::new().grammar(), src);
        typescript(t.root_node(), src.as_bytes())
            .consumes
            .into_iter()
            .map(|c| (c.key.method, c.key.path, c.confidence_hint))
            .collect()
    }

    /// The consumer side of an HTTP boundary: the method is read rather than
    /// guessed, and an interpolation normalizes to the same `Param` a route
    /// declares with `:id`.
    #[test]
    fn a_fetch_and_an_axios_call_become_route_keys() {
        let lit = |s: &str| Segment::Literal(s.to_owned());

        assert_eq!(
            ts_consumes("fetch(\"/api/users\");"),
            vec![(Some("GET".into()), vec![lit("api"), lit("users")], 1.0)]
        );
        assert_eq!(
            ts_consumes("fetch(\"/api/users\", { method: \"POST\" });"),
            vec![(Some("POST".into()), vec![lit("api"), lit("users")], 1.0)]
        );
        assert_eq!(
            ts_consumes("axios.get(`/api/users/${id}`);"),
            vec![(
                Some("GET".into()),
                vec![lit("api"), lit("users"), Segment::Param],
                // two of three segments are spelled out, which is what the key pins
                2.0 / 3.0
            )]
        );

        // a generated client describes the request in an object; both parts read
        assert_eq!(
            ts_consumes("customInstance({ url: `/api/v1/users/${id}`, method: \"POST\" }, opts);"),
            vec![(
                Some("POST".into()),
                vec![lit("api"), lit("v1"), lit("users"), Segment::Param],
                0.75
            )]
        );
        // MSW spells a mock handler `http.get("*/health", …)`; reading those would
        // invent a client for every stub
        assert!(ts_consumes("http.get(\"*/health\", handler);").is_empty());

        // a method ripple cannot read is not a method it invents
        assert!(ts_consumes("fetch(url, { method: verb });").is_empty());
        // and neither is a library it has not been taught
        assert!(ts_consumes("superagent.get(\"/api/users\");").is_empty());
        // a relative URL names no route the router declares
        assert!(ts_consumes("fetch(\"users\");").is_empty());
    }

    #[test]
    fn ts_facts() {
        let src = "import { PlayerDocument, TeamDocument } from \"@/g\";\nuseQuery({query: PlayerDocument});\n";
        let t = parse(crate::typescript::Adapter::new().grammar(), src);
        let docs = typescript(t.root_node(), src.as_bytes()).graphql.op_refs;
        assert!(docs.contains(&"Player".to_string()) && docs.contains(&"Team".to_string()));
    }

    /// graphql-codegen client-preset: the operation lives inside a `graphql(`…`)`
    /// call with no `<Name>Document` identifier, so the file that runs the query
    /// must still contribute the operation as a consumer fact. See #32 and the
    /// dogfood finding on a real urql frontend.
    #[test]
    fn ts_client_preset_operations() {
        let src = "const PostsQuery = graphql(`\n  query Posts { posts { id } }\n`);\n\
                   const M = graphql(`mutation CreatePost($t: String!) { createPost { id } }`);\n\
                   const Anon = graphql(`query { me { id } }`);\n";
        let t = parse(crate::typescript::Adapter::new().grammar(), src);
        let docs = typescript(t.root_node(), src.as_bytes()).graphql.op_refs;
        assert!(docs.contains(&"Posts".to_string()), "named query: {docs:?}");
        assert!(
            docs.contains(&"CreatePost".to_string()),
            "named mutation: {docs:?}"
        );
        // an anonymous operation has no name to key a cross-service match on
        assert_eq!(docs.len(), 2, "anonymous op is not keyed: {docs:?}");
    }

    #[test]
    fn gql_tag_alias_is_also_detected() {
        let src = "const q = gql(`query Feed { feed { id } }`);\n";
        let t = parse(crate::typescript::Adapter::new().grammar(), src);
        let docs = typescript(t.root_node(), src.as_bytes()).graphql.op_refs;
        assert_eq!(docs, vec!["Feed".to_string()]);
    }

    /// Client-preset fragment masking: fragments and their spreads are the harder
    /// half of a codegen app — most nested selections live in fragments. An inline
    /// `graphql(`fragment … `)` must yield the same fragment/spread facts a `.gql`
    /// file would, so a spread still expands to its resolver at link time (#87).
    #[test]
    fn ts_client_preset_fragments_and_spreads() {
        let src = "const F = graphql(`fragment PlayerFields on Player { id name }`);\n\
                   const Q = graphql(`query P { currentPlayer { ...PlayerFields } }`);\n";
        let t = parse(crate::typescript::Adapter::new().grammar(), src);
        let g = typescript(t.root_node(), src.as_bytes()).graphql;
        assert!(
            g.fragments.iter().any(|f| f.name == "PlayerFields"),
            "fragment extracted: {:?}",
            g.fragments.iter().map(|f| &f.name).collect::<Vec<_>>()
        );
        assert!(g.op_refs.contains(&"P".to_string()), "op: {:?}", g.op_refs);
        assert_eq!(
            g.spreads.len(),
            1,
            "the fragment spread inside the operation"
        );
    }
}
