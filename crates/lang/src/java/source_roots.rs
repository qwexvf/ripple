//! Build-tool source-root discovery, shared by the three JVM adapters.
//!
//! A dotted JVM FQN (`timber.log.Timber`) names a *package* path, not a filesystem
//! path. Where that package path hangs off the tree is a property of the build, not
//! of the import: Maven/Gradle/sbt put it under `src/<variant>/<lang>`, and Mill
//! flattens it to `<module>/src/` without encoding the package in the path at all.
//! Probing the FQN against the importing file's ancestors — all `resolve_import`
//! used to do — therefore misses every layout where the package prefix is not
//! literally a suffix of some ancestor directory, and misses *all* cross-module
//! imports. See issue #114: on timber and os-lib that was 0 of 126 and 0 of 219
//! `Imports` edges resolved to a local file.
//!
//! So: walk up from the importing file to the nearest build markers, enumerate the
//! source roots each one implies (its own and its sibling modules'), and probe the
//! FQN under those. [`resolve`] keeps the old ancestor probe as a final fallback so
//! nothing that resolved before stops resolving, and returns `None` when nothing
//! matches so a genuine third-party import still falls through to an external node.
//!
//! It lives under `java/` — rather than as a top-level `lang` module — because
//! `crates/lang/src/lib.rs` cannot be touched to declare one; `kotlin` and `scala`
//! reach it as `crate::java::source_roots`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// How far up from the importing file we look for build markers. Deep package
/// paths (`src/main/java/com/example/a/b/c/`) plus a module dir plus the repo root
/// need roughly a dozen.
const MAX_ANCESTORS: usize = 16;

/// Ceiling on discovered roots, so a repo with hundreds of modules cannot turn one
/// import into an unbounded stat storm.
const MAX_ROOTS: usize = 512;

/// How many trailing FQN segments may name members rather than the file. Two covers
/// the deepest form that shows up in practice — a static member of a nested type,
/// `timber.lint.WrongTimberUsageDetector.Companion.issues`.
const MAX_MEMBER_SEGMENTS: usize = 2;

/// The per-language directory a `src/<variant>/` holds. All three are probed for
/// every language: polyglot JVM modules routinely put Java and Kotlin side by side
/// and either may define the imported type.
const LANG_DIRS: [&str; 3] = ["java", "kotlin", "scala"];

/// Build markers that make a directory a project (or module) root.
const MARKERS: [&str; 8] = [
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "build.sbt",
    "build.sc",
    "build.mill",
];

/// Whether a hit under a source root is proof on its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Layout {
    /// The path encodes the package (`src/main/java/com/x/Y.java`), so a path hit
    /// *is* the evidence.
    Packaged,
    /// The path does not encode the package (Mill's `os/src/Path.scala` holds
    /// `package os`), so the FQN's leading segments have to be stripped to find the
    /// file — and a hit must be confirmed against the file's own `package`
    /// declaration, or `scala.util.Try` would happily bind to any stray `Try.scala`.
    Flat,
}

type Roots = Vec<(PathBuf, Layout)>;

/// Resolve a dotted FQN to a local source file, build-file discovery first and the
/// legacy ancestor probe second. `exts` are the language's file extensions, most
/// specific first (`["kt", "kts"]`).
pub fn resolve(spec: &str, from: &Path, exts: &[&str]) -> Option<PathBuf> {
    resolve_under_source_roots(spec, from, exts).or_else(|| ancestor_probe(spec, from, exts))
}

/// Probe the FQN under every source root the surrounding build declares.
fn resolve_under_source_roots(spec: &str, from: &Path, exts: &[&str]) -> Option<PathBuf> {
    let segments: Vec<&str> = spec.split('.').collect();
    if segments.len() < 2 || segments.iter().any(|s| s.is_empty()) {
        return None;
    }
    // `import a.b.Foo` names the type, but `import a.b.Foo.Companion.issues` names a
    // member of a member of it — the file is `a/b/Foo`. So drop trailing segments
    // too, longest prefix first, capped at MAX_MEMBER_SEGMENTS so a package path
    // can't be mistaken for a type.
    let shortest = segments.len().saturating_sub(MAX_MEMBER_SEGMENTS).max(2);
    for (root, layout) in roots_for(from) {
        for len in (shortest..=segments.len()).rev() {
            if let Some(hit) = probe(&root, layout, &segments[..len], exts) {
                return Some(hit);
            }
        }
    }
    None
}

/// Map the FQN to a relative path and try it against a bounded run of the importing
/// file's ancestors, implicitly discovering a source root. Kept as a fallback: it is
/// what resolved same-tree imports before build discovery existed.
pub fn ancestor_probe(spec: &str, from: &Path, exts: &[&str]) -> Option<PathBuf> {
    let segments: Vec<&str> = spec.split('.').collect();
    if segments.iter().any(|s| s.is_empty()) {
        return None;
    }
    let mut dir = from.parent();
    for _ in 0..8 {
        let base = dir?;
        if let Some(hit) = file_at(base, &segments, exts) {
            return Some(hit);
        }
        dir = base.parent();
    }
    None
}

fn probe(root: &Path, layout: Layout, segs: &[&str], exts: &[&str]) -> Option<PathBuf> {
    match layout {
        Layout::Packaged => file_at(root, segs, exts),
        Layout::Flat => {
            let package = segs.get(..segs.len().checked_sub(1)?)?.join(".");
            if package.is_empty() {
                return None;
            }
            (0..segs.len()).find_map(|drop| {
                let hit = file_at(root, &segs[drop..], exts)?;
                let declared = declared_package(&hit)?;
                package_matches(&declared, &package).then_some(hit)
            })
        }
    }
}

/// `root/a/b/C.<ext>` for the first extension that exists.
fn file_at(root: &Path, segs: &[&str], exts: &[&str]) -> Option<PathBuf> {
    if segs.is_empty() {
        return None;
    }
    let mut base = root.to_path_buf();
    for s in segs {
        base.push(s);
    }
    for ext in exts {
        let mut candidate = base.clone().into_os_string();
        candidate.push(".");
        candidate.push(ext);
        let candidate = PathBuf::from(candidate);
        if candidate.is_file() {
            return candidate.canonicalize().ok();
        }
    }
    None
}

/// The package declared at the head of a JVM source file. Scala allows the nested
/// form (`package a` then `package b` meaning `a.b`), so consecutive declarations
/// are joined.
fn declared_package(file: &Path) -> Option<String> {
    let text = std::fs::read_to_string(file).ok()?;
    let mut parts: Vec<&str> = Vec::new();
    for line in text.lines().take(80) {
        let line = line.trim();
        if line.is_empty()
            || line.starts_with("//")
            || line.starts_with('*')
            || line.starts_with("/*")
        {
            continue;
        }
        let Some(rest) = line.strip_prefix("package ") else {
            if parts.is_empty() {
                continue;
            }
            break;
        };
        // `package object foo` is a definition, not a package declaration
        let name = rest.trim().trim_end_matches(['{', ';']).trim();
        if name.is_empty() || name.starts_with("object ") {
            break;
        }
        parts.push(name);
    }
    (!parts.is_empty()).then(|| parts.join("."))
}

/// Is the package a file declares consistent with the package the FQN implies? One
/// may be a dot-boundary prefix of the other: a nested/partial `package` declaration
/// is common, and the FQN's tail may name an inner type rather than the file's own.
fn package_matches(declared: &str, fqn_package: &str) -> bool {
    declared == fqn_package
        || fqn_package
            .strip_prefix(declared)
            .is_some_and(|rest| rest.starts_with('.'))
        || declared
            .strip_prefix(fqn_package)
            .is_some_and(|rest| rest.starts_with('.'))
}

fn cache() -> &'static Mutex<HashMap<PathBuf, Roots>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Roots>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Source roots visible from the importing file, memoized per importing directory —
/// every import in a file shares the answer, and discovery is all filesystem stats.
fn roots_for(from: &Path) -> Roots {
    let Some(dir) = from.parent() else {
        return Roots::new();
    };
    if let Ok(cached) = cache().lock() {
        if let Some(hit) = cached.get(dir) {
            return hit.clone();
        }
    }
    let roots = discover(dir);
    if let Ok(mut cached) = cache().lock() {
        // the index walks one tree at a time; a hard reset is enough of a bound
        if cached.len() > 4096 {
            cached.clear();
        }
        cached.insert(dir.to_path_buf(), roots.clone());
    }
    roots
}

/// Walk up from `dir` collecting the source roots of every project marker on the
/// way, nearest first, stopping at the repo root.
fn discover(dir: &Path) -> Roots {
    let mut roots = Roots::new();
    let mut seen = HashSet::new();
    let mut cur = Some(dir);
    for _ in 0..MAX_ANCESTORS {
        let Some(project) = cur else { break };
        if MARKERS.iter().any(|m| project.join(m).is_file()) {
            push_project_roots(project, &mut roots, &mut seen);
        }
        if project.join(".git").exists() {
            break;
        }
        cur = project.parent();
    }
    roots.truncate(MAX_ROOTS);
    roots
}

fn push_project_roots(project: &Path, out: &mut Roots, seen: &mut HashSet<PathBuf>) {
    let mut dirs = vec![project.to_path_buf()];
    dirs.extend(module_dirs(project));
    for dir in &dirs {
        for root in packaged_roots(dir) {
            push(out, seen, root, Layout::Packaged);
        }
    }
    if project.join("build.sc").is_file() || project.join("build.mill").is_file() {
        for root in mill_roots(project) {
            push(out, seen, root, Layout::Flat);
        }
    }
}

fn push(out: &mut Roots, seen: &mut HashSet<PathBuf>, root: PathBuf, layout: Layout) {
    if out.len() < MAX_ROOTS && seen.insert(root.clone()) {
        out.push((root, layout));
    }
}

/// Roots under `dir` whose path encodes the package: the Maven/Gradle/sbt
/// `src/<variant>/<lang>` shape. Variants are *enumerated* rather than listed, so
/// `src/main`, `src/test` and every Kotlin-Multiplatform target (`commonMain`,
/// `androidMain`, `jvmMain`, …) come for free. Plus any explicit `<sourceDirectory>`
/// a `pom.xml` declares.
fn packaged_roots(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir.join("src")) {
        let mut variants: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        variants.sort();
        for variant in variants {
            for lang in LANG_DIRS {
                let root = variant.join(lang);
                if root.is_dir() {
                    out.push(root);
                }
            }
        }
    }
    out.extend(maven_explicit_roots(dir));
    out
}

/// Mill does not put the package in the path: a module directory holds `src/`,
/// per-platform `src-jvm/`/`src-2/` variants, and `test/src/`. os-lib's `os` package
/// lives at `os/src/`. Module dirs are scanned two levels deep (`os/`, `os/watch/`).
fn mill_roots(project: &Path) -> Vec<PathBuf> {
    let mut modules = vec![project.to_path_buf()];
    let direct = child_dirs(project);
    modules.extend(direct.iter().flat_map(|d| child_dirs(d)));
    modules.extend(direct);
    let mut out = Vec::new();
    for module in modules {
        for base in [module.clone(), module.join("test")] {
            for dir in child_dirs(&base) {
                let name = dir.file_name().unwrap_or_default().to_string_lossy();
                if name == "src" || name.starts_with("src-") {
                    out.push(dir);
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Sibling modules of a multi-module build: the ones the build file declares
/// (Maven `<modules>`, Gradle `include`) plus anything on disk that looks like a
/// module, since sbt and Mill declare theirs in code we do not evaluate.
fn module_dirs(project: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = declared_modules(project)
        .iter()
        .map(|rel| project.join(rel))
        .filter(|d| d.is_dir())
        .collect();
    for dir in child_dirs(project) {
        if dir.join("src").is_dir() || MARKERS.iter().any(|m| dir.join(m).is_file()) {
            out.push(dir);
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Module paths declared in `pom.xml` (`<modules>`) and `settings.gradle[.kts]`
/// (`include`), as project-relative paths.
fn declared_modules(project: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(pom) = std::fs::read_to_string(project.join("pom.xml")) {
        out.extend(maven_modules(&pom));
    }
    for name in ["settings.gradle", "settings.gradle.kts"] {
        if let Ok(text) = std::fs::read_to_string(project.join(name)) {
            out.extend(gradle_includes(&text));
        }
    }
    out
}

/// `<module>gson</module>` entries. The `lang` crate has no XML parser available
/// (and Cargo.toml is not ours to change), so this scans for the one tag we need
/// instead of parsing. Consequence: a `<module>` inside a comment or an unrelated
/// element is picked up too — harmless, because roots are additive and a path that
/// is not a directory is dropped.
fn maven_modules(pom: &str) -> Vec<String> {
    tag_values(pom, "module")
}

/// `include ':timber'`, `include(":a:b")` → `timber`, `a/b`.
fn gradle_includes(settings: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in settings.lines() {
        let line = line.trim();
        if line.starts_with("//") || !line.starts_with("include") {
            continue;
        }
        for chunk in line.split(['\'', '"']).skip(1).step_by(2) {
            let path = chunk.trim().trim_start_matches(':').replace(':', "/");
            if !path.is_empty() {
                out.push(path);
            }
        }
    }
    out
}

/// `<sourceDirectory>`/`<testSourceDirectory>` from a `pom.xml`, resolved against
/// the module dir. Same string-scanning caveat as [`maven_modules`]: a tag nested in
/// a plugin's configuration is read too, and is filtered out later by not existing.
fn maven_explicit_roots(dir: &Path) -> Vec<PathBuf> {
    let Ok(pom) = std::fs::read_to_string(dir.join("pom.xml")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for tag in ["sourceDirectory", "testSourceDirectory"] {
        for value in tag_values(&pom, tag) {
            let value = value
                .replace("${project.basedir}", ".")
                .replace("${basedir}", ".");
            let root = dir.join(value.trim_start_matches("./"));
            if root.is_dir() {
                out.push(root);
            }
        }
    }
    out
}

fn tag_values(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        rest = &rest[start + open.len()..];
        let Some(end) = rest.find(&close) else { break };
        let value = rest[..end].trim();
        if !value.is_empty() && !value.contains('<') {
            out.push(value.to_owned());
        }
        rest = &rest[end + close.len()..];
    }
    out
}

fn child_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maven_modules_reads_the_module_list() {
        let pom = "<project>\n  <modules>\n    <module>gson</module>\n    <module>extras</module>\n  </modules>\n</project>";
        assert_eq!(maven_modules(pom), vec!["gson", "extras"]);
    }

    #[test]
    fn gradle_includes_flattens_colon_paths() {
        let settings = "include ':timber'\ninclude ':timber-lint'\n// include ':nope'\ninclude(\":a:b\")\nrootProject.name = 'x'\n";
        assert_eq!(
            gradle_includes(settings),
            vec!["timber", "timber-lint", "a/b"]
        );
    }

    #[test]
    fn package_matches_on_dot_boundaries_only() {
        assert!(package_matches("test.os", "test.os"));
        assert!(package_matches("os", "os.inner"));
        assert!(package_matches("a.b.c", "a.b"));
        assert!(!package_matches("oswald", "os"));
        assert!(!package_matches("os", "oswald"));
    }
}
