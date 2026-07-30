//! Discover the resolution context for a project: tsconfig `paths`/`baseUrl`
//! aliases and workspace package locations. Best-effort — missing or malformed
//! config degrades to relative-only resolution, never an error.

use lang::Workspace;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const IGNORED_DIRS: &[&str] = &["node_modules", "dist", "build", "out", ".git", ".ripple"];

pub fn discover(root: &Path) -> Workspace {
    let mut ws = Workspace::default();
    read_tsconfig(root, &mut ws);
    read_packages(root, &mut ws);
    ws
}

/// One resolution context per indexed root, because they are not interchangeable:
/// two repos both defining `@/*` in their tsconfig mean different directories, and
/// sharing them would resolve an import to the wrong repo's file.
///
/// Package *names* are the exception, and the reason this type exists: a name
/// declared in exactly one root is visible from every root, which is what makes
/// `import "@org/api-client"` in one repo land in another. A name declared in two
/// roots stays local to each — two candidate directories is an ambiguity, and
/// `resolve_import` returns one path, so there is nothing to split 1/N over.
pub struct Workspaces {
    by_root: Vec<(PathBuf, Workspace)>,
    empty: Workspace,
}

impl Workspaces {
    pub fn discover_all(roots: &[(String, PathBuf)]) -> Workspaces {
        let mut by_root: Vec<(PathBuf, Workspace)> = roots
            .iter()
            .map(|(_, r)| (r.clone(), discover(r)))
            .collect();

        // how many roots declare each package name
        let mut declared: HashMap<String, usize> = HashMap::new();
        for (_, ws) in &by_root {
            for name in ws.packages.keys() {
                *declared.entry(name.clone()).or_default() += 1;
            }
        }
        let shared: Vec<(String, PathBuf)> = by_root
            .iter()
            .flat_map(|(_, ws)| ws.packages.iter())
            .filter(|(name, _)| declared.get(*name) == Some(&1))
            .map(|(n, d)| (n.clone(), d.clone()))
            .collect();
        for (_, ws) in &mut by_root {
            for (name, dir) in &shared {
                ws.packages.entry(name.clone()).or_insert(dir.clone());
            }
        }
        Workspaces {
            by_root,
            empty: Workspace::default(),
        }
    }

    /// The context of the root this file belongs to. Longest matching root wins, so
    /// a root nested inside another is answered by the more specific one.
    pub fn for_file(&self, file: &Path) -> &Workspace {
        self.by_root
            .iter()
            .filter(|(root, _)| file.starts_with(root))
            .max_by_key(|(root, _)| root.as_os_str().len())
            .map_or(&self.empty, |(_, ws)| ws)
    }
}

/// Parse `compilerOptions.baseUrl` + `paths` from the root tsconfig.
fn read_tsconfig(root: &Path, ws: &mut Workspace) {
    let Ok(text) = std::fs::read_to_string(root.join("tsconfig.json")) else {
        return;
    };
    let Some(json) = parse_jsonc(&text) else {
        return;
    };
    let opts = json.get("compilerOptions");

    let base_rel = opts
        .and_then(|o| o.get("baseUrl"))
        .and_then(|b| b.as_str())
        .unwrap_or("."); // tsconfig defaults baseUrl to the config's dir
    let base = root.join(base_rel);
    ws.base_url = base.canonicalize().ok().or(Some(base));

    if let Some(paths) = opts
        .and_then(|o| o.get("paths"))
        .and_then(|p| p.as_object())
    {
        for (pattern, targets) in paths {
            let targets: Vec<String> = targets
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            if !targets.is_empty() {
                ws.paths.push((pattern.clone(), targets));
            }
        }
    }
}

/// Map every workspace `package.json` "name" to its directory (skips node_modules).
fn read_packages(root: &Path, ws: &mut Workspace) {
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !is_ignored_dir(e))
        .filter_map(Result::ok)
    {
        if entry.file_name() != "package.json" {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Some(json) = parse_jsonc(&text) else {
            continue;
        };
        if let Some(name) = json.get("name").and_then(|n| n.as_str()) {
            if let Some(dir) = entry.path().parent() {
                if let Ok(dir) = dir.canonicalize() {
                    ws.packages.insert(name.to_owned(), dir);
                }
            }
        }
    }
}

/// Parse JSON, retrying with comments/trailing commas stripped (tsconfig is JSONC).
fn parse_jsonc(text: &str) -> Option<serde_json::Value> {
    if let Ok(v) = serde_json::from_str(text) {
        return Some(v);
    }
    serde_json::from_str(&strip_jsonc(text)).ok()
}

/// Crude JSONC cleanup: drop `//` and `/* */` comments and trailing commas.
/// Comment markers inside string literals are preserved.
fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => out.push(c),
        }
    }
    strip_trailing_commas(&out)
}

fn strip_trailing_commas(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ',' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && (chars[j] == '}' || chars[j] == ']') {
                // trailing comma: drop it, keep the whitespace
                out.extend(&chars[i + 1..j]);
                i = j;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn is_ignored_dir(e: &walkdir::DirEntry) -> bool {
    e.file_type().is_dir()
        && e.file_name()
            .to_str()
            .is_some_and(|n| IGNORED_DIRS.contains(&n))
}
