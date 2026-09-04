//! Build-declared include-directory discovery, shared by the C and C++ adapters.
//!
//! A quoted `#include "foo.h"` names a file, but *where* that file lives is a
//! property of the build: the preprocessor only looks next to the including file
//! and then along the `-I` search path the compiler was invoked with. Probing the
//! including file's ancestors — all `resolve_import` used to do — therefore misses
//! every header reached through a build-declared `-I`, and those bind to an
//! `External` node instead of the local file, so the import graph is incomplete
//! (issue #121).
//!
//! Two sources, in priority order:
//!
//! 1. **`compile_commands.json`** — the precise answer. A flat JSON array of
//!    `{directory, file, command|arguments}` entries emitted by CMake, Bazel, Meson
//!    and Ninja alike, so it needs no build-system evaluation and gives the search
//!    path *per translation unit*, which beats any project-wide guess. Looked for
//!    at each ancestor of the importing file and in the usual build directories.
//! 2. **`CMakeLists.txt`** — `include_directories()` and
//!    `target_include_directories()`, used only when no compile DB was found. This
//!    is a string scan, not a CMake evaluation: a path behind an unexpanded
//!    variable, a `if()` branch that is not taken, or a directory added by a macro
//!    is invisible to it. It is a fallback, and additive — a wrong guess costs a
//!    failed `stat`, not a wrong edge, because a candidate must exist on disk.
//!
//! [`resolve`] documents the exact priority. The short version: an entry that names
//! *this* file is authoritative, everything else sits below the old relative +
//! `<ancestor>/include/` probing, so nothing that resolved before stops resolving
//! and no existing edge changes target. `None` when nothing matches, so a genuine
//! system header still falls through to `external_dep_key`.
//!
//! It lives under `c/` — rather than as a top-level `lang` module — because
//! `crates/lang/src/lib.rs` cannot be touched to declare one; the C++ adapter
//! reaches it as `crate::c::include_dirs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// How far up from the importing file we look for a compile DB or a CMakeLists.
/// A deep source tree plus a build dir needs roughly this many.
const MAX_ANCESTORS: usize = 16;

/// Ceiling on discovered include dirs, so a compile DB with thousands of entries
/// cannot turn one `#include` into an unbounded stat storm.
const MAX_DIRS: usize = 2048;

/// How many ancestors the legacy relative probe walks. Unchanged from before
/// build discovery existed.
const LEGACY_ANCESTORS: usize = 8;

/// Where a compile DB hides relative to a project (or build) root. Covers CMake's
/// own `-B build`, Ninja/Bazel `out/`, Meson's `builddir/`, plus the
/// `cmake-build-*` directories the JetBrains IDEs generate (matched by prefix).
const BUILD_DIRS: [&str; 5] = ["build", "out", "builddir", "_build", "bazel-bin"];
const BUILD_DIR_PREFIX: &str = "cmake-build-";

/// Include-search dirs a build declares, split into the per-translation-unit map
/// the compile DB gives us and the union used for everything else (a header is
/// not itself a translation unit, so it has no entry of its own).
#[derive(Default, Debug)]
pub struct IncludeDirs {
    per_file: HashMap<PathBuf, Vec<PathBuf>>,
    global: Vec<PathBuf>,
}

impl IncludeDirs {
    fn is_empty(&self) -> bool {
        self.global.is_empty() && self.per_file.is_empty()
    }

    /// The compile DB's entry for this exact file: the search path the compiler
    /// itself was given for this translation unit, so it is authoritative.
    fn own(&self, from: &Path) -> Vec<PathBuf> {
        from.canonicalize()
            .ok()
            .and_then(|c| self.per_file.get(&c).cloned())
            .unwrap_or_default()
    }
}

/// Resolve a quoted `#include` spec to a local file, in descending order of
/// confidence:
///
/// 1. the including file's own directory — what the preprocessor looks at first;
/// 2. the `-I` flags the compile DB records **for this exact file**, which is the
///    compiler's own search path for this translation unit;
/// 3. the legacy ancestor + `<ancestor>/include/` probe;
/// 4. the union of every include dir the build declares.
///
/// The union sits *below* the legacy probe deliberately. It flattens away which
/// target each `-I` belonged to, so for a header — which is not a translation unit
/// and therefore has no entry of its own — it can outrank a nearer, correct
/// neighbour: on libgit2, `src/cli/win32/precompiled.h`'s `#include "common.h"`
/// resolved to `src/libgit2/common.h` instead of `src/cli/common.h` when the union
/// went first. Below the probe the union is purely additive: it only ever resolves
/// includes that used to bind to an external node.
///
/// Returns `None` for a system `<...>` include and when nothing matches, so the
/// caller can mint an external dependency instead.
pub fn resolve(spec: &str, from: &Path) -> Option<PathBuf> {
    if spec.starts_with('<') || spec.is_empty() {
        return None;
    }
    let dir = from.parent()?;
    if let Some(hit) = file_at(dir, spec) {
        return Some(hit);
    }
    let dirs = dirs_for(from);
    for base in dirs.own(from) {
        if let Some(hit) = file_at(&base, spec) {
            return Some(hit);
        }
    }
    if let Some(hit) = ancestor_probe(spec, from) {
        return Some(hit);
    }
    dirs.global.iter().find_map(|base| file_at(base, spec))
}

/// The probe that existed before build discovery: walk up a bounded run of the
/// importing file's ancestors trying `<ancestor>/spec` and the very common
/// `<ancestor>/include/spec` layout. Kept so nothing that resolved before stops.
fn ancestor_probe(spec: &str, from: &Path) -> Option<PathBuf> {
    let mut dir = from.parent();
    for _ in 0..LEGACY_ANCESTORS {
        let base = dir?;
        for cand in [base.to_path_buf(), base.join("include")] {
            if let Some(hit) = file_at(&cand, spec) {
                return Some(hit);
            }
        }
        dir = base.parent();
    }
    None
}

fn file_at(base: &Path, spec: &str) -> Option<PathBuf> {
    let cand = base.join(spec);
    if !cand.is_file() {
        return None;
    }
    cand.canonicalize().ok()
}

fn dir_cache() -> &'static Mutex<HashMap<PathBuf, Arc<IncludeDirs>>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Arc<IncludeDirs>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn db_cache() -> &'static Mutex<HashMap<PathBuf, Arc<IncludeDirs>>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Arc<IncludeDirs>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Include dirs visible from the importing file, memoized per importing directory —
/// every `#include` in a file shares the answer, and rediscovering means re-reading
/// (and re-parsing) the compile DB. The DB parse is memoized a second time by its
/// own path, so files in different directories under one build share the parse.
fn dirs_for(from: &Path) -> Arc<IncludeDirs> {
    let Some(dir) = from.parent() else {
        return Arc::new(IncludeDirs::default());
    };
    if let Ok(cache) = dir_cache().lock() {
        if let Some(hit) = cache.get(dir) {
            return Arc::clone(hit);
        }
    }
    let dirs = discover(dir);
    if let Ok(mut cache) = dir_cache().lock() {
        // the index walks one tree at a time; a hard reset is enough of a bound
        if cache.len() > 4096 {
            cache.clear();
        }
        cache.insert(dir.to_path_buf(), Arc::clone(&dirs));
    }
    dirs
}

/// Walk up from `dir` looking for a compile DB (nearest first). Falling back to a
/// CMakeLists scan only when no DB was found anywhere on the way up.
fn discover(dir: &Path) -> Arc<IncludeDirs> {
    let mut cmake_lists: Vec<PathBuf> = Vec::new();
    let mut cur = Some(dir);
    for _ in 0..MAX_ANCESTORS {
        let Some(base) = cur else { break };
        for db in compile_db_candidates(base) {
            let parsed = parse_db_cached(&db);
            if !parsed.is_empty() {
                return parsed;
            }
        }
        let lists = base.join("CMakeLists.txt");
        if lists.is_file() {
            cmake_lists.push(lists);
        }
        if base.join(".git").exists() {
            break;
        }
        cur = base.parent();
    }
    let mut global = Vec::new();
    for lists in &cmake_lists {
        for root in cmake_include_dirs(lists) {
            push(&mut global, root);
        }
    }
    Arc::new(IncludeDirs {
        per_file: HashMap::new(),
        global,
    })
}

fn parse_db_cached(db: &Path) -> Arc<IncludeDirs> {
    if let Ok(cache) = db_cache().lock() {
        if let Some(hit) = cache.get(db) {
            return Arc::clone(hit);
        }
    }
    let parsed = Arc::new(parse_compile_db(db));
    if let Ok(mut cache) = db_cache().lock() {
        if cache.len() > 64 {
            cache.clear();
        }
        cache.insert(db.to_path_buf(), Arc::clone(&parsed));
    }
    parsed
}

/// Places a compile DB may sit relative to one ancestor directory: beside it, and
/// inside each of the usual build directories.
fn compile_db_candidates(base: &Path) -> Vec<PathBuf> {
    const NAME: &str = "compile_commands.json";
    let mut out = vec![base.join(NAME)];
    for d in BUILD_DIRS {
        out.push(base.join(d).join(NAME));
    }
    if let Ok(entries) = std::fs::read_dir(base) {
        let mut extra: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(BUILD_DIR_PREFIX))
            })
            .map(|p| p.join(NAME))
            .collect();
        extra.sort();
        out.extend(extra);
    }
    out.retain(|p| p.is_file());
    out
}

/// Parse a `compile_commands.json` into per-file and union include dirs.
///
/// Parsed with `serde_yaml`, already a dependency of this crate: YAML 1.2 is a
/// JSON superset, which is the same trick the PHP adapter uses for
/// `composer.json`. A file that fails to parse yields nothing, so every include
/// falls through to the legacy probe unchanged.
///
/// Each entry's `-I`/`-isystem`/`-iquote` arguments are resolved against that
/// entry's own `directory`, which is what makes the answer per translation unit.
fn parse_compile_db(path: &Path) -> IncludeDirs {
    let Ok(text) = std::fs::read_to_string(path) else {
        return IncludeDirs::default();
    };
    let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(&text) else {
        return IncludeDirs::default();
    };
    let Some(entries) = doc.as_sequence() else {
        return IncludeDirs::default();
    };
    let mut per_file: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    let mut global: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let directory = entry.get("directory").and_then(serde_yaml::Value::as_str);
        let base = directory.map_or_else(
            || path.parent().unwrap_or(Path::new(".")).to_path_buf(),
            PathBuf::from,
        );
        let tokens = entry_tokens(entry);
        let dirs: Vec<PathBuf> = include_flags(&tokens)
            .into_iter()
            .map(|raw| absolutize(&base, &raw))
            .filter(|d| d.is_dir())
            .collect();
        if dirs.is_empty() {
            continue;
        }
        if let Some(file) = entry.get("file").and_then(serde_yaml::Value::as_str) {
            if let Ok(tu) = absolutize(&base, file).canonicalize() {
                let slot = per_file.entry(tu).or_default();
                for d in &dirs {
                    push(slot, d.clone());
                }
            }
        }
        for d in dirs {
            push(&mut global, d);
        }
        if global.len() >= MAX_DIRS {
            break;
        }
    }
    IncludeDirs { per_file, global }
}

/// The command line of one compile DB entry, as tokens. Either form is legal:
/// `arguments` is already a list, `command` is one shell-quoted string.
fn entry_tokens(entry: &serde_yaml::Value) -> Vec<String> {
    if let Some(args) = entry
        .get("arguments")
        .and_then(serde_yaml::Value::as_sequence)
    {
        return args
            .iter()
            .filter_map(|a| a.as_str().map(str::to_owned))
            .collect();
    }
    entry
        .get("command")
        .and_then(serde_yaml::Value::as_str)
        .map(split_command)
        .unwrap_or_default()
}

/// Split a shell-ish command line: single and double quotes group, a backslash
/// escapes the next character outside single quotes. Enough for the quoting a
/// build system emits; not a shell.
fn split_command(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), _) if c == q => quote = None,
            (Some('\''), _) => cur.push(c),
            (Some(_), '\\') => cur.extend(chars.next()),
            (Some(_), _) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                started = true;
            }
            (None, '\\') => cur.extend(chars.next()),
            (None, c) if c.is_whitespace() => {
                if started || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            (None, _) => cur.push(c),
        }
    }
    if started || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The include-search directories a command line declares, in command-line order.
/// Both the joined (`-Idir`) and separated (`-I dir`) spellings are accepted, as
/// is the `=`-suffixed long form. `-isystem`/`-iquote` count too: a project's own
/// headers are routinely pulled in through them.
fn include_flags(tokens: &[String]) -> Vec<String> {
    const FLAGS: [&str; 3] = ["-I", "-isystem", "-iquote"];
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i].as_str();
        i += 1;
        let Some(flag) = FLAGS.iter().find(|f| token.starts_with(**f)) else {
            continue;
        };
        let rest = token[flag.len()..].trim_start_matches('=');
        if rest.is_empty() {
            if let Some(next) = tokens.get(i) {
                i += 1;
                if !next.is_empty() {
                    out.push(next.clone());
                }
            }
        } else if token.len() == flag.len() + rest.len() || token[flag.len()..].starts_with('=') {
            out.push(rest.to_owned());
        }
    }
    out
}

/// Include dirs declared by one `CMakeLists.txt`, resolved against its directory.
///
/// A string scan, not an evaluation: CMake variables other than the handful of
/// well-known source-dir ones are not expanded, and a path that still contains a
/// `${...}` is dropped rather than guessed at.
fn cmake_include_dirs(lists: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(lists) else {
        return Vec::new();
    };
    let Some(dir) = lists.parent() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for command in ["include_directories", "target_include_directories"] {
        for args in cmake_calls(&text, command) {
            let target_call = command.starts_with("target_");
            for raw in cmake_paths(&args, target_call) {
                let expanded = expand_cmake_vars(&raw, dir);
                if expanded.contains("${") {
                    continue;
                }
                let path = absolutize(dir, &expanded);
                if path.is_dir() {
                    push(&mut out, path);
                }
            }
        }
    }
    out
}

/// Argument text of every `name(...)` call in a CMakeLists, comment lines removed.
fn cmake_calls(text: &str, name: &str) -> Vec<String> {
    let stripped: String = text
        .lines()
        .map(|l| l.split('#').next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = Vec::new();
    let lower = stripped.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find(name) {
        let start = from + at;
        from = start + name.len();
        // must be a call, not a suffix of a longer identifier
        let prev_ok = start == 0
            || !stripped[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
        let rest = stripped[from..].trim_start();
        if !prev_ok || !rest.starts_with('(') {
            continue;
        }
        let open = stripped[from..].find('(').map(|i| from + i + 1);
        let (Some(open), Some(close)) = (open, stripped[from..].find(')').map(|i| from + i)) else {
            continue;
        };
        if close > open {
            out.push(stripped[open..close].to_owned());
            from = close;
        }
    }
    out
}

/// Path arguments of one include-dirs call. Keyword arguments are dropped, and for
/// `target_include_directories` so is the leading target name.
fn cmake_paths(args: &str, target_call: bool) -> Vec<String> {
    const KEYWORDS: [&str; 6] = [
        "PUBLIC",
        "PRIVATE",
        "INTERFACE",
        "SYSTEM",
        "BEFORE",
        "AFTER",
    ];
    let mut tokens: Vec<String> = split_command(args);
    if target_call {
        let first_keyword = tokens.iter().position(|t| KEYWORDS.contains(&t.as_str()));
        tokens.drain(..first_keyword.unwrap_or(1).min(tokens.len()));
    }
    tokens
        .into_iter()
        .filter(|t| !KEYWORDS.contains(&t.as_str()) && !t.is_empty())
        .map(|t| {
            // $<BUILD_INTERFACE:path> / $<INSTALL_INTERFACE:path>
            match t.strip_prefix("$<").and_then(|r| r.strip_suffix('>')) {
                Some(inner) => inner.split_once(':').map_or(inner, |(_, p)| p).to_owned(),
                None => t,
            }
        })
        .filter(|t| !t.is_empty())
        .collect()
}

/// Expand the CMake variables that name a source or build directory. Anything else
/// is left alone, and its argument is then dropped by the caller.
fn expand_cmake_vars(raw: &str, dir: &Path) -> String {
    let here = dir.to_string_lossy().into_owned();
    let mut out = raw.to_owned();
    for var in [
        "CMAKE_CURRENT_SOURCE_DIR",
        "CMAKE_CURRENT_LIST_DIR",
        "CMAKE_SOURCE_DIR",
        "PROJECT_SOURCE_DIR",
    ] {
        out = out.replace(&format!("${{{var}}}"), &here);
    }
    out
}

fn absolutize(base: &Path, raw: &str) -> PathBuf {
    let p = Path::new(raw);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

fn push(out: &mut Vec<PathBuf>, dir: PathBuf) {
    if out.len() < MAX_DIRS && !out.contains(&dir) {
        out.push(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_command_groups_quotes_and_escapes() {
        assert_eq!(
            split_command(r#"cc -I "a b" -I'c d' -Ie\ f x.c"#),
            ["cc", "-I", "a b", "-Ic d", "-Ie f", "x.c"]
        );
    }

    #[test]
    fn include_flags_reads_joined_separated_and_equals_forms() {
        let tokens: Vec<String> = [
            "cc",
            "-Ione",
            "-I",
            "two",
            "-isystem",
            "three",
            "-iquote=four",
            "-Ldir",
            "x.c",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        assert_eq!(include_flags(&tokens), ["one", "two", "three", "four"]);
    }

    #[test]
    fn include_flags_ignores_unrelated_i_flags() {
        let tokens: Vec<String> = ["-include", "pch.h", "-Iok"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        assert_eq!(include_flags(&tokens), ["ok"]);
    }

    #[test]
    fn cmake_paths_drops_keywords_and_the_target_name() {
        assert_eq!(
            cmake_paths("fmt PUBLIC include $<BUILD_INTERFACE:src>", true),
            ["include", "src"]
        );
        assert_eq!(cmake_paths("include lib", false), ["include", "lib"]);
    }

    #[test]
    fn cmake_calls_ignores_comments_and_longer_identifiers() {
        let text = "# include_directories(nope)\nmy_include_directories(also_nope)\ninclude_directories(yes)\n";
        assert_eq!(cmake_calls(text, "include_directories"), ["yes"]);
    }
}
