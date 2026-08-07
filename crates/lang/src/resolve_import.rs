//! Module-specifier resolution: relative paths, tsconfig `paths` aliases, and
//! workspace packages. External `node_modules` deps are intentionally not
//! resolved (they're out of the indexed graph).

use crate::Workspace;
use std::path::{Path, PathBuf};

/// Resolve via tsconfig `paths` (e.g. `@app/x` with `"@app/*": ["src/*"]`).
pub fn tsconfig_paths(spec: &str, ws: &Workspace, globs: &[&str]) -> Option<PathBuf> {
    let base = ws.base_url.as_deref()?;
    for (pattern, targets) in &ws.paths {
        if let Some(rest) = match_pattern(pattern, spec) {
            for target in targets {
                let sub = target.replace('*', rest);
                if let Some(p) = probe(&base.join(sub), globs) {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// Resolve a bare specifier to a workspace package (`@org/pkg` or `@org/pkg/sub`).
///
/// Longest name wins, which is both npm's rule and the only deterministic one:
/// with `@org/a` and `@org/a/sub` both registered, iterating a `HashMap` resolved
/// `@org/a/sub/x` differently from run to run.
pub fn workspace_package(spec: &str, ws: &Workspace, globs: &[&str]) -> Option<PathBuf> {
    let mut names: Vec<&String> = ws.packages.keys().collect();
    names.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
    for name in names {
        let dir = &ws.packages[name];
        if spec == name {
            return probe(&dir.join("index"), globs)
                .or_else(|| probe(&dir.join("src/index"), globs));
        }
        if let Some(sub) = spec.strip_prefix(&format!("{name}/")) {
            if let Some(p) = probe(&dir.join(sub), globs) {
                return Some(p);
            }
        }
    }
    None
}

/// Match a tsconfig path pattern against a specifier, returning the `*` capture.
/// Supports a single trailing-ish `*` wildcard and exact patterns.
fn match_pattern<'a>(pattern: &str, spec: &'a str) -> Option<&'a str> {
    match pattern.split_once('*') {
        Some((prefix, suffix)) => spec
            .strip_prefix(prefix)
            .and_then(|r| r.strip_suffix(suffix)),
        None if pattern == spec => Some(""),
        None => None,
    }
}

/// Resolve a relative `spec` (`./x`, `../y`) against `from_file`, probing the
/// adapter's extensions and `index.*`. Returns a canonicalized path or `None`
/// for bare specifiers (handled later).
pub fn relative(spec: &str, from_file: &Path, globs: &[&str]) -> Option<PathBuf> {
    if !(spec.starts_with("./") || spec.starts_with("../")) {
        return None; // bare specifier → tsconfig paths / workspace package
    }
    probe(&from_file.parent()?.join(spec), globs)
}

/// Try to resolve `base` to an existing file: as-is, then `base.<ext>`, then
/// `base/index.<ext>`, for each of the adapter's extensions. Returns canonical.
pub fn probe(base: &Path, globs: &[&str]) -> Option<PathBuf> {
    let exts = globs.iter().filter_map(|g| g.strip_prefix('*'));

    if base.is_file() {
        return base.canonicalize().ok();
    }
    for ext in exts.clone() {
        let p = with_suffix(base, ext);
        if p.is_file() {
            return p.canonicalize().ok();
        }
    }
    for ext in exts {
        let p = base.join(format!("index{ext}"));
        if p.is_file() {
            return p.canonicalize().ok();
        }
    }
    // ESM/NodeNext spells the import by its *emitted* name (`./cause.js` for
    // `cause.ts`). Drop that suffix and re-probe the source extensions.
    if let Some(stem) = strip_emitted_js(base) {
        return probe(&stem, globs);
    }
    None
}

/// `./cause.js` → `./cause`, so a `.ts`/`.tsx` source can be probed for it.
/// Only the JS-family emitted extensions, and only when a source spelling
/// doesn't already exist as a real file (checked by the caller via `base`).
fn strip_emitted_js(base: &Path) -> Option<PathBuf> {
    const EMITTED: &[&str] = &["js", "jsx", "mjs", "cjs"];
    let ext = base.extension()?.to_str()?;
    if !EMITTED.contains(&ext) {
        return None;
    }
    Some(base.with_extension(""))
}

fn with_suffix(base: &Path, ext: &str) -> PathBuf {
    let mut s = base.as_os_str().to_owned();
    s.push(ext);
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    const TS: &[&str] = &["*.ts", "*.tsx"];

    /// A throwaway directory tree; files are created by relative path.
    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(files: &[&str]) -> Fixture {
            static N: AtomicU32 = AtomicU32::new(0);
            let id = N.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!("ripple-ri-{}-{id}", std::process::id()));
            for f in files {
                let p = root.join(f);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(&p, "").unwrap();
            }
            Fixture { root }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn canon(base: &Path) -> Option<PathBuf> {
        base.canonicalize().ok()
    }

    /// `import "./cause.js"` must land on `cause.ts` — the NodeNext convention of
    /// spelling an import by its emitted `.js` name.
    #[test]
    fn relative_js_specifier_resolves_to_ts_source() {
        let fx = Fixture::new(&["src/client.ts", "src/cause.ts"]);
        let from = fx.root.join("src/client.ts");
        let got = relative("./cause.js", &from, TS);
        assert_eq!(got, canon(&fx.root.join("src/cause.ts")));
    }

    #[test]
    fn relative_js_specifier_resolves_to_tsx_source() {
        let fx = Fixture::new(&["src/a.tsx", "src/view.tsx"]);
        let from = fx.root.join("src/a.tsx");
        let got = relative("./view.js", &from, TS);
        assert_eq!(got, canon(&fx.root.join("src/view.tsx")));
    }

    /// A real `.js` file still wins over the source-extension fallback.
    #[test]
    fn a_real_js_file_is_preferred() {
        let fx = Fixture::new(&["src/a.ts", "src/legacy.js"]);
        let from = fx.root.join("src/a.ts");
        let got = relative("./legacy.js", &from, TS);
        assert_eq!(got, canon(&fx.root.join("src/legacy.js")));
    }

    #[test]
    fn extensionless_relative_still_resolves() {
        let fx = Fixture::new(&["src/a.ts", "src/b.ts"]);
        let from = fx.root.join("src/a.ts");
        let got = relative("./b", &from, TS);
        assert_eq!(got, canon(&fx.root.join("src/b.ts")));
    }

    /// `.mjs`/`.cjs` map to source the same way `.js` does.
    #[test]
    fn mjs_and_cjs_specifiers_resolve_to_ts_source() {
        let fx = Fixture::new(&["src/a.ts", "src/m.ts", "src/c.ts"]);
        let from = fx.root.join("src/a.ts");
        assert_eq!(
            relative("./m.mjs", &from, TS),
            canon(&fx.root.join("src/m.ts"))
        );
        assert_eq!(
            relative("./c.cjs", &from, TS),
            canon(&fx.root.join("src/c.ts"))
        );
    }

    /// The barrel case behind bug #3: `@pkg` resolves to `src/index.ts`, whose
    /// re-export `from "./stack.js"` must then reach `stack.ts`.
    #[test]
    fn workspace_index_reexport_via_js_extension() {
        let fx = Fixture::new(&["pkg/src/index.ts", "pkg/src/stack.ts"]);
        let mut ws = Workspace::default();
        ws.packages.insert(
            "@nande/protocol".to_owned(),
            fx.root.join("pkg").canonicalize().unwrap(),
        );
        let entry = workspace_package("@nande/protocol", &ws, TS);
        assert_eq!(entry, canon(&fx.root.join("pkg/src/index.ts")));

        // the re-export inside the barrel, resolved relative to it
        let reexport = relative("./stack.js", entry.as_ref().unwrap(), TS);
        assert_eq!(reexport, canon(&fx.root.join("pkg/src/stack.ts")));
    }
}
