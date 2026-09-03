//! PHP adapter — Tier 0 defs, Tier 1 imports, Tier 2 call sites and binding.
//!
//! This started as reachability-engine parity: aegis-reach covers PHP at the
//! import level (does the project `use` this dependency at all), and the engine
//! must match that before aegis-reach can be deleted. Every `use A\B\C;` mints an
//! `External` module node and an `Imports` edge, so `engine::imports(dep)`
//! answers true.
//!
//! What binds today:
//!
//! * **`use` of a first-party class.** [`Adapter::resolve_import`] reads the
//!   nearest `composer.json` and follows its PSR-4 map, so `use App\Support\Util;`
//!   resolves to `src/Support/Util.php` and the importing file gets a real
//!   `Imports` edge to a local module instead of an external stub. A specifier no
//!   PSR-4 prefix covers falls through to [`Adapter::external_dep_key`] unchanged.
//! * **Members, keyed by their class.** [`Adapter::qualified_name`] spells a
//!   method `Class.method`, which is what the resolver's owner and by-class
//!   indexes are keyed on. Without it every same-named method in a repo hashed to
//!   one `SymbolId` (all but one silently dropped) and no member call had an
//!   owner-qualified target at all — see #117.
//! * **Static calls.** `refs.scm` records a `Foo::bar()` scope as the call's
//!   *qualifier* rather than a receiver, so it prefers a `bar` declared on `Foo`
//!   and resolves to nothing at all — rather than to a local look-alike — when
//!   `Foo` is third-party.
//!
//! What still does not: an instance call's receiver type. `$obj->m()` and
//! `self::m()` pin no class the adapter can name, so they resolve against every
//! same-named method in the repo with confidence split `1/N` across them. On
//! guzzle that is where the remaining wrong edges are — a PSR-17
//! `$factory->createRequest()` reaches guzzle's own `createRequest` test helpers.
//! Closing it needs a `bindings_query` over PHP's parameter and property type
//! declarations, which does not exist yet. A dynamic callee (`$fn()`,
//! `$obj->$m()`) is not captured at all, since its name is not in the source.

use crate::{resolve_import, LanguageAdapter, Workspace};
use std::path::{Path, PathBuf};
use tree_sitter::Node;

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

    /// Members are qualified by the class, interface, trait or enum they are
    /// declared in, so `Utils.describeType` and `Stream.describeType` stay
    /// distinct symbols and each has an owner the resolver can key on.
    ///
    /// Unqualified — which is what PHP did before #117 — both consequences bite
    /// at once: every same-named method in the repo hashes to one `SymbolId` and
    /// all but one are dropped, and the owner index (keyed `(root, owner, name)`)
    /// has no PHP entry at all, so `Utils::describeType()` has nothing
    /// owner-qualified to bind to.
    fn qualified_name(&self, kind: ir::NodeKind, name: &str, def: Node, src: &[u8]) -> String {
        match kind {
            ir::NodeKind::Method | ir::NodeKind::Field => match owner_type(def, src) {
                Some(owner) => format!("{owner}.{name}"),
                None => name.to_owned(),
            },
            _ => name.to_owned(),
        }
    }

    /// A `use` of a first-party class names a file on disk, found through
    /// Composer's PSR-4 autoload map: `use App\Support\Util;` under
    /// `{"psr-4": {"App\\": "src/"}}` is `<composer dir>/src/Support/Util.php`.
    ///
    /// Resolving it is what lets a member call bind — without it the whole
    /// namespace collapses to one external dep node (`App`) and the class the
    /// call targets is never linked to the file that defines it. Anything no
    /// PSR-4 prefix covers returns `None` and falls through to
    /// [`Self::external_dep_key`], so a genuine third-party `use` is unchanged.
    fn resolve_import(&self, spec: &str, from: &Path, _ws: &Workspace) -> Option<PathBuf> {
        let ns = spec.trim_start_matches('\\');
        let dir = composer_dir(from)?;
        // longest prefix wins: `App\Support\` must beat `App\` when both map.
        let mut best: Option<(usize, PathBuf)> = None;
        for (prefix, roots) in psr4_map(&dir.join("composer.json")) {
            let Some(rest) = ns.strip_prefix(prefix.as_str()) else {
                continue;
            };
            if best.as_ref().is_some_and(|(len, _)| *len >= prefix.len()) {
                continue;
            }
            let rel = format!("{}.php", rest.replace('\\', "/"));
            let hit = roots
                .iter()
                .map(|r| dir.join(r).join(&rel))
                .find(|p| p.is_file());
            if let Some(p) = hit {
                best = Some((prefix.len(), p));
            }
        }
        let (_, path) = best?;
        path.canonicalize().ok()
    }

    /// The dep-key of a `use` path is its top namespace segment
    /// (`GuzzleHttp\Client` → `GuzzleHttp`). Composer maps namespaces to packages
    /// out of band, so this is the import-level floor, not a package identity.
    ///
    /// Only reached when [`Self::resolve_import`] found no first-party file, so a
    /// `use` of the project's own class no longer lands here.
    fn external_dep_key(&self, spec: &str) -> Option<String> {
        resolve_import::php_dep_key(spec)
    }
}

/// Name of the class/interface/trait/enum a member is declared inside — walk up
/// to the first type declaration and read its `name`. A member of an anonymous
/// class (`new class { … }`) has no named owner and keeps its bare name.
fn owner_type<'a>(def: Node, src: &'a [u8]) -> Option<&'a str> {
    let mut cur = def.parent();
    while let Some(n) = cur {
        if matches!(
            n.kind(),
            "class_declaration"
                | "interface_declaration"
                | "trait_declaration"
                | "enum_declaration"
        ) {
            return n.child_by_field_name("name")?.utf8_text(src).ok();
        }
        cur = n.parent();
    }
    None
}

/// How far up from a source file to look for the `composer.json` that governs
/// it. A PSR-4 root is normally one or two levels above the file, but a deeply
/// namespaced class (`App\A\B\C\D\E`) sits further down; bounded so a file
/// outside any Composer project doesn't walk to `/`.
const COMPOSER_SEARCH_DEPTH: usize = 8;

/// Directory of the nearest `composer.json` at or above `from`.
fn composer_dir(from: &Path) -> Option<PathBuf> {
    let mut dir = from.parent()?;
    for _ in 0..COMPOSER_SEARCH_DEPTH {
        if dir.join("composer.json").is_file() {
            return Some(dir.to_owned());
        }
        dir = dir.parent()?;
    }
    None
}

/// Composer's PSR-4 map as `(namespace prefix, source directories)`, from both
/// `autoload` and `autoload-dev` — a test class is a first-party class too.
///
/// Prefixes keep Composer's trailing `\` so `strip_prefix` can't match `AppFoo`
/// against `App\`. A target may be one directory or a list of them.
///
/// Parsed with `serde_yaml`, already a dependency of this crate: YAML 1.2 is a
/// JSON superset, and it reads the shapes composer.json comes in (tab indents,
/// `\/` escapes, `"App\\"` unescaping to `App\`). Only the *top-level*
/// `autoload` counts — a `repositories` entry can carry an inline package with
/// an `autoload` of its own, which is not this project's map. A file that fails
/// to parse yields no map, so every `use` falls through to the external key.
fn psr4_map(composer_json: &Path) -> Vec<(String, Vec<String>)> {
    let Ok(text) = std::fs::read_to_string(composer_json) else {
        return Vec::new();
    };
    let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(&text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for section in ["autoload", "autoload-dev"] {
        let Some(map) = doc
            .get(section)
            .and_then(|s| s.get("psr-4"))
            .and_then(serde_yaml::Value::as_mapping)
        else {
            continue;
        };
        for (prefix, target) in map {
            let Some(prefix) = prefix.as_str().filter(|p| !p.is_empty()) else {
                continue;
            };
            let dirs = match target {
                serde_yaml::Value::String(s) => vec![s.clone()],
                serde_yaml::Value::Sequence(xs) => xs
                    .iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect(),
                _ => continue,
            };
            if !dirs.is_empty() {
                out.push((prefix.to_owned(), dirs));
            }
        }
    }
    out
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

    /// `Foo::bar()` names the owner `bar` must be declared on, so it is a
    /// qualified call — captured once, not also as a member call.
    #[test]
    fn a_static_call_through_a_class_name_is_a_qualified_call() {
        for (src, qualifier) in [
            ("<?php Foo::bar($x);\n", "Foo"),
            ("<?php Psr7\\Utils::bar($x);\n", "Psr7\\Utils"),
            ("<?php \\Foo\\Bar::bar($x);\n", "\\Foo\\Bar"),
        ] {
            assert_eq!(
                refs(src),
                [
                    ("ref.call".to_owned(), "bar".to_owned()),
                    ("ref.qualifier".to_owned(), qualifier.to_owned()),
                ],
                "{src}"
            );
        }
    }

    /// A scope that pins no class stays on the by-name member path.
    #[test]
    fn a_scope_that_names_no_class_is_recv_plus_member() {
        for (src, recv) in [
            ("<?php self::bar($x);\n", "self"),
            ("<?php parent::bar($x);\n", "parent"),
            ("<?php static::bar($x);\n", "static"),
            ("<?php $class::bar($x);\n", "$class"),
        ] {
            assert_eq!(
                refs(src),
                [
                    ("ref.member".to_owned(), "bar".to_owned()),
                    ("ref.recv".to_owned(), recv.to_owned()),
                ],
                "{src}"
            );
        }
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

    /// Every def capture as `(kind, bare name, qualified name)`, mirroring what
    /// `parse::extract_defs` does with the same query.
    fn captures(src: &str) -> Vec<(String, String, String)> {
        let adapter = Adapter::new();
        let lang = adapter.grammar();
        let query = tree_sitter::Query::new(&lang, adapter.tags_query()).expect("tags.scm");
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang).expect("grammar");
        let tree = parser.parse(src, None).expect("parse");
        let bytes = src.as_bytes();
        let names = query.capture_names();
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), bytes);
        let mut out = Vec::new();
        while let Some(m) = streaming_iterator::StreamingIterator::next(&mut matches) {
            let mut def = None;
            let mut name = None;
            for cap in m.captures {
                let cap_name = names[cap.index as usize];
                if let Some(k) = cap_name.strip_prefix("def.") {
                    def = Some((k.to_owned(), cap.node));
                } else if cap_name == "name" {
                    name = cap.node.utf8_text(bytes).ok().map(str::to_owned);
                }
            }
            let (Some((k, node)), Some(n)) = (def, name) else {
                continue;
            };
            let Some(kind) = ir::NodeKind::from_capture(&format!("def.{k}")) else {
                continue;
            };
            let qn = adapter.qualified_name(kind, &n, node, bytes);
            out.push((k, n, qn));
        }
        out.sort();
        out
    }

    /// Captures keyed by qualified name — the string a `SymbolId` is hashed from,
    /// so this is where two symbols either stay apart or collide.
    fn qualified(src: &str) -> Vec<(String, String)> {
        captures(src).into_iter().map(|(k, _, q)| (k, q)).collect()
    }

    /// The #117 collision: unqualified, both `describeType` methods and the free
    /// function hash to one `SymbolId` and two of the three are dropped.
    #[test]
    fn methods_are_qualified_by_their_class() {
        let qns = qualified(
            "<?php\nclass Utils {\n  public static function describeType($v) {}\n}\nclass Stream {\n  public function describeType($v) {}\n}\nfunction describeType($v) {}\n",
        );
        assert_eq!(
            qns,
            [
                ("class".to_owned(), "Stream".to_owned()),
                ("class".to_owned(), "Utils".to_owned()),
                ("function".to_owned(), "describeType".to_owned()),
                ("method".to_owned(), "Stream.describeType".to_owned()),
                ("method".to_owned(), "Utils.describeType".to_owned()),
            ]
        );
    }

    /// Interfaces, traits and enums own their members the same way a class does.
    #[test]
    fn every_type_declaration_owns_its_methods() {
        let qns = qualified(
            "<?php\ninterface I { public function m(); }\ntrait T { public function m() {} }\nenum E { case A; public function m() {} }\n",
        );
        let methods: Vec<&(String, String)> = qns.iter().filter(|(k, _)| k == "method").collect();
        assert_eq!(
            methods,
            [
                &("method".to_owned(), "E.m".to_owned()),
                &("method".to_owned(), "I.m".to_owned()),
                &("method".to_owned(), "T.m".to_owned()),
            ]
        );
    }

    /// A method of `new class { … }` has no named owner to qualify by, and must
    /// not be dropped or given a bogus one.
    #[test]
    fn an_anonymous_class_method_keeps_its_bare_name() {
        let qns = qualified("<?php\n$x = new class { public function m() {} };\n");
        assert_eq!(qns, [("method".to_owned(), "m".to_owned())]);
    }

    /// A self-cleaning temp directory. `tempfile` is not a dependency of this
    /// crate and Cargo.toml is not ours to grow for a fixture.
    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new() -> ScratchDir {
            static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("ripple-php-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&path).expect("mkdir scratch");
            // the platform temp dir is a symlink on macOS; canonicalize once so
            // every expected path in a test is comparable to a resolved one
            ScratchDir(path.canonicalize().expect("canonicalize scratch"))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A PSR-4 fixture: `composer.json` at the root, one autoload prefix, and a
    /// class file where the prefix says it should be.
    fn psr4_fixture(composer: &str) -> ScratchDir {
        let dir = ScratchDir::new();
        let root = dir.path();
        std::fs::write(root.join("composer.json"), composer).expect("composer.json");
        std::fs::create_dir_all(root.join("src/Support")).expect("mkdir src/Support");
        std::fs::write(root.join("src/Support/Util.php"), "<?php\nclass Util {}\n")
            .expect("Util.php");
        std::fs::create_dir_all(root.join("tests")).expect("mkdir tests");
        std::fs::write(
            root.join("tests/UtilTest.php"),
            "<?php\nclass UtilTest {}\n",
        )
        .expect("UtilTest.php");
        dir
    }

    #[test]
    fn a_psr4_use_resolves_to_the_class_file() {
        let dir = psr4_fixture(r#"{"autoload":{"psr-4":{"App\\":"src/"}}}"#);
        let root = dir.path();
        let importer = root.join("src/Client.php");
        std::fs::write(&importer, "<?php\nuse App\\Support\\Util;\n").expect("importer");

        let adapter = Adapter::new();
        let ws = Workspace::default();
        assert_eq!(
            adapter.resolve_import("App\\Support\\Util", &importer, &ws),
            Some(
                root.join("src/Support/Util.php")
                    .canonicalize()
                    .expect("canonicalize")
            )
        );
        // a fully-qualified `use \App\Support\Util;` names the same class
        assert_eq!(
            adapter.resolve_import("\\App\\Support\\Util", &importer, &ws),
            Some(
                root.join("src/Support/Util.php")
                    .canonicalize()
                    .expect("canonicalize")
            )
        );
    }

    /// A `use` no PSR-4 prefix covers is a third-party dependency: it must not
    /// resolve to a file, so it falls through to `external_dep_key` unchanged.
    #[test]
    fn an_unmapped_namespace_does_not_resolve() {
        let dir = psr4_fixture(r#"{"autoload":{"psr-4":{"App\\":"src/"}}}"#);
        let importer = dir.path().join("src/Client.php");
        std::fs::write(&importer, "<?php\n").expect("importer");

        let adapter = Adapter::new();
        let ws = Workspace::default();
        assert_eq!(
            adapter.resolve_import("Guzzle\\Unrelated\\Thing", &importer, &ws),
            None
        );
        assert_eq!(
            adapter.external_dep_key("Guzzle\\Unrelated\\Thing"),
            Some("Guzzle".to_owned())
        );
        // the prefix matches but nothing is on disk at the mapped path
        assert_eq!(adapter.resolve_import("App\\Missing", &importer, &ws), None);
        // a prefix must not match a longer namespace segment: `AppKit` is not `App\`
        assert_eq!(
            adapter.resolve_import("AppKit\\Support\\Util", &importer, &ws),
            None
        );
    }

    /// `autoload-dev` is first-party too — a test class is in the repo.
    #[test]
    fn autoload_dev_prefixes_resolve() {
        let dir = psr4_fixture(
            r#"{"autoload":{"psr-4":{"App\\":"src/"}},
                "autoload-dev":{"psr-4":{"App\\Tests\\":"tests/"}}}"#,
        );
        let root = dir.path();
        let importer = root.join("tests/Other.php");
        std::fs::write(&importer, "<?php\n").expect("importer");

        let adapter = Adapter::new();
        assert_eq!(
            adapter.resolve_import("App\\Tests\\UtilTest", &importer, &Workspace::default()),
            Some(
                root.join("tests/UtilTest.php")
                    .canonicalize()
                    .expect("canonicalize")
            )
        );
    }

    /// Longest prefix wins, and a list of directories is a valid PSR-4 target.
    #[test]
    fn the_longest_matching_prefix_wins_over_a_shorter_one() {
        let dir = psr4_fixture(
            r#"{"autoload":{"psr-4":{"App\\":["lib/","src/"],"App\\Support\\":"src/Support/"}}}"#,
        );
        let root = dir.path();
        // `App\` would look for src/Support/Util.php too, so put a decoy where the
        // shorter prefix maps and assert the longer one's target is chosen.
        std::fs::create_dir_all(root.join("lib/Support")).expect("mkdir lib/Support");
        std::fs::write(root.join("lib/Support/Util.php"), "<?php\n").expect("decoy");
        std::fs::write(root.join("src/Support/Util.php"), "<?php\n").expect("real");
        let importer = root.join("src/Client.php");
        std::fs::write(&importer, "<?php\n").expect("importer");

        let adapter = Adapter::new();
        assert_eq!(
            adapter.resolve_import("App\\Support\\Util", &importer, &Workspace::default()),
            Some(
                root.join("src/Support/Util.php")
                    .canonicalize()
                    .expect("canonicalize")
            )
        );
    }

    /// composer.json is found by walking up, and a file outside any Composer
    /// project resolves nothing rather than walking to `/`.
    #[test]
    fn composer_json_is_found_by_walking_up_and_only_up() {
        let dir = psr4_fixture(r#"{"autoload":{"psr-4":{"App\\":"src/"}}}"#);
        let root = dir.path();
        let deep = root.join("src/A/B/C");
        std::fs::create_dir_all(&deep).expect("mkdir deep");
        let importer = deep.join("Deep.php");
        std::fs::write(&importer, "<?php\n").expect("importer");

        let adapter = Adapter::new();
        assert_eq!(
            adapter.resolve_import("App\\Support\\Util", &importer, &Workspace::default()),
            Some(
                root.join("src/Support/Util.php")
                    .canonicalize()
                    .expect("canonicalize")
            )
        );

        let bare = ScratchDir::new();
        let orphan = bare.path().join("Orphan.php");
        std::fs::write(&orphan, "<?php\n").expect("orphan");
        assert_eq!(
            adapter.resolve_import("App\\Support\\Util", &orphan, &Workspace::default()),
            None
        );
    }

    /// A composer.json that is absent, unparseable, or has no PSR-4 section
    /// yields no map — every `use` falls through to the external key.
    #[test]
    fn a_broken_composer_json_yields_no_map() {
        let dir = ScratchDir::new();
        let root = dir.path();
        assert!(psr4_map(&root.join("composer.json")).is_empty());
        std::fs::write(root.join("composer.json"), "{ not json at all ][").expect("write");
        assert!(psr4_map(&root.join("composer.json")).is_empty());
        std::fs::write(root.join("composer.json"), r#"{"name":"a/b"}"#).expect("write");
        assert!(psr4_map(&root.join("composer.json")).is_empty());
    }

    /// A `repositories` entry can inline a package with an `autoload` of its own
    /// (guzzle's composer.json does). That is not this project's map.
    #[test]
    fn only_the_top_level_autoload_counts() {
        let dir = ScratchDir::new();
        let composer = dir.path().join("composer.json");
        std::fs::write(
            &composer,
            r#"{"repositories":[{"package":{"autoload":{"psr-4":{"Vendor\\":"vendor-src/"}}}}],
                "autoload":{"psr-4":{"App\\":"src/"}}}"#,
        )
        .expect("write");
        assert_eq!(
            psr4_map(&composer),
            [("App\\".to_owned(), vec!["src/".to_owned()])]
        );
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
