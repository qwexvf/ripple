//! TypeScript / TSX adapter (v0, Tier 0).
//!
//! Two flavours, because tree-sitter ships two grammars: JSX nodes exist only in
//! the TSX one. Parsing `.tsx` with the plain TypeScript grammar left every JSX
//! body as an error node, so anything a component rendered was invisible — the
//! rendered component itself, and any call inside a JSX expression.

use crate::{resolve_import, LanguageAdapter, Workspace};
use std::path::{Path, PathBuf};

/// Is this definition named by a separate `export { … }` statement?
///
/// `function Input() {…}` followed by `export { Input };` is the shadcn/ui
/// convention — 24 files in one real app — and the ancestor walk cannot see it,
/// because the export is a sibling statement, not a parent. Without this the
/// component is not in the export table, so no importer can resolve it and a
/// component's callers come back empty.
fn exported_by_a_list(def: tree_sitter::Node, src: &[u8]) -> bool {
    let Some(name) = def
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
    else {
        return false;
    };
    // top-level statements only: a nested `export` is not a module export
    let mut program = def;
    while let Some(parent) = program.parent() {
        program = parent;
    }
    let mut cursor = program.walk();
    let statements: Vec<tree_sitter::Node> = program
        .named_children(&mut cursor)
        .filter(|n| n.kind() == "export_statement")
        .collect();
    statements
        .into_iter()
        .any(|stmt| names_in_export_clause(stmt, src).any(|n| n == name))
}

/// The *local* names an `export { a, b as c }` statement re-exports. `b as c`
/// yields `b`: the local definition is what is being marked exported (the alias a
/// consumer imports under is issue #1).
fn names_in_export_clause<'a>(
    stmt: tree_sitter::Node<'a>,
    src: &'a [u8],
) -> impl Iterator<Item = &'a str> + 'a {
    let mut cursor = stmt.walk();
    let specifiers: Vec<tree_sitter::Node<'a>> = stmt
        .named_children(&mut cursor)
        .filter(|n| n.kind() == "export_clause")
        .flat_map(|clause| {
            let mut c = clause.walk();
            clause
                .named_children(&mut c)
                .filter(|n| n.kind() == "export_specifier")
                .collect::<Vec<_>>()
        })
        .collect();
    specifiers.into_iter().filter_map(move |spec| {
        spec.child_by_field_name("name")
            .and_then(|n| n.utf8_text(src).ok())
    })
}

/// Every extension an import from TypeScript may land on, regardless of which
/// flavour is doing the importing.
const TS_FAMILY: &[&str] = &["*.ts", "*.tsx", "*.mts", "*.cts"];

/// Which grammar (and therefore which file set) this instance serves.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Flavour {
    Ts,
    Tsx,
}

pub struct Adapter {
    flavour: Flavour,
}

impl Adapter {
    /// Plain TypeScript (`.ts`, `.mts`, `.cts`).
    pub fn new() -> Self {
        Adapter {
            flavour: Flavour::Ts,
        }
    }

    /// TSX (`.tsx`) — the grammar that knows JSX.
    pub fn tsx() -> Self {
        Adapter {
            flavour: Flavour::Tsx,
        }
    }
}

impl Default for Adapter {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAdapter for Adapter {
    /// Distinct ids: queries are compiled once per id, and the two flavours compile
    /// against different grammars. Both are TypeScript to a language server, which
    /// is why `lsp::defaults` lists the same command for each.
    fn id(&self) -> &'static str {
        match self.flavour {
            Flavour::Ts => "typescript",
            Flavour::Tsx => "tsx",
        }
    }

    fn grammar(&self) -> tree_sitter::Language {
        match self.flavour {
            Flavour::Ts => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Flavour::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        }
    }

    fn file_globs(&self) -> &'static [&'static str] {
        match self.flavour {
            Flavour::Ts => &["*.ts", "*.mts", "*.cts"],
            Flavour::Tsx => &["*.tsx"],
        }
    }

    fn is_test_path(&self, rel: &str) -> bool {
        let file = rel.rsplit('/').next().unwrap_or(rel);
        let named = file.contains(".test.") || file.contains(".spec.");
        // every other adapter honours a directory convention too, and vitest's
        // default `include` covers `test/**` and `tests/**` with no suffix at all
        let dir = ["__tests__/", "test/", "tests/", "spec/"]
            .iter()
            .any(|d| rel.starts_with(d) || rel.contains(&format!("/{d}")));
        named || dir
    }

    fn tags_query(&self) -> &'static str {
        include_str!("queries/tags.scm")
    }

    fn imports_query(&self) -> Option<&'static str> {
        Some(include_str!("queries/imports.scm"))
    }

    fn refs_query(&self) -> Option<&'static str> {
        Some(match self.flavour {
            Flavour::Ts => include_str!("queries/refs.scm"),
            // JSX patterns are only valid against the TSX grammar
            Flavour::Tsx => concat!(
                include_str!("queries/refs.scm"),
                include_str!("queries/refs-jsx.scm")
            ),
        })
    }

    fn bindings_query(&self) -> Option<&'static str> {
        Some(include_str!("queries/bindings.scm"))
    }

    fn resolve_import(&self, spec: &str, from_file: &Path, ws: &Workspace) -> Option<PathBuf> {
        // Probe the whole family, not this flavour's globs: a `.tsx` module importing
        // a `.ts` one is the common case, and probing only `*.tsx` silently resolved
        // nothing — every aliased import in a TSX file disappeared.
        let globs = TS_FAMILY;
        resolve_import::relative(spec, from_file, globs)
            .or_else(|| resolve_import::tsconfig_paths(spec, ws, globs))
            .or_else(|| resolve_import::workspace_package(spec, ws, globs))
    }

    fn extract_cross(&self, root: tree_sitter::Node, src: &[u8]) -> crate::cross::CrossFacts {
        crate::cross::typescript(root, src)
    }

    /// Exported if an ancestor is an `export_statement` (covers `export fn/class/
    /// const`, `export default`), or if a top-level `export { … }` list names it.
    /// A class member is never a module export.
    fn is_exported(&self, def: tree_sitter::Node, src: &[u8]) -> bool {
        let mut cur = def.parent();
        while let Some(n) = cur {
            match n.kind() {
                "export_statement" => return true,
                "class_body" | "interface_body" | "enum_body" | "object_type"
                | "statement_block" | "object" | "arguments" => return false,
                _ => {}
            }
            cur = n.parent();
        }
        exported_by_a_list(def, src)
    }

    /// Methods/fields are qualified by their enclosing type (`Class.method`).
    fn qualified_name(
        &self,
        kind: ir::NodeKind,
        name: &str,
        def: tree_sitter::Node,
        src: &[u8],
    ) -> String {
        use ir::NodeKind::{Field, Method};
        if matches!(kind, Method | Field) {
            if let Some(ty) = enclosing_type_name(def, src) {
                return format!("{ty}.{name}");
            }
        }
        name.to_owned()
    }
}

/// Name of the class/interface/enum enclosing a member definition, if any.
fn enclosing_type_name(node: tree_sitter::Node, src: &[u8]) -> Option<String> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        match n.kind() {
            "class_declaration"
            | "abstract_class_declaration"
            | "interface_declaration"
            | "enum_declaration" => {
                return n
                    .child_by_field_name("name")
                    .and_then(|name| name.utf8_text(src).ok())
                    .map(str::to_owned);
            }
            _ => {}
        }
        cur = n.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use ir::NodeKind;

    fn parse(flavour: Flavour, src: &str) -> tree_sitter::Tree {
        let adapter = Adapter { flavour };
        let mut p = tree_sitter::Parser::new();
        p.set_language(&adapter.grammar()).expect("ts grammar");
        p.parse(src, None).expect("parse")
    }

    /// What `extract_defs` would build, minus the parse crate: every `@def.*`
    /// capture as `(kind, qualified name)`, in document order.
    ///
    /// Document order matters and is not sorted away: two patterns matching one
    /// node are resolved by "first kind wins" downstream, so a test that sorted
    /// would not see a reordering that reclassifies every arrow-assigned function.
    fn captured(src: &str) -> Vec<(String, String)> {
        captured_in(Flavour::Ts, src)
    }

    fn captured_in(flavour: Flavour, src: &str) -> Vec<(String, String)> {
        let adapter = Adapter { flavour };
        let lang = adapter.grammar();
        let query = tree_sitter::Query::new(&lang, adapter.tags_query()).expect("tags.scm");
        let tree = parse(flavour, src);
        let bytes = src.as_bytes();
        let names = query.capture_names();
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), bytes);
        let mut out = Vec::new();
        while let Some(m) = streaming_iterator::StreamingIterator::next(&mut matches) {
            let mut def: Option<(NodeKind, tree_sitter::Node)> = None;
            let mut name = None;
            for cap in m.captures {
                let cap_name = names[cap.index as usize];
                if cap_name == "name" {
                    name = cap.node.utf8_text(bytes).ok().map(str::to_owned);
                } else if let Some(k) = NodeKind::from_capture(cap_name) {
                    def = Some((k, cap.node));
                }
            }
            let (Some((kind, node)), Some(name)) = (def, name) else {
                continue;
            };
            out.push((
                format!("{kind:?}"),
                adapter.qualified_name(kind, &name, node, bytes),
            ));
        }
        out
    }

    fn has(caps: &[(String, String)], kind: &str, qname: &str) -> bool {
        caps.iter().any(|(k, n)| k == kind && n == qname)
    }

    #[test]
    fn queries_compile() {
        for adapter in [Adapter::new(), Adapter::tsx()] {
            let lang = adapter.grammar();
            tree_sitter::Query::new(&lang, adapter.tags_query()).expect("tags.scm");
            tree_sitter::Query::new(&lang, adapter.imports_query().expect("imports"))
                .expect("imports.scm");
            tree_sitter::Query::new(&lang, adapter.refs_query().expect("refs")).expect("refs.scm");
            tree_sitter::Query::new(&lang, adapter.bindings_query().expect("bindings"))
                .expect("bindings.scm");
        }
    }

    /// `const load = () => …` matches the arrow pattern *and* the general
    /// variable one, and the two captures share a SymbolId. Both must be emitted:
    /// `resolve::index_defs` upgrades the merged node Variable → Function, and a
    /// function is a call container while a variable is not — drop the function
    /// capture and every arrow-assigned function stops being a caller (#53).
    ///
    /// Which of the two arrives first is a property of the query engine, not of
    /// the language, which is why the upgrade is unconditional rather than
    /// order-dependent. Pinned here so a future reordering that *does* change it
    /// is visible.
    #[test]
    fn an_arrow_bound_const_is_captured_as_both_a_function_and_a_variable() {
        let caps = captured("const load = () => 1;\nconst plain = 2;\n");
        assert_eq!(
            caps,
            [
                ("Variable".to_owned(), "load".to_owned()),
                ("Function".to_owned(), "load".to_owned()),
                ("Variable".to_owned(), "plain".to_owned()),
            ]
        );
    }

    /// `enum Color { Red, Green = 2 }` yielded the enum and nothing else: a bare
    /// member is a lone `property_identifier` under the body and an initialized
    /// one an `enum_assignment`, and the `enum_declaration` pattern reaches
    /// neither. `Color.Red` had no node for a reference to resolve to.
    #[test]
    fn enum_members_are_captured_in_both_spellings() {
        let caps = captured("export enum Color { Red, Green = 2 }\n");
        assert!(has(&caps, "Enum", "Color"), "got {caps:?}");
        assert!(has(&caps, "Field", "Color.Red"), "got {caps:?}");
        assert!(has(&caps, "Field", "Color.Green"), "got {caps:?}");
    }

    /// A `const enum` is the same node kind; its members were missing too.
    #[test]
    fn const_enum_members_are_captured() {
        let caps = captured("export const enum Flag { On = 1 }\n");
        assert!(has(&caps, "Field", "Flag.On"), "got {caps:?}");
    }

    /// `interface Foo { bar(): void; baz: string }` captured only `Foo`.
    /// Members are qualified by the enclosing interface, so they cannot collide
    /// with a same-named member of another interface in the file.
    #[test]
    fn interface_members_are_captured_and_qualified() {
        let caps = captured(
            "export interface Foo { bar(): void; baz: string }\ninterface Other { baz: number }\n",
        );
        assert!(has(&caps, "Method", "Foo.bar"), "got {caps:?}");
        assert!(has(&caps, "Field", "Foo.baz"), "got {caps:?}");
        assert!(has(&caps, "Field", "Other.baz"), "got {caps:?}");
    }

    /// Call/construct/index signatures have no addressable name — capturing them
    /// would invent symbols named after the interface's own braces.
    #[test]
    fn unnamed_interface_signatures_are_not_captured() {
        let caps = captured(
            "interface F { (x: string): void; new (x: number): F; [k: string]: unknown }\n",
        );
        assert_eq!(caps, [("Interface".to_owned(), "F".to_owned())]);
    }

    /// The same two node kinds make up every anonymous `object_type`. Capturing
    /// them unanchored would turn an inline parameter type into file-scope
    /// fields — `verbose` here is not a symbol anything can reference.
    #[test]
    fn inline_object_types_do_not_produce_fields() {
        let caps = captured("export function run(opts: { verbose: boolean }): void {}\n");
        assert_eq!(caps, [("Function".to_owned(), "run".to_owned())]);
    }

    /// `export abstract class` is `abstract_class_declaration`, a different node
    /// kind, so the class produced no node at all — while `qualified_name`
    /// already qualified its members by it, leaving `Repo.run` owned by nothing.
    #[test]
    fn an_abstract_class_and_its_signatures_are_captured() {
        let caps =
            captured("export abstract class Repo {\n  abstract run(): void;\n  concrete() {}\n}\n");
        assert!(has(&caps, "Class", "Repo"), "got {caps:?}");
        assert!(has(&caps, "Method", "Repo.run"), "got {caps:?}");
        assert!(has(&caps, "Method", "Repo.concrete"), "got {caps:?}");
    }

    /// A `declare class` member is a `method_signature`, which neither the
    /// `method_definition` nor the `public_field_definition` pattern matches.
    #[test]
    fn bodiless_class_members_are_captured() {
        let caps = captured("declare class Api { fetch(): void; static make(): Api; }\n");
        assert!(has(&caps, "Class", "Api"), "got {caps:?}");
        assert!(has(&caps, "Method", "Api.fetch"), "got {caps:?}");
        assert!(has(&caps, "Method", "Api.make"), "got {caps:?}");
    }

    /// Getters, setters and statics are all `method_definition` — this pins that
    /// the plain pattern already covers them, so nobody adds a redundant one.
    /// `#private` members are a separate node kind and did need adding: without
    /// them every call in a private method fell through to the file.
    #[test]
    fn accessors_statics_and_private_members_are_all_methods() {
        let caps = captured(
            "class C {\n  static make(): C { return new C(); }\n  get size() { return 1; }\n  set size(v: number) {}\n  #secret() {}\n  #count = 0;\n}\n",
        );
        assert!(has(&caps, "Method", "C.make"), "got {caps:?}");
        assert!(has(&caps, "Method", "C.size"), "got {caps:?}");
        assert!(has(&caps, "Method", "C.#secret"), "got {caps:?}");
        assert!(has(&caps, "Field", "C.#count"), "got {caps:?}");
    }

    /// #53 one level in. The shorthand `load` was already a `method_definition`;
    /// the arrow-valued `save` was only reachable as the `api` variable, so a
    /// generated client's methods collapsed into a single node.
    #[test]
    fn arrow_valued_object_properties_are_captured_like_their_shorthand_siblings() {
        let caps = captured(
            "export const api = {\n  load() {},\n  save: () => 1,\n  legacy: function () {},\n  version: 3,\n};\n",
        );
        assert!(has(&caps, "Method", "load"), "got {caps:?}");
        assert!(has(&caps, "Method", "save"), "got {caps:?}");
        assert!(has(&caps, "Method", "legacy"), "got {caps:?}");
        // a non-callable property is a value, not a symbol calls can be attributed to
        assert!(!has(&caps, "Method", "version"), "got {caps:?}");
    }

    /// The same `pair` shape is every inline callback in a hook or config
    /// argument. Capturing those would merge unrelated handlers that happen to
    /// share a name into one node whose span then swallows their callers.
    #[test]
    fn inline_callback_properties_are_not_captured() {
        let caps = captured(
            "function View() {\n  useQuery({ onSuccess: () => 1 });\n  register({ onSuccess: () => 2 });\n}\n",
        );
        assert_eq!(caps, [("Function".to_owned(), "View".to_owned())]);
    }

    /// `declare function` has no body, so it parses as `function_signature` —
    /// every entry point of a hand-written `.d.ts` was invisible.
    #[test]
    fn ambient_function_declarations_are_captured() {
        let caps = captured(
            "declare function ambient(x: number): void;\nexport declare function exported(): void;\n",
        );
        assert!(has(&caps, "Function", "ambient"), "got {caps:?}");
        assert!(has(&caps, "Function", "exported"), "got {caps:?}");
    }

    /// Inside `declare module` / `declare namespace` the signature sits in the
    /// module's statement block, bare or under an `export`.
    #[test]
    fn signatures_inside_ambient_modules_are_captured() {
        let caps = captured(
            "declare module \"m\" { export function mfn(): void; function helper(): void; }\ndeclare namespace N { export function nfn(): void; }\n",
        );
        assert!(has(&caps, "Function", "mfn"), "got {caps:?}");
        assert!(has(&caps, "Function", "helper"), "got {caps:?}");
        assert!(has(&caps, "Function", "nfn"), "got {caps:?}");
    }

    /// A `function_signature` is also how an overload declaration parses, and an
    /// overload shares its implementation's SymbolId. The signature comes first
    /// in document order, so an unanchored pattern would hand the primary span
    /// to a bodiless node and stop attributing the body's calls to the function.
    #[test]
    fn overload_signatures_do_not_shadow_their_implementation() {
        let caps = captured(
            "export function pick(a: string): void;\nexport function pick(a: number): void;\nexport function pick(a: any): void {}\n",
        );
        assert_eq!(caps, [("Function".to_owned(), "pick".to_owned())]);
    }

    /// `export default function` is an ordinary `function_declaration` under an
    /// `export_statement` — this pins that it is neither dropped nor duplicated.
    #[test]
    fn export_default_function_is_captured_once_and_marked_exported() {
        let src = "export default function main() {}\n";
        let caps = captured(src);
        assert_eq!(caps, [("Function".to_owned(), "main".to_owned())]);

        let tree = parse(Flavour::Ts, src);
        let decl = tree
            .root_node()
            .named_child(0)
            .and_then(|e| e.named_child(0))
            .expect("function_declaration");
        assert!(Adapter::new().is_exported(decl, src.as_bytes()));
    }

    /// Members of a type are scoped to it, not to the module: `Color.Red` is not
    /// something an importer can name.
    #[test]
    fn type_members_are_never_module_exports() {
        let src = "export enum Color { Red }\nexport interface Foo { bar(): void }\n";
        let tree = parse(Flavour::Ts, src);
        let bytes = src.as_bytes();
        let adapter = Adapter::new();
        let mut member_count = 0;
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            let mut c = node.walk();
            stack.extend(node.named_children(&mut c));
            if !matches!(node.kind(), "property_identifier") {
                continue;
            }
            member_count += 1;
            assert!(
                !adapter.is_exported(node, bytes),
                "{} read as a module export",
                node.utf8_text(bytes).unwrap_or_default()
            );
        }
        assert_eq!(
            member_count, 2,
            "fixture stopped covering both member kinds"
        );
    }

    /// Both grammars are fed the same `tags.scm`; a pattern naming a node kind
    /// the TSX grammar spells differently would fail to compile there only.
    #[test]
    fn the_tsx_grammar_captures_the_same_definitions() {
        let src = "export enum Color { Red }\nexport interface Foo { bar(): void }\nexport abstract class R { abstract run(): void; }\n";
        assert_eq!(
            captured_in(Flavour::Ts, src),
            captured_in(Flavour::Tsx, src)
        );
    }
}
