//! `ripple stats`: how ripple is actually used by an agent, read from Claude Code
//! session transcripts (`~/.claude/projects/**/*.jsonl`).
//!
//! It compares ripple lookups with text searches (grep, rg, find, the Grep and Glob
//! tools) on two things a transcript records: how many tokens a result costs to read,
//! and whether the agent then used it, meaning it read or edited a file the result
//! named before searching again. That second number is a proxy for accuracy, not a
//! measurement of it: an agent can read a named file and still have been misled.
//! `eval --vs-grep` is the controlled comparison against an independent oracle.
//!
//! Sessions that chose ripple are not comparable with sessions that did not: the
//! choice follows the task. When an arms file assigns sessions at random to `ripple`
//! or `holdout` (no ripple allowed), the report also compares the two arms on what
//! a whole session cost per edit, which is the unconfounded number.

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// How many later tool calls a lookup's result gets to be used in.
const FOLLOW_WINDOW: usize = 8;

/// Tokens are estimated as characters / 4; no tokenizer is run.
fn tokens(chars: usize) -> f64 {
    chars as f64 / 4.0
}

/// One tool call, classified, with what came back.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// a ripple query: the subcommand or MCP tool (`locate`, `impact`, …)
    Ripple(String),
    /// a text search: grep, rg, find, the Grep/Glob tools
    Search,
    /// a file read, with the path read
    Read(String),
    /// a file edit or write, with the path changed
    Edit(String),
    Other,
}

#[derive(Debug, Clone)]
pub struct Call {
    pub kind: Kind,
    /// characters of the tool result
    pub result_chars: usize,
    pub is_error: bool,
    /// file paths the result mentions
    pub paths: Vec<String>,
}

/// What became of one lookup's result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// a file it named was read or edited before any other lookup
    Used,
    /// another lookup came first
    SearchedAgain,
    /// neither, within the window
    Unused,
    /// it named no file at all (an error, or nothing found)
    Empty,
}

const SEARCH_TOOLS: &[&str] = &["grep", "egrep", "fgrep", "rg", "ag", "ack", "find", "fd"];
const READ_TOOLS: &[&str] = &["cat", "head", "tail", "sed", "less", "bat"];
const RIPPLE_QUERIES: &[&str] = &[
    "locate",
    "impact",
    "review",
    "neighbors",
    "path",
    "risk",
    "search",
    "review_focus",
    "explain_edge",
];
const CODE_EXTS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "ex", "exs", "heex", "go", "py", "rb", "php",
    "java", "kt", "kts", "scala", "cs", "c", "h", "cc", "cpp", "hpp", "svelte", "vue", "gleam",
    "graphql", "gql", "html", "gd", "md", "json", "toml", "yaml", "yml", "scm", "sql",
];

/// Classify a Bash command by the first program of each pipeline (`&&`, `||`, `;`,
/// newline). A program after a single `|` reads the previous one's output, so
/// `cargo test | grep result` is a filter, not a code search.
/// ripple wins over a search in the same command, since that is what was asked.
fn classify_bash(command: &str) -> Kind {
    let mut search = false;
    let mut read: Option<String> = None;
    let pipelines = command
        .split("&&")
        .flat_map(|p| p.split("||"))
        .flat_map(|p| p.split([';', '\n']));
    for pipeline in pipelines {
        let segment = pipeline.split('|').next().unwrap_or(pipeline);
        let words: Vec<&str> = segment.split_whitespace().collect();
        // `rtk grep …`, `nice -n 19 ripple …`, `RAYON_NUM_THREADS=2 ripple …`
        let mut i = 0;
        while i < words.len() {
            let w = words[i];
            let wrapper = matches!(w, "rtk" | "nice" | "time" | "timeout") || w.contains('=');
            let wrapper_arg = w.starts_with('-') || w.chars().all(|c| c.is_ascii_digit());
            if !(wrapper || wrapper_arg) {
                break;
            }
            i += 1;
        }
        let Some(program) = words.get(i) else {
            continue;
        };
        let program = program.rsplit('/').next().unwrap_or(program);
        if program == "ripple" {
            if let Some(sub) = words.get(i + 1).filter(|s| RIPPLE_QUERIES.contains(s)) {
                return Kind::Ripple((*sub).to_owned());
            }
        } else if SEARCH_TOOLS.contains(&program)
            || (program == "git" && words.get(i + 1) == Some(&"grep"))
        {
            search = true;
        } else if READ_TOOLS.contains(&program) && read.is_none() {
            read = words[i + 1..]
                .iter()
                .rev()
                .find(|w| !w.starts_with('-') && w.contains('.'))
                .map(|w| w.trim_matches(['\'', '"']).to_owned());
        }
    }
    if search {
        Kind::Search
    } else if let Some(path) = read {
        Kind::Read(path)
    } else {
        Kind::Other
    }
}

/// Classify one `tool_use` block.
fn classify(name: &str, input: &Value) -> Kind {
    let path = || {
        input
            .get("file_path")
            .or_else(|| input.get("notebook_path"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    if let Some(tool) = name.strip_prefix("mcp__ripple__") {
        return Kind::Ripple(tool.to_owned());
    }
    match name {
        "Bash" => classify_bash(input.get("command").and_then(Value::as_str).unwrap_or("")),
        "Grep" | "Glob" => Kind::Search,
        "Read" => Kind::Read(path()),
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => Kind::Edit(path()),
        _ => Kind::Other,
    }
}

/// The text of a `tool_result` content field: a string, or a list of text blocks.
fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Source-file paths mentioned in a result: tokens with a code extension, with a
/// trailing `:line` or `:line:col` cut off. Deduplicated, first-seen order.
fn mentioned_paths(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split(|c: char| c.is_whitespace() || "()[]{},;\"'`<>".contains(c)) {
        let token = raw.split(':').next().unwrap_or(raw).trim_end_matches('.');
        let Some((stem, ext)) = token.rsplit_once('.') else {
            continue;
        };
        if stem.is_empty() || !CODE_EXTS.contains(&ext) || !stem.chars().any(char::is_alphabetic) {
            continue;
        }
        if !out.iter().any(|p| p == token) {
            out.push(token.to_owned());
        }
    }
    out
}

/// Does a read/edit target name the same file as a path a result mentioned? Results
/// print repo-relative paths, tools take absolute ones.
fn same_file(target: &str, mentioned: &str) -> bool {
    let mentioned = mentioned.trim_start_matches("./");
    target == mentioned || target.ends_with(&format!("/{mentioned}"))
}

/// One transcript's tool calls, in order, each with its result.
pub fn parse_session(text: &str) -> Vec<Call> {
    let mut order: Vec<(String, Kind)> = Vec::new();
    let mut results: HashMap<String, (usize, bool, Vec<String>)> = HashMap::new();
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(blocks) = entry.pointer("/message/content").and_then(Value::as_array) else {
            continue;
        };
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("tool_use")
                    if entry.get("type").and_then(Value::as_str) == Some("assistant") =>
                {
                    let id = block.get("id").and_then(Value::as_str).unwrap_or("");
                    let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                    let input = block.get("input").cloned().unwrap_or(Value::Null);
                    order.push((id.to_owned(), classify(name, &input)));
                }
                Some("tool_result")
                    if entry.get("type").and_then(Value::as_str) == Some("user") =>
                {
                    let id = block
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let text = result_text(block.get("content").unwrap_or(&Value::Null));
                    let is_error = block
                        .get("is_error")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    results.insert(
                        id.to_owned(),
                        (text.len(), is_error, mentioned_paths(&text)),
                    );
                }
                _ => {}
            }
        }
    }
    order
        .into_iter()
        .map(|(id, kind)| {
            let (result_chars, is_error, paths) = results.remove(&id).unwrap_or_default();
            Call {
                kind,
                result_chars,
                is_error,
                paths,
            }
        })
        .collect()
}

/// What became of the lookup at `i`.
pub fn outcome(calls: &[Call], i: usize) -> Outcome {
    let call = &calls[i];
    if call.is_error || call.paths.is_empty() {
        return Outcome::Empty;
    }
    for next in calls.iter().skip(i + 1).take(FOLLOW_WINDOW) {
        match &next.kind {
            Kind::Read(t) | Kind::Edit(t) if call.paths.iter().any(|p| same_file(t, p)) => {
                return Outcome::Used;
            }
            Kind::Ripple(_) | Kind::Search => return Outcome::SearchedAgain,
            _ => {}
        }
    }
    Outcome::Unused
}

/// Running totals for one kind of lookup.
#[derive(Default, Debug, Clone)]
pub struct Tally {
    pub calls: usize,
    pub chars: usize,
    pub used: usize,
    pub searched_again: usize,
    pub unused: usize,
    pub empty: usize,
    /// result sizes, for the median
    pub sizes: Vec<usize>,
}

impl Tally {
    fn add(&mut self, call: &Call, outcome: Outcome) {
        self.calls += 1;
        self.chars += call.result_chars;
        self.sizes.push(call.result_chars);
        match outcome {
            Outcome::Used => self.used += 1,
            Outcome::SearchedAgain => self.searched_again += 1,
            Outcome::Unused => self.unused += 1,
            Outcome::Empty => self.empty += 1,
        }
    }

    fn median_tokens(&self) -> f64 {
        let mut s = self.sizes.clone();
        s.sort_unstable();
        s.get(s.len() / 2).map_or(0.0, |&c| tokens(c))
    }

    fn pct(&self, n: usize) -> f64 {
        if self.calls == 0 {
            0.0
        } else {
            100.0 * n as f64 / self.calls as f64
        }
    }

    fn json(&self) -> Value {
        serde_json::json!({
            "calls": self.calls,
            "result_tokens_total": tokens(self.chars).round(),
            "result_tokens_mean": if self.calls == 0 { 0.0 } else { (tokens(self.chars) / self.calls as f64).round() },
            "result_tokens_median": self.median_tokens().round(),
            "used": self.used,
            "searched_again": self.searched_again,
            "unused": self.unused,
            "empty": self.empty,
        })
    }
}

/// The lookups made before one edit (since the previous edit), and what they cost.
#[derive(Default, Debug, Clone)]
pub struct Stretch {
    pub with_ripple: bool,
    pub lookups: usize,
    pub chars: usize,
}

#[derive(Default, Debug)]
pub struct Report {
    pub sessions: usize,
    pub sessions_with_ripple: usize,
    pub ripple: Tally,
    pub search: Tally,
    pub by_command: HashMap<String, Tally>,
    pub stretches: Vec<Stretch>,
}

impl Report {
    pub fn add_session(&mut self, calls: &[Call]) {
        self.sessions += 1;
        if calls.iter().any(|c| matches!(c.kind, Kind::Ripple(_))) {
            self.sessions_with_ripple += 1;
        }
        let mut stretch = Stretch::default();
        for (i, call) in calls.iter().enumerate() {
            match &call.kind {
                Kind::Ripple(sub) => {
                    let o = outcome(calls, i);
                    self.ripple.add(call, o);
                    self.by_command.entry(sub.clone()).or_default().add(call, o);
                    stretch.with_ripple = true;
                    stretch.lookups += 1;
                    stretch.chars += call.result_chars;
                }
                Kind::Search => {
                    self.search.add(call, outcome(calls, i));
                    stretch.lookups += 1;
                    stretch.chars += call.result_chars;
                }
                Kind::Edit(_) if stretch.lookups > 0 => {
                    self.stretches.push(std::mem::take(&mut stretch));
                }
                _ => {}
            }
        }
    }

    fn stretch_summary(&self, with_ripple: bool) -> (usize, f64, f64) {
        let picked: Vec<&Stretch> = self
            .stretches
            .iter()
            .filter(|s| s.with_ripple == with_ripple)
            .collect();
        let n = picked.len();
        if n == 0 {
            return (0, 0.0, 0.0);
        }
        let lookups: usize = picked.iter().map(|s| s.lookups).sum();
        let chars: usize = picked.iter().map(|s| s.chars).sum();
        (n, lookups as f64 / n as f64, tokens(chars) / n as f64)
    }

    pub fn json(&self) -> Value {
        let stretch = |w: bool| {
            let (n, lookups, toks) = self.stretch_summary(w);
            serde_json::json!({ "edits": n, "lookups_per_edit": lookups, "lookup_tokens_per_edit": toks.round() })
        };
        let mut by_command: Vec<(&String, &Tally)> = self.by_command.iter().collect();
        by_command.sort_by(|a, b| b.1.calls.cmp(&a.1.calls).then(a.0.cmp(b.0)));
        serde_json::json!({
            "token_estimate": "characters / 4",
            "sessions": self.sessions,
            "sessions_with_ripple": self.sessions_with_ripple,
            "ripple": self.ripple.json(),
            "search": self.search.json(),
            "ripple_by_command": by_command.into_iter().map(|(k, t)| (k.clone(), t.json())).collect::<serde_json::Map<_, _>>(),
            "edits_preceded_by_ripple": stretch(true),
            "edits_preceded_by_search_only": stretch(false),
        })
    }

    pub fn print(&self) {
        println!(
            "{} sessions, {} used ripple. Tokens ≈ characters / 4.",
            self.sessions, self.sessions_with_ripple
        );
        println!();
        println!("                 calls  ~tok/result (mean, median)   used  searched again  unused  empty");
        let row = |label: &str, t: &Tally| {
            let mean = if t.calls == 0 {
                0.0
            } else {
                tokens(t.chars) / t.calls as f64
            };
            println!(
                "  {label:<13} {:>6}  {:>10.0} {:>8.0}             {:>5.1}%  {:>13.1}%  {:>5.1}%  {:>4.1}%",
                t.calls,
                mean,
                t.median_tokens(),
                t.pct(t.used),
                t.pct(t.searched_again),
                t.pct(t.unused),
                t.pct(t.empty)
            );
        };
        row("ripple", &self.ripple);
        row("text search", &self.search);
        let mut by_command: Vec<(&String, &Tally)> = self.by_command.iter().collect();
        by_command.sort_by(|a, b| b.1.calls.cmp(&a.1.calls).then(a.0.cmp(b.0)));
        for (sub, t) in by_command {
            row(&format!("  {sub}"), t);
        }
        println!();
        println!("before each edit (lookups since the previous edit):");
        for (label, with) in [("with ripple", true), ("search only", false)] {
            let (n, lookups, toks) = self.stretch_summary(with);
            println!(
                "  {label:<12} {n:>5} edits  {lookups:>5.1} lookups/edit  {toks:>7.0} lookup tokens/edit"
            );
        }
        println!();
        println!(
            "\"used\": a file the result named was read or edited within the next {FOLLOW_WINDOW} \
             tool calls, before any other lookup. A proxy for usefulness, not a check that \
             the answer was right; `ripple eval --vs-grep` measures that against git history."
        );
    }
}

/// What one transcript says about its session as a whole.
#[derive(Default, Debug, Clone, PartialEq)]
pub struct SessionMeta {
    /// `sessionId`; a subagent transcript carries its parent's
    pub id: Option<String>,
    /// input tokens the model read, cache reads and writes included
    pub context_tokens: u64,
    pub output_tokens: u64,
}

/// The session id and the model's token use, from the `usage` of each assistant
/// message. A message split over several lines repeats its usage, so it counts once.
pub fn session_meta(text: &str) -> SessionMeta {
    let mut id = None;
    let mut usage: HashMap<String, (u64, u64)> = HashMap::new();
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if id.is_none() {
            id = entry
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        let (Some(msg_id), Some(u)) = (
            entry.pointer("/message/id").and_then(Value::as_str),
            entry.pointer("/message/usage"),
        ) else {
            continue;
        };
        let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
        let context =
            n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens");
        usage.insert(msg_id.to_owned(), (context, n("output_tokens")));
    }
    SessionMeta {
        id,
        context_tokens: usage.values().map(|u| u.0).sum(),
        output_tokens: usage.values().map(|u| u.1).sum(),
    }
}

/// One session's totals across its main and subagent transcripts.
#[derive(Default, Debug, Clone)]
struct SessionTotals {
    edits: usize,
    lookups: usize,
    lookup_chars: usize,
    ripple_calls: usize,
    context_tokens: u64,
    output_tokens: u64,
}

/// Sessions grouped by the arm an A/B assignment file put them in.
#[derive(Default, Debug)]
pub struct Arms {
    assignment: HashMap<String, String>,
    by_arm: BTreeMap<String, BTreeMap<String, SessionTotals>>,
}

impl Arms {
    /// Reads `{"session_id": .., "arm": ..}` lines; the latest row for a session wins, so a
    /// holdout session the user switched back to ripple counts under its new arm.
    pub fn load(text: &str) -> Self {
        let mut assignment = HashMap::new();
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if let (Some(s), Some(a)) = (
                v.get("session_id").and_then(Value::as_str),
                v.get("arm").and_then(Value::as_str),
            ) {
                assignment.insert(s.to_owned(), a.to_owned());
            }
        }
        Arms {
            assignment,
            by_arm: BTreeMap::new(),
        }
    }

    pub fn add(&mut self, meta: &SessionMeta, calls: &[Call]) {
        let Some(id) = &meta.id else { return };
        let Some(arm) = self.assignment.get(id) else {
            return;
        };
        let t = self
            .by_arm
            .entry(arm.clone())
            .or_default()
            .entry(id.clone())
            .or_default();
        t.context_tokens += meta.context_tokens;
        t.output_tokens += meta.output_tokens;
        for c in calls {
            match c.kind {
                Kind::Edit(_) => t.edits += 1,
                Kind::Ripple(_) => {
                    t.ripple_calls += 1;
                    t.lookups += 1;
                    t.lookup_chars += c.result_chars;
                }
                Kind::Search => {
                    t.lookups += 1;
                    t.lookup_chars += c.result_chars;
                }
                _ => {}
            }
        }
    }

    fn summary(sessions: &BTreeMap<String, SessionTotals>) -> Value {
        let n = sessions.len();
        let sum = |f: fn(&SessionTotals) -> f64| sessions.values().map(f).sum::<f64>();
        let edits = sum(|t| t.edits as f64);
        let per_edit = |x: f64| {
            if edits == 0.0 {
                0.0
            } else {
                (x / edits).round()
            }
        };
        let mut totals: Vec<u64> = sessions
            .values()
            .map(|t| t.context_tokens + t.output_tokens)
            .collect();
        totals.sort_unstable();
        serde_json::json!({
            "sessions": n,
            "sessions_with_edits": sessions.values().filter(|t| t.edits > 0).count(),
            "edits": edits,
            "context_tokens_per_edit": per_edit(sum(|t| t.context_tokens as f64)),
            "output_tokens_per_edit": per_edit(sum(|t| t.output_tokens as f64)),
            "median_session_tokens": totals.get(n / 2).copied().unwrap_or(0),
            "lookups_per_edit": if edits == 0.0 { 0.0 } else { sum(|t| t.lookups as f64) / edits },
            "lookup_tokens_per_edit": per_edit(tokens(sum(|t| t.lookup_chars as f64) as usize)),
            "ripple_calls": sum(|t| t.ripple_calls as f64),
        })
    }

    pub fn json(&self) -> Value {
        self.by_arm
            .iter()
            .map(|(arm, s)| (arm.clone(), Self::summary(s)))
            .collect::<serde_json::Map<_, _>>()
            .into()
    }

    pub fn print(&self) {
        println!();
        println!("A/B arms (sessions assigned at random; tokens are the model's own usage):");
        println!("              sessions  edits  context tok/edit  output tok/edit  median session tok  lookups/edit  lookup tok/edit  ripple calls");
        for (arm, s) in &self.by_arm {
            let v = Self::summary(s);
            let f = |k: &str| v[k].as_f64().unwrap_or(0.0);
            println!(
                "  {arm:<11} {:>8} {:>6} {:>17.0} {:>16.0} {:>19.0} {:>13.1} {:>16.0} {:>13.0}",
                f("sessions"),
                f("edits"),
                f("context_tokens_per_edit"),
                f("output_tokens_per_edit"),
                f("median_session_tokens"),
                f("lookups_per_edit"),
                f("lookup_tokens_per_edit"),
                f("ripple_calls"),
            );
        }
        if self.by_arm.is_empty() {
            println!("  no assigned session found in these transcripts yet");
        }
    }
}

/// `$XDG_STATE_HOME/ripple/ab.jsonl`, where the session-start hook records arms.
fn default_arms_file() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(state.join("ripple/ab.jsonl"))
}

/// Every `.jsonl` under `dir`, subagent transcripts included.
fn transcripts(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "jsonl") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// `ripple stats [--transcripts <dir>] [--project <substr>] [--exclude <substr>] [--days N] [--arms <file>] [--json]`
pub fn run(args: &[String], flag_value: impl Fn(&str) -> Option<String>) -> Result<()> {
    let json = args.iter().any(|a| a == "--json");
    let dir = match flag_value("--transcripts") {
        Some(d) => PathBuf::from(d),
        None => std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(".claude/projects"))
            .context("no --transcripts and no $HOME")?,
    };
    let project = flag_value("--project");
    let exclude = flag_value("--exclude");
    let since = flag_value("--days")
        .and_then(|d| d.parse::<u64>().ok())
        .map(|d| std::time::SystemTime::now() - std::time::Duration::from_secs(d * 86_400));

    let arms_file = flag_value("--arms")
        .map(PathBuf::from)
        .or_else(|| default_arms_file().filter(|p| p.exists()));
    let mut arms = match &arms_file {
        Some(p) => Some(Arms::load(
            &std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?,
        )),
        None => None,
    };

    let mut report = Report::default();
    for path in transcripts(&dir) {
        let shown = path.to_string_lossy();
        if project.as_deref().is_some_and(|p| !shown.contains(p))
            || exclude.as_deref().is_some_and(|x| shown.contains(x))
        {
            continue;
        }
        if let Some(since) = since {
            let fresh = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .is_ok_and(|t| t >= since);
            if !fresh {
                continue;
            }
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let calls = parse_session(&text);
        if let Some(arms) = arms.as_mut() {
            arms.add(&session_meta(&text), &calls);
        }
        report.add_session(&calls);
    }
    if json {
        let mut v = report.json();
        if let Some(arms) = &arms {
            v["arms"] = arms.json();
        }
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        report.print();
        if let Some(arms) = &arms {
            arms.print();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_use(id: &str, name: &str, input: Value) -> String {
        serde_json::json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": id, "name": name, "input": input}
        ]}})
        .to_string()
    }

    fn tool_result(id: &str, text: &str) -> String {
        serde_json::json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": id, "content": text, "is_error": false}
        ]}})
        .to_string()
    }

    #[test]
    fn bash_commands_are_classified_by_their_program() {
        assert_eq!(
            classify_bash("cd ../web && ripple locate image fallback"),
            Kind::Ripple("locate".into())
        );
        assert_eq!(
            classify_bash("RAYON_NUM_THREADS=2 nice -n 19 ~/.local/bin/ripple impact f"),
            Kind::Ripple("impact".into())
        );
        assert_eq!(
            classify_bash("ripple index ."),
            Kind::Other,
            "indexing is not a lookup"
        );
        assert_eq!(classify_bash("rtk grep -rn foo src"), Kind::Search);
        assert_eq!(classify_bash("git grep -n foo"), Kind::Search);
        assert_eq!(
            classify_bash("sed -n 1,40p src/a.rs"),
            Kind::Read("src/a.rs".into())
        );
        assert_eq!(classify_bash("cargo test"), Kind::Other);
        assert_eq!(
            classify_bash("cargo test 2>&1 | grep -E 'test result'"),
            Kind::Other,
            "a grep reading a pipe filters output, it does not search code"
        );
    }

    #[test]
    fn paths_are_read_out_of_results() {
        let text = "  1.53  Calls<0.81> lockfile_deps (crates/aegis-cli/src/scan.rs:1177)\n\
                    src/a.ts:12:  foo()\nversion 0.8.2 and v1.2";
        assert_eq!(
            mentioned_paths(text),
            vec!["crates/aegis-cli/src/scan.rs", "src/a.ts"]
        );
    }

    #[test]
    fn a_lookup_is_used_when_a_named_file_is_read_next() {
        let session = [
            tool_use(
                "1",
                "Bash",
                serde_json::json!({"command": "ripple locate login"}),
            ),
            tool_result("1", "Function login (src/auth.ts:3)"),
            tool_use(
                "2",
                "Read",
                serde_json::json!({"file_path": "/repo/src/auth.ts"}),
            ),
            tool_result("2", "..."),
            tool_use("3", "Grep", serde_json::json!({"pattern": "x"})),
            tool_result("3", "src/x.ts:1:x"),
            tool_use("4", "Grep", serde_json::json!({"pattern": "y"})),
            tool_result("4", "src/y.ts:1:y"),
            tool_use(
                "5",
                "Edit",
                serde_json::json!({"file_path": "/repo/src/auth.ts"}),
            ),
            tool_result("5", "ok"),
        ]
        .join("\n");
        let calls = parse_session(&session);
        assert_eq!(calls.len(), 5);
        assert_eq!(outcome(&calls, 0), Outcome::Used);
        assert_eq!(outcome(&calls, 2), Outcome::SearchedAgain);
        assert_eq!(
            outcome(&calls, 3),
            Outcome::Unused,
            "src/y.ts is never opened"
        );

        let mut report = Report::default();
        report.add_session(&calls);
        assert_eq!(report.ripple.calls, 1);
        assert_eq!(report.search.calls, 2);
        assert_eq!(report.stretches.len(), 1);
        assert!(report.stretches[0].with_ripple);
        assert_eq!(report.stretches[0].lookups, 3);
    }

    #[test]
    fn arms_sum_a_session_across_transcripts_and_count_usage_once() {
        let assistant = |msg: &str, input: u64, output: u64, block: Value| {
            serde_json::json!({"type": "assistant", "sessionId": "s1", "message": {
                "id": msg,
                "usage": {"input_tokens": input, "cache_read_input_tokens": 100, "output_tokens": output},
                "content": [block]
            }})
            .to_string()
        };
        let main = [
            assistant("m1", 10, 5, serde_json::json!({"type": "text", "text": "hi"})),
            assistant(
                "m1",
                10,
                5,
                serde_json::json!({"type": "tool_use", "id": "1", "name": "Edit", "input": {"file_path": "/r/a.rs"}}),
            ),
        ]
        .join("\n");
        let meta = session_meta(&main);
        assert_eq!(meta.id.as_deref(), Some("s1"));
        assert_eq!(
            (meta.context_tokens, meta.output_tokens),
            (110, 5),
            "one message split over two lines"
        );

        let sub = assistant(
            "m2",
            0,
            20,
            serde_json::json!({"type": "tool_use", "id": "2", "name": "Grep", "input": {}}),
        );
        let mut arms = Arms::load(
            "{\"session_id\":\"s1\",\"arm\":\"ripple\"}\n{\"session_id\":\"s1\",\"arm\":\"holdout\"}",
        );
        arms.add(&meta, &parse_session(&main));
        arms.add(&session_meta(&sub), &parse_session(&sub));
        arms.add(&SessionMeta::default(), &[]);
        let v = arms.json();
        assert!(v.get("ripple").is_none(), "the latest row wins");
        let h = &v["holdout"];
        assert_eq!(h["sessions"], 1);
        assert_eq!(h["edits"], 1.0);
        assert_eq!(h["output_tokens_per_edit"], 25.0);
        assert_eq!(h["lookups_per_edit"], 1.0);
    }
}
