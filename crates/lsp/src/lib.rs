//! Language-server client: the optional Tier-2 accuracy layer over the
//! tree-sitter base. See docs/11-lsp-integration.md.
//!
//! Deliberately synchronous and dependency-free (no async runtime): a reader
//! thread frames messages off the server's stdout onto a channel, and requests
//! block with an explicit timeout. Nothing here mutates the graph — this crate
//! only speaks the protocol and reports what a server can do, so a slow or
//! missing server can never sit on a query's critical path.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How to launch one language server, and how much a query may trust it.
///
/// `inline` is the speed contract: servers that answer in milliseconds may be
/// consulted while a query is running, slow ones are background-warm only. The
/// value comes from measurement (`probe`), not guesswork.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerSpec {
    /// Adapter id this server serves (`typescript`, `elixir`, …).
    pub language: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Files whose presence means this server applies to a project.
    #[serde(default)]
    pub root_markers: Vec<String>,
    /// May a query block on this server?
    #[serde(default)]
    pub inline: bool,
    /// Parallel requests one client may have in flight. Recorded, not yet
    /// enforced: both `doctor` and `--verify lsp` send one request at a time per
    /// server (they fan out across *servers*, not within one).
    #[serde(default = "default_concurrency")]
    pub max_concurrency: usize,
    #[serde(default = "default_init_timeout_ms")]
    pub init_timeout_ms: u64,
    /// Budget for one query request once the handshake is done.
    #[serde(default = "default_request_timeout_ms")]
    pub request_timeout_ms: u64,
}

fn default_concurrency() -> usize {
    4
}

fn default_init_timeout_ms() -> u64 {
    30_000
}

fn default_request_timeout_ms() -> u64 {
    5_000
}

/// Built-in server table. Overridden per language by `.ripple/lsp.json`.
///
/// `inline` reflects whether a server can answer without first compiling the
/// project: `dexter` indexes Elixir from source (~11s for 57k files, ~10ms
/// queries) where `ElixirLS` compiles it, and `rust-analyzer` builds its cache
/// before it answers anything.
pub fn defaults() -> Vec<ServerSpec> {
    let spec =
        |language: &str, command: &str, args: &[&str], markers: &[&str], inline: bool| ServerSpec {
            language: language.to_owned(),
            command: command.to_owned(),
            args: args.iter().map(|s| (*s).to_owned()).collect(),
            root_markers: markers.iter().map(|s| (*s).to_owned()).collect(),
            inline,
            max_concurrency: default_concurrency(),
            init_timeout_ms: default_init_timeout_ms(),
            request_timeout_ms: default_request_timeout_ms(),
        };
    vec![
        spec("elixir", "dexter", &["lsp"], &["mix.exs"], true),
        spec(
            "typescript",
            "typescript-language-server",
            &["--stdio"],
            &["tsconfig.json", "package.json"],
            true,
        ),
        // ripple splits TS and TSX because tree-sitter has two grammars; a language
        // server does not, so both point at the same command
        spec(
            "tsx",
            "typescript-language-server",
            &["--stdio"],
            &["tsconfig.json", "package.json"],
            true,
        ),
        spec("go", "gopls", &[], &["go.mod"], true),
        spec("gleam", "gleam", &["lsp"], &["gleam.toml"], true),
        spec(
            "python",
            "pyright-langserver",
            &["--stdio"],
            &["pyproject.toml", "setup.py", "requirements.txt"],
            true,
        ),
        spec("rust", "rust-analyzer", &[], &["Cargo.toml"], false),
    ]
}

/// The server table for one root, plus anything ripple refused to take from it.
pub struct Config {
    pub specs: Vec<ServerSpec>,
    /// An in-repo config that was read but **not applied**, because the root is
    /// not trusted. Present so `doctor` can say so instead of staying quiet.
    pub untrusted: Option<Untrusted>,
}

/// An in-repo `.ripple/lsp.json` ripple declined to obey.
pub struct Untrusted {
    pub path: PathBuf,
    /// `language → command` it wanted spawned, so the user can see what they are
    /// being asked to trust before trusting it.
    pub declares: Vec<(String, String)>,
}

/// Load the server table for `root`.
///
/// Sources, in increasing precedence: built-in defaults, then the **user's own**
/// config (`$RIPPLE_LSP_CONFIG`, else `$XDG_CONFIG_HOME/ripple/lsp.json`, else
/// `~/.config/ripple/lsp.json`), then the repository's `.ripple/lsp.json` — that
/// last one **only if the root is trusted**. Each entry replaces the default for
/// its language; omitted fields take their default.
///
/// The trust step exists because a config file names a `command` that gets
/// executed, and `root` is a repository somebody else wrote. Honoring it by
/// default made cloning a repo and running `ripple lsp doctor` enough to run
/// arbitrary code as the user, which is issue #34. A repo-supplied table is
/// therefore data to be reported, not instructions to be followed, until the user
/// says otherwise with `ripple lsp trust`.
pub fn load(root: &Path) -> Result<Config> {
    load_from(root, user_config_path().as_deref(), is_trusted(root))
}

/// `load` with its two environment lookups passed in, so the policy can be tested
/// without setting process-global environment variables from several threads.
fn load_from(root: &Path, user_config: Option<&Path>, trusted: bool) -> Result<Config> {
    let mut specs = defaults();
    if let Some(path) = user_config {
        if let Some(user) = read_specs(path)? {
            apply(&mut specs, user);
        }
    }

    let repo_path = root.join(".ripple").join("lsp.json");
    let Some(repo) = read_specs(&repo_path)? else {
        return Ok(Config {
            specs,
            untrusted: None,
        });
    };
    if !trusted {
        let declares = repo
            .iter()
            .map(|s| (s.language.clone(), s.command.clone()))
            .collect();
        return Ok(Config {
            specs,
            untrusted: Some(Untrusted {
                path: repo_path,
                declares,
            }),
        });
    }
    apply(&mut specs, repo);
    Ok(Config {
        specs,
        untrusted: None,
    })
}

/// `Ok(None)` when the file simply isn't there; an unreadable or malformed one is
/// an error, because silently falling back would hide a config the user wrote.
fn read_specs(path: &Path) -> Result<Option<Vec<ServerSpec>>> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    let specs: Vec<ServerSpec> = serde_json::from_str(&text)
        .with_context(|| format!("invalid server config at {}", path.display()))?;
    Ok(Some(specs))
}

fn apply(table: &mut Vec<ServerSpec>, overrides: Vec<ServerSpec>) {
    for o in overrides {
        match table.iter_mut().find(|d| d.language == o.language) {
            Some(d) => *d = o,
            None => table.push(o),
        }
    }
}

/// The user's own config file — outside any repository, so a repository cannot
/// write it. `$RIPPLE_LSP_CONFIG` wins, for CI and for tests.
pub fn user_config_path() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("RIPPLE_LSP_CONFIG") {
        return Some(PathBuf::from(explicit));
    }
    Some(config_dir()?.join("lsp.json"))
}

fn config_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let xdg = PathBuf::from(xdg);
        if xdg.is_absolute() {
            return Some(xdg.join("ripple"));
        }
    }
    Some(
        PathBuf::from(std::env::var_os("HOME")?)
            .join(".config")
            .join("ripple"),
    )
}

/// Where trusted root paths are recorded — one absolute path per line, in the
/// user's config directory rather than in any repository.
pub fn trust_file() -> Option<PathBuf> {
    Some(config_dir()?.join("trusted-roots"))
}

/// May ripple obey `<root>/.ripple/lsp.json`?
///
/// `RIPPLE_TRUST_REPO_LSP=1` covers CI, where writing a user config is awkward and
/// the checkout is already trusted by whoever configured the job.
pub fn is_trusted(root: &Path) -> bool {
    if std::env::var("RIPPLE_TRUST_REPO_LSP").is_ok_and(|v| v == "1") {
        return true;
    }
    let Some(file) = trust_file() else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(file) else {
        return false;
    };
    listed_as_trusted(&text, root)
}

/// Is `root` one of the paths in a trust file's text? Exact path match on a
/// canonical path — no prefix rule, so trusting one repo never trusts a sibling
/// or anything nested inside it.
fn listed_as_trusted(text: &str, root: &Path) -> bool {
    let want = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .any(|l| Path::new(l) == want)
}

/// Record `root` as trusted, so its own `.ripple/lsp.json` is honored from now on.
/// Returns the trust file written, and whether it was already there.
pub fn trust(root: &Path) -> Result<(PathBuf, bool)> {
    let file = trust_file().context("no config directory (set HOME or XDG_CONFIG_HOME)")?;
    if is_trusted(root) {
        return Ok((file, true));
    }
    let canonical = root
        .canonicalize()
        .with_context(|| format!("cannot access {}", root.display()))?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut text = std::fs::read_to_string(&file).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&canonical.to_string_lossy());
    text.push('\n');
    std::fs::write(&file, text).with_context(|| format!("writing {}", file.display()))?;
    Ok((file, false))
}

/// The protocol features ripple can actually use, as reported by the server.
#[derive(Debug, Default, Clone, Serialize)]
pub struct Caps {
    /// The one that matters: function-level call edges.
    pub call_hierarchy: bool,
    pub references: bool,
    pub document_symbol: bool,
    pub workspace_symbol: bool,
    pub definition: bool,
}

impl Caps {
    fn from_result(result: &Value) -> Caps {
        let present = |key: &str| {
            !matches!(
                result.pointer(&format!("/capabilities/{key}")),
                None | Some(Value::Null) | Some(Value::Bool(false))
            )
        };
        Caps {
            call_hierarchy: present("callHierarchyProvider"),
            references: present("referencesProvider"),
            document_symbol: present("documentSymbolProvider"),
            workspace_symbol: present("workspaceSymbolProvider"),
            definition: present("definitionProvider"),
        }
    }

    /// Whether this server can supply Tier-2 call edges at all.
    pub fn usable_for_calls(&self) -> bool {
        self.call_hierarchy || self.references
    }
}

/// What `probe` found. Every variant is a fact about the environment, so
/// `doctor` can explain exactly why a language isn't getting verified edges.
#[derive(Debug, Clone, Serialize)]
pub enum Health {
    /// No root marker for this server in the project.
    NotApplicable,
    /// Configured, but the executable isn't on `PATH`.
    BinaryMissing,
    /// Started but did not complete the handshake.
    Failed { error: String, log: Vec<String> },
    Ready {
        init_ms: u128,
        caps: Caps,
        /// Server-reported name/version, when it sends one.
        server: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub language: String,
    pub command: String,
    pub inline: bool,
    pub health: Health,
}

/// Does any of this server's root markers exist at or just below `root`?
/// Umbrella and monorepo layouts keep the marker one or two levels down
/// (`apps/*/mix.exs`, `packages/*/package.json`), so a shallow scan is needed.
pub fn applies(spec: &ServerSpec, root: &Path) -> bool {
    if spec.root_markers.is_empty() {
        return true;
    }
    let has_marker = |dir: &Path| spec.root_markers.iter().any(|m| dir.join(m).exists());
    if has_marker(root) {
        return true;
    }
    let mut children: Vec<PathBuf> = read_dirs(root);
    if children.iter().any(|d| has_marker(d)) {
        return true;
    }
    children = children.iter().flat_map(|d| read_dirs(d)).collect();
    children.iter().any(|d| has_marker(d))
}

fn read_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && !is_ignored(p))
        .collect()
}

fn is_ignored(dir: &Path) -> bool {
    matches!(
        dir.file_name().and_then(|n| n.to_str()),
        Some("node_modules" | "_build" | "deps" | "target" | ".git" | ".ripple")
    )
}

/// Start the server, complete the handshake, record what it supports, shut it
/// down. Measures the handshake so `doctor` can report real latency instead of
/// asserting a server is "fast".
pub fn probe(spec: &ServerSpec, root: &Path) -> Report {
    let report = |health| Report {
        language: spec.language.clone(),
        command: spec.command.clone(),
        inline: spec.inline,
        health,
    };
    if !applies(spec, root) {
        return report(Health::NotApplicable);
    }
    if which(&spec.command).is_none() {
        return report(Health::BinaryMissing);
    }

    let started = Instant::now();
    match handshake(spec, root) {
        Ok((caps, server)) => report(Health::Ready {
            init_ms: started.elapsed().as_millis(),
            caps,
            server,
        }),
        Err((e, log)) => report(Health::Failed {
            error: format!("{e:#}"),
            log,
        }),
    }
}

/// Probe every spec concurrently and return within `budget`.
///
/// Serial probing cost `init_timeout_ms` per unresponsive server — five hung
/// servers meant 2.5 minutes before a single line of output. Each probe's own init
/// timeout is also clamped to the time left in the budget, so every thread finishes
/// on its own: nothing is abandoned mid-handshake and no server process is left
/// running after the call returns.
///
/// Results keep the order of `specs`, so output stays deterministic regardless of
/// which server answers first. Fan-out is the size of the server table (~6), which
/// is why it needs no pool.
pub fn probe_all(specs: &[ServerSpec], root: &Path, budget: Duration) -> Vec<Report> {
    let budget_ms = u64::try_from(budget.as_millis()).unwrap_or(u64::MAX);
    if budget_ms == 0 {
        // an earlier root spent the budget; say so rather than spawning servers
        // only to kill them at a 0ms timeout
        return specs
            .iter()
            .map(|s| failed(s, "the doctor budget was already spent"))
            .collect();
    }
    let bounded: Vec<ServerSpec> = specs
        .iter()
        .map(|spec| ServerSpec {
            init_timeout_ms: spec.init_timeout_ms.min(budget_ms),
            request_timeout_ms: spec.request_timeout_ms.min(budget_ms),
            ..spec.clone()
        })
        .collect();

    std::thread::scope(|scope| {
        let handles: Vec<_> = bounded
            .iter()
            .map(|spec| scope.spawn(|| probe(spec, root)))
            .collect();
        handles
            .into_iter()
            .zip(&bounded)
            .map(|(h, spec)| {
                h.join()
                    .unwrap_or_else(|_| failed(spec, "the probe thread panicked"))
            })
            .collect()
    })
}

fn failed(spec: &ServerSpec, error: &str) -> Report {
    Report {
        language: spec.language.clone(),
        command: spec.command.clone(),
        inline: spec.inline,
        health: Health::Failed {
            error: error.to_owned(),
            log: Vec::new(),
        },
    }
}

fn handshake(
    spec: &ServerSpec,
    root: &Path,
) -> Result<(Caps, Option<String>), (anyhow::Error, Vec<String>)> {
    let mut client = match Client::start(spec, root) {
        Ok(c) => c,
        Err(e) => return Err((e, Vec::new())),
    };
    let result = client.initialize(root, spec);
    let log = client.log();
    client.stop();
    result.map_err(|e| (e, log))
}

/// One reference site the server reported, mapped back to plain paths so the
/// caller never handles URIs or protocol shapes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSite {
    /// Absolute path of the file containing the caller.
    pub path: PathBuf,
    /// The calling function's name, as the server names it.
    pub name: String,
    /// 1-based line of the caller's own definition.
    pub line: u32,
    /// 1-based lines of the actual call expressions inside the caller
    /// (`fromRanges`), sorted and deduplicated.
    ///
    /// Separate from `line` because the two disagree, and the disagreement is the
    /// point: dexter credits a call made inside an ExUnit `test` block to the
    /// preceding `defp`, so `name`/`line` can name a function that does not contain
    /// the call. Whoever compares against ripple's spans needs the call's own
    /// position to attribute it honestly. Empty if the server sends none.
    pub call_lines: Vec<u32>,
}

/// A function the server found in a file, with the position to ask about it.
///
/// Positions come straight from the server and go straight back to it, so no
/// UTF-16 column arithmetic is involved — the reason the oracle starts from
/// `documentSymbol` rather than from ripple's own spans.
#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    /// LSP-native position of the name (0-based line, UTF-16 character).
    pub line: u32,
    pub character: u32,
}

/// Keep a version string printable. `gopls` answers with its entire build-info
/// JSON, which is not a version.
fn short_version(v: &str) -> Option<String> {
    let v = v.trim();
    if v.is_empty() || v.starts_with('{') || v.contains('\n') {
        return None;
    }
    Some(v.chars().take(40).collect())
}

fn initialize_params(root: &Path) -> Value {
    let uri = format!("file://{}", root.display());
    json!({
        "processId": std::process::id(),
        "clientInfo": { "name": "ripple" },
        "rootUri": uri,
        "workspaceFolders": [{ "uri": uri, "name": "root" }],
        // advertise only what ripple consumes, so servers report the matching
        // provider capabilities back
        "capabilities": {
            "textDocument": {
                "callHierarchy": { "dynamicRegistration": false },
                "references": { "dynamicRegistration": false },
                "definition": { "dynamicRegistration": false },
                "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
            },
            "workspace": { "symbol": { "dynamicRegistration": false } },
        },
    })
}

fn file_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn uri_to_path(uri: &str) -> Option<PathBuf> {
    uri.strip_prefix("file://").map(PathBuf::from)
}

/// Function-like symbols out of either `documentSymbol` shape: the hierarchical
/// `DocumentSymbol` tree or the flat `SymbolInformation` list.
fn collect_symbols(node: &Value, out: &mut Vec<Symbol>) {
    // SymbolKind: 6 = Method, 12 = Function, 13 = Variable, 14 = Constant.
    //
    // Variables and constants are in because a callable is routinely declared as
    // one — `const useAuthGuard = () => …`, `let handler = function …` — and a
    // server reports it with that kind. Nothing is trusted on this basis alone: the
    // caller still has to find a symbol of that name in that file, so an import
    // binding or a plain value simply matches nothing.
    const FUNCTION_KINDS: [u64; 4] = [6, 12, 13, 14];
    match node {
        Value::Array(items) => {
            for i in items {
                collect_symbols(i, out);
            }
        }
        Value::Object(map) => {
            let kind = map.get("kind").and_then(Value::as_u64);
            let name = map.get("name").and_then(Value::as_str);
            // `selectionRange` is the name itself; `location` is the flat form
            let pos = node
                .pointer("/selectionRange/start")
                .or_else(|| node.pointer("/location/range/start"));
            if let (Some(kind), Some(name), Some(pos)) = (kind, name, pos) {
                if FUNCTION_KINDS.contains(&kind) {
                    if let (Some(line), Some(character)) = (
                        pos.get("line").and_then(Value::as_u64),
                        pos.get("character").and_then(Value::as_u64),
                    ) {
                        out.push(Symbol {
                            name: name.to_owned(),
                            line: line as u32,
                            character: character as u32,
                        });
                    }
                }
            }
            if let Some(children) = map.get("children") {
                collect_symbols(children, out);
            }
        }
        _ => {}
    }
}

fn caller_of(call: &Value) -> Option<CallSite> {
    let from = call.get("from")?;
    let mut call_lines: Vec<u32> = call
        .get("fromRanges")
        .and_then(Value::as_array)
        .map(|rs| {
            rs.iter()
                .filter_map(|r| r.pointer("/start/line")?.as_u64())
                .map(|l| l as u32 + 1)
                .collect()
        })
        .unwrap_or_default();
    call_lines.sort_unstable();
    call_lines.dedup();
    Some(CallSite {
        path: uri_to_path(from.get("uri")?.as_str()?)?,
        name: from.get("name")?.as_str()?.to_owned(),
        line: from.pointer("/range/start/line")?.as_u64()? as u32 + 1,
        call_lines,
    })
}

/// A `Location` (or `LocationLink`) from a references reply → `(file, 1-based line)`.
fn location_of(item: &Value) -> Option<(PathBuf, u32)> {
    let uri = item
        .get("uri")
        .or_else(|| item.get("targetUri"))?
        .as_str()?;
    let line = item
        .pointer("/range/start/line")
        .or_else(|| item.pointer("/targetRange/start/line"))?
        .as_u64()?;
    Some((uri_to_path(uri)?, line as u32 + 1))
}

fn which(command: &str) -> Option<PathBuf> {
    if command.contains('/') {
        let p = PathBuf::from(command);
        return is_executable(&p).then_some(p);
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(command))
            .find(|p| is_executable(p))
    })
}

/// A readable file is not a runnable server. `doctor` exists to diagnose, so
/// reporting a non-executable file as present and then failing at spawn time is
/// the one thing it must not do.
fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    true
}

/// A running server. Requests are id-matched and time-bounded; unsolicited
/// notifications (`$/progress`, `window/logMessage`) are skipped while waiting.
pub struct Client {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    log: Arc<Mutex<Vec<String>>>,
    next_id: i64,
    /// Per-request budget after the handshake; a slow server must never hang a
    /// query (docs/11-lsp-integration.md).
    timeout: Duration,
    language_id: String,
}

impl Client {
    /// `cwd` is the project root: servers that keep an on-disk index locate it
    /// relative to the working directory, not from `rootUri` alone.
    pub fn start(spec: &ServerSpec, cwd: &Path) -> Result<Client> {
        let mut child = Command::new(&spec.command)
            .args(&spec.args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("cannot start {}", spec.command))?;

        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let stderr = child.stderr.take().context("no stderr")?;

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Some(msg) = read_message(&mut reader) {
                if tx.send(msg).is_err() {
                    break;
                }
            }
        });

        // Drain stderr so a chatty server can't fill its pipe and block, and
        // keep the tail as diagnostics for a failed handshake.
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&log);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Ok(mut l) = sink.lock() {
                    if l.len() == 20 {
                        l.remove(0);
                    }
                    l.push(line);
                }
            }
        });

        Ok(Client {
            child,
            stdin,
            rx,
            log,
            next_id: 1,
            timeout: Duration::from_millis(spec.request_timeout_ms),
            language_id: spec.language.clone(),
        })
    }

    /// Handshake. Returns what the server supports and how it identifies itself.
    pub fn initialize(&mut self, root: &Path, spec: &ServerSpec) -> Result<(Caps, Option<String>)> {
        let timeout = Duration::from_millis(spec.init_timeout_ms);
        let result = self.request("initialize", initialize_params(root), timeout)?;
        // fire-and-forget, so a failed write here must not fail the handshake: a
        // server that exits right after answering `initialize` (a crash, or a stub
        // scripted to) closed the pipe before this landed, and surfacing that as a
        // bare "Broken pipe" hid a completed handshake behind a write error. The
        // next request reports the death with context.
        let _ = self.notify("initialized", json!({}));
        self.timeout = Duration::from_millis(spec.request_timeout_ms);
        self.language_id = spec.language.clone();
        let server = result
            .pointer("/serverInfo/name")
            .and_then(Value::as_str)
            .map(|name| {
                match result
                    .pointer("/serverInfo/version")
                    .and_then(Value::as_str)
                    .and_then(short_version)
                {
                    Some(v) => format!("{name} {v}"),
                    None => name.to_owned(),
                }
            });
        Ok((Caps::from_result(&result), server))
    }

    /// Tell the server about a file. Servers that answer from their own index
    /// don't need this, but ones that only know open documents do, and it's
    /// cheap either way.
    pub fn open(&mut self, path: &Path) -> Result<()> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": file_uri(path),
                "languageId": self.language_id,
                "version": 1,
                "text": text,
            }}),
        )
    }

    /// The functions and methods a file defines, with server-native positions.
    pub fn functions(&mut self, path: &Path) -> Result<Vec<Symbol>> {
        let result = self.request(
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": file_uri(path)}}),
            self.timeout,
        )?;
        let mut out = Vec::new();
        collect_symbols(&result, &mut out);
        Ok(out)
    }

    /// Callers of the symbol at `line`/`character`, via the call hierarchy.
    /// An empty result and "the server has no idea" are indistinguishable in the
    /// protocol, so a symbol it can't prepare yields `None`, not an empty set —
    /// the difference decides whether a missing ripple edge counts against us.
    pub fn incoming_calls(
        &mut self,
        path: &Path,
        line: u32,
        character: u32,
    ) -> Result<Option<Vec<CallSite>>> {
        let at = json!({
            "textDocument": {"uri": file_uri(path)},
            "position": {"line": line, "character": character},
        });
        let items = self.request("textDocument/prepareCallHierarchy", at, self.timeout)?;
        let Some(item) = items.as_array().and_then(|a| a.first()) else {
            return Ok(None);
        };
        let calls = self.request(
            "callHierarchy/incomingCalls",
            json!({"item": item}),
            self.timeout,
        )?;
        let Some(calls) = calls.as_array() else {
            return Ok(None);
        };
        Ok(Some(calls.iter().filter_map(caller_of).collect()))
    }

    /// Every place the symbol at `line`/`character` is referenced, as
    /// `(file, 1-based line)`.
    ///
    /// The fallback for servers with no call hierarchy — `gleam lsp` reports
    /// `callHierarchy=false` and `references=true`, and without this a Gleam index
    /// has symbols and no static edges at all. A reference is weaker evidence than
    /// an incoming call: it may be a type mention rather than a call, which is why
    /// what gets built from it is an `EdgeKind::References` edge and not a `Calls`
    /// one. `None` means the server could not answer, the same distinction
    /// `incoming_calls` makes; an empty vector means "no references".
    pub fn references(
        &mut self,
        path: &Path,
        line: u32,
        character: u32,
    ) -> Result<Option<Vec<(PathBuf, u32)>>> {
        let result = self.request(
            "textDocument/references",
            json!({
                "textDocument": {"uri": file_uri(path)},
                "position": {"line": line, "character": character},
                "context": {"includeDeclaration": false},
            }),
            self.timeout,
        )?;
        let Some(items) = result.as_array() else {
            return Ok(None);
        };
        Ok(Some(items.iter().filter_map(location_of).collect()))
    }

    pub fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        // a write failure here means the child is gone; say that rather than
        // surfacing a bare "broken pipe"
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .with_context(|| format!("server exited before {method} could be sent"))?;

        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("{method} timed out after {:?}", timeout);
            }
            let msg = match self.rx.recv_timeout(left) {
                Ok(m) => m,
                Err(RecvTimeoutError::Timeout) => bail!("{method} timed out after {:?}", timeout),
                Err(RecvTimeoutError::Disconnected) => bail!("server exited during {method}"),
            };
            // a server → client *request* blocks the server until it is answered:
            // tsgo sends client/registerCapability right after `initialized` and
            // stops serving anything else until it gets a reply, so ignoring these
            // looks exactly like a server that never answers.
            if msg.get("method").is_some() && msg.get("id").is_some() {
                self.answer(&msg)?;
                continue;
            }
            if msg.get("id").and_then(Value::as_i64) != Some(id) {
                continue; // a notification, or an answer we're not waiting for
            }
            if let Some(err) = msg.get("error") {
                bail!("{method} failed: {err}");
            }
            return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// Reply to a request the *server* made. We implement none of them, so the
    /// answer is the protocol's "nothing to say": `null`, except
    /// `workspace/configuration`, which must return one entry per item asked for.
    /// The point is only to unblock the server, so an unknown method is answered
    /// rather than refused — a refusal makes some servers retry forever.
    fn answer(&mut self, req: &Value) -> Result<()> {
        let method = req
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let result = if method == "workspace/configuration" {
            let n = req
                .pointer("/params/items")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            Value::Array(vec![Value::Null; n])
        } else {
            Value::Null
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        self.send(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }

    pub fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    fn send(&mut self, msg: Value) -> Result<()> {
        let body = serde_json::to_vec(&msg)?;
        let mut write = || -> std::io::Result<()> {
            write!(self.stdin, "Content-Length: {}\r\n\r\n", body.len())?;
            self.stdin.write_all(&body)?;
            self.stdin.flush()
        };
        write().context("writing to the language server's stdin")
    }

    /// The tail of the server's stderr.
    pub fn log(&self) -> Vec<String> {
        self.log.lock().map(|l| l.clone()).unwrap_or_default()
    }

    /// Ask politely, then make sure the process is gone either way.
    pub fn stop(mut self) {
        let _ = self.request("shutdown", Value::Null, Duration::from_secs(2));
        let _ = self.notify("exit", Value::Null);
        self.kill();
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A dropped client must not leave a language server running — callers that hit
/// an error path never get to call `stop`.
impl Drop for Client {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Largest message we'll allocate for. Real responses are far smaller; a bigger
/// `Content-Length` means a desynced stream or a non-LSP process on the pipe, and
/// we shouldn't try to allocate our way through it.
const MAX_FRAME_BYTES: usize = 64 << 20;

/// Read one `Content-Length`-framed JSON-RPC message. `None` at EOF or on a
/// frame we can't parse — the caller treats that as the server going away.
fn read_message(reader: &mut impl BufRead) -> Option<Value> {
    let mut len = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None; // EOF
        }
        let line = line.trim_end();
        if line.is_empty() {
            break; // end of headers
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            len = v.trim().parse::<usize>().ok();
        }
    }
    let len = len.filter(|n| *n <= MAX_FRAME_BYTES)?;
    let mut body = vec![0; len];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repo whose `.ripple/lsp.json` names servers, for the trust tests.
    fn repo_with_config(tag: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ripple-lsp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".ripple")).unwrap();
        std::fs::write(dir.join(".ripple/lsp.json"), body).unwrap();
        dir
    }

    /// The extra language is deliberately one the built-in table does not cover,
    /// so "the repo added this" stays distinguishable from "we ship this" even as
    /// the defaults grow.
    const REPO_CONFIG: &str = r#"[
             {"language": "elixir", "command": "lexical", "inline": false},
             {"language": "zig", "command": "zls"}
           ]"#;

    #[test]
    fn a_trusted_repo_config_overrides_one_language_and_adds_others() {
        let dir = repo_with_config("trusted", REPO_CONFIG);

        let cfg = load_from(&dir, None, true).unwrap();
        let specs = &cfg.specs;
        assert!(cfg.untrusted.is_none());
        let elixir = specs.iter().find(|s| s.language == "elixir").unwrap();
        assert_eq!(elixir.command, "lexical", "override replaces the default");
        assert!(!elixir.inline);
        assert_eq!(
            elixir.max_concurrency,
            default_concurrency(),
            "omitted fields keep their default"
        );
        assert!(specs.iter().any(|s| s.language == "zig" && s.command == "zls"));
        // untouched defaults survive
        assert!(specs
            .iter()
            .any(|s| s.language == "go" && s.command == "gopls"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The #34 case: a repository that names a command must not get it run just
    /// because someone pointed ripple at the repository.
    #[test]
    fn an_untrusted_repo_config_is_reported_and_not_applied() {
        let dir = repo_with_config(
            "untrusted",
            r#"[{"language": "go", "command": "/bin/touch", "args": ["/tmp/pwned"], "root_markers": ["go.mod"]}]"#,
        );

        let cfg = load_from(&dir, None, false).unwrap();
        assert!(
            cfg.specs
                .iter()
                .any(|s| s.language == "go" && s.command == "gopls"),
            "go must still be the built-in gopls, not the repo's command"
        );
        assert!(!cfg.specs.iter().any(|s| s.command == "/bin/touch"));
        let untrusted = cfg.untrusted.expect("the ignored config is reported");
        assert_eq!(
            untrusted.declares,
            [("go".to_owned(), "/bin/touch".to_owned())],
            "doctor needs to show what it refused to run"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// User config lives outside any repository, so it is trusted; and an
    /// untrusted repo cannot override it.
    #[test]
    fn user_config_applies_and_outranks_an_untrusted_repo() {
        let dir = repo_with_config("usercfg", REPO_CONFIG);
        let user = dir.join("user-lsp.json");
        std::fs::write(
            &user,
            r#"[{"language": "elixir", "command": "expert", "inline": true}]"#,
        )
        .unwrap();

        let cfg = load_from(&dir, Some(&user), false).unwrap();
        let elixir = cfg.specs.iter().find(|s| s.language == "elixir").unwrap();
        assert_eq!(elixir.command, "expert");
        assert!(
            !cfg.specs.iter().any(|s| s.language == "zig"),
            "the untrusted repo's extra language is not added either"
        );
        assert!(cfg.untrusted.is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_config_yields_defaults() {
        let cfg = load_from(Path::new("/nonexistent-ripple-root"), None, false).unwrap();
        assert_eq!(cfg.specs.len(), defaults().len());
        assert!(cfg.untrusted.is_none());
    }

    #[test]
    fn trust_is_an_exact_path_match() {
        let dir = std::env::temp_dir().join(format!("ripple-trust-{}", std::process::id()));
        let nested = dir.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let listed = dir.canonicalize().unwrap();
        let text = format!("# roots I wrote myself\n\n{}\n", listed.display());

        assert!(listed_as_trusted(&text, &dir));
        assert!(
            !listed_as_trusted(&text, &nested),
            "trusting a repo must not trust anything inside it"
        );
        assert!(!listed_as_trusted("", &dir));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn root_markers_are_found_below_the_root() {
        let dir = std::env::temp_dir().join(format!("ripple-lsp-mark-{}", std::process::id()));
        let nested = dir.join("apps").join("web");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("mix.exs"), "").unwrap();

        let elixir = defaults()
            .into_iter()
            .find(|s| s.language == "elixir")
            .unwrap();
        // an umbrella keeps mix.exs two levels down
        assert!(applies(&elixir, &dir));

        let go = defaults().into_iter().find(|s| s.language == "go").unwrap();
        assert!(!applies(&go, &dir), "no go.mod anywhere");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn version_strings_stay_printable() {
        assert_eq!(short_version("v0.21.1").as_deref(), Some("v0.21.1"));
        // gopls returns its whole build-info blob here
        assert!(short_version(r#"{"GoVersion":"go1.26.2","Deps":[]}"#).is_none());
        assert!(short_version("").is_none());
        assert_eq!(short_version(&"x".repeat(100)).map(|v| v.len()), Some(40));
    }

    #[test]
    fn frames_round_trip() {
        let body = r#"{"jsonrpc":"2.0","id":7,"result":{"capabilities":{}}}"#;
        let framed = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        let msg = read_message(&mut BufReader::new(framed.as_bytes())).unwrap();
        assert_eq!(msg["id"], 7);

        // headers we don't care about, and a truncated frame
        let with_type = format!(
            "Content-Length: {}\r\nContent-Type: application/vscode-jsonrpc\r\n\r\n{body}",
            body.len()
        );
        assert!(read_message(&mut BufReader::new(with_type.as_bytes())).is_some());
        assert!(read_message(&mut BufReader::new(&b"Content-Length: 99\r\n\r\n{}"[..])).is_none());
        // an absurd length is a desynced stream, not something to allocate for
        let huge = format!("Content-Length: {}\r\n\r\n", MAX_FRAME_BYTES + 1);
        assert!(read_message(&mut BufReader::new(huge.as_bytes())).is_none());
        assert!(read_message(&mut BufReader::new(&b""[..])).is_none());
    }

    #[test]
    fn capabilities_read_both_bool_and_object_forms() {
        let caps = Caps::from_result(&json!({
            "capabilities": {
                "callHierarchyProvider": true,
                "referencesProvider": { "workDoneProgress": true },
                "documentSymbolProvider": false,
                "definitionProvider": null,
            }
        }));
        assert!(caps.call_hierarchy);
        assert!(caps.references, "an object provider counts as supported");
        assert!(!caps.document_symbol);
        assert!(!caps.definition);
        assert!(!caps.workspace_symbol, "absent means unsupported");
        assert!(caps.usable_for_calls());
    }
}
