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
pub fn workspace_package(spec: &str, ws: &Workspace, globs: &[&str]) -> Option<PathBuf> {
    for (name, dir) in &ws.packages {
        if spec == name {
            return probe(&dir.join("index"), globs).or_else(|| probe(&dir.join("src/index"), globs));
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
    None
}

fn with_suffix(base: &Path, ext: &str) -> PathBuf {
    let mut s = base.as_os_str().to_owned();
    s.push(ext);
    PathBuf::from(s)
}
