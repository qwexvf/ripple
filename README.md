# ripple

**Code impact and review targeting, built for AI agents.** Give it a change and it tells you
what is likely to break, ranked by risk. Give it a PR and it tells you which hunks to read
first. Written in Rust. The core doesn't know about languages; each one plugs in through a
small adapter.

```bash
ripple locate "add a new language adapter"   # where do I start for this task?
ripple impact <symbol>                        # if I change this, what likely breaks?
ripple review [<base>]                         # for this PR, what do I read first?
ripple path <from> <to>                        # how does A reach B?
```

You can run ripple three ways, all over the same graph:

- a **CLI**
- an **[MCP server](docs/src/content/docs/reference/mcp.md)**, so an AI agent can find its
  way around an unfamiliar codebase before it edits (find the right symbol, see what a change
  touches, rank what to review) instead of grepping blind
- a **[daemon](docs/src/content/docs/reference/daemon.md)** that keeps the graph in memory and
  re-indexes on save, so a query is a sub-millisecond socket round-trip

## The two questions

ripple exists to answer two questions that come up constantly when working with AI on code,
and that I couldn't find an existing tool answering well:

1. **Blast radius:** "if I change `X`, what is likely to break?" The answer is a list of
   affected code *ranked by risk*, not a flat dump of everything reachable.
2. **Review targeting:** "for this PR, where do I look first?" The risky hunks, ordered by
   how likely they are to hide a bug, each with its downstream impact and a reason.

Plenty of tools build call graphs or mine git history. What ripple adds is joining a
function-level static graph with git co-change and defect-risk scoring, ranking the result,
and returning it within a size budget an agent can work with. More on that in
[`02-gap.md`](docs/src/content/docs/design/02-gap.md).

## Languages

TypeScript · TSX · Python · Go · Ruby · PHP · Rust · Elixir · Gleam · Java · Scala · Kotlin · C# · C · C++ · Svelte · Vue · GraphQL · HTML · GDScript

Adding a language only touches `crates/lang/`: a module, some `.scm` query files, and one
registry line. Nothing above the [IR boundary](docs/src/content/docs/design/04-architecture.md)
knows which language a node came from. Git-history signals work per *file*, so even a
language with thin support gets useful impact and review results from the start.

## Install

**Prebuilt binary** (Linux x86_64 and macOS arm64, from the latest release):

```bash
# Linux x86_64
curl -L https://github.com/qwexvf/ripple/releases/latest/download/ripple-x86_64-unknown-linux-gnu.tar.gz | tar xz
# macOS (Apple silicon): …/ripple-aarch64-apple-darwin.tar.gz
sudo mv ripple /usr/local/bin/            # or anywhere on your PATH
ripple --help
```

**From source** (Rust 1.85+, edition 2021):

```bash
git clone https://github.com/qwexvf/ripple && cd ripple
cargo install --path crates/cli          # installs `ripple` onto PATH
# …or just: cargo build --release        # binary at target/release/ripple
```

## Quick start

```bash
ripple index .          # build the graph → .ripple/graph.redb  (add .ripple/ to .gitignore)
```

Indexing ripple's own repo (130 files, ~2k nodes, 2.6k edges) takes a fraction of a second.
A warm incremental re-index is faster. Then ask it things:

```bash
$ ripple locate "add a new language adapter"
start here for "add a new language adapter" — 5 of 215 candidates:
  Function new (crates/lang/src/html/mod.rs:18)   26 dependents
  Function adapter_for (crates/lang/src/lib.rs:283)   9 dependents
  ...

$ ripple impact registry --budget 5
blast radius of registry — 5 of 75 hits (ranked):
  1.40  Calls<0.81> for_path (crates/lang/src/lib.rs:295)
  0.89  Calls<0.51> cmd_parse (crates/cli/src/main.rs:172)
  ...
```

Each ranked line has a score and a confidence (`Calls<0.81>`), so an inferred edge never
looks more certain than it is. Add `--json` to any query for machine output, `--root <path>`
to query a repo you're not in, and `--budget N` to cap the size of the answer.

### The commands

| Command | Answers |
|---|---|
| `ripple locate <task words>` | Where do I start for this task? |
| `ripple impact <symbol>` | If I change this, what likely breaks? |
| `ripple review [<base>]` | For this diff, what do I review first? |
| `ripple path <from> <to>` | How does A reach B? |
| `ripple neighbors <symbol>` | Direct callers / importers (`--in`/`--out`, `--depth N`) |
| `ripple risk <symbol\|file>` | Churn / co-change / bug-density / ownership risk terms |
| `ripple mcp` | MCP server over stdio for AI agents |
| `ripple daemon` | Resident, file-watching index server (see below) |
| `ripple index <path>...` | Build/refresh the graph (multi-root, incremental) |

Repos indexed together get linked to each other. For example, an HTTP call in one service is
matched to the route that handles it in another. When you need more precision, ripple can
also upgrade call edges using a language server (`--verify lsp`). The
[CLI reference](docs/src/content/docs/reference/cli.md) has every flag.

## Daemon

Most of the time a query spends is *startup*: compiling the tree-sitter queries for every
language adapter (~0.8s) before it can answer anything. `ripple daemon` does that once,
keeps each project's graph in RAM, and **re-indexes on save** through a file watcher. A query
then becomes a sub-millisecond socket round-trip instead of a cold build. One daemon serves
many projects.

```bash
ripple daemon                     # run it (foreground; a service manager keeps it up)
ripple daemon register .          # build + start watching this project
ripple daemon status              # which projects are resident, node/edge counts
ripple daemon stop
```

It stays light on a machine with lots of repos. Graphs are **loaded on demand and evicted
LRU** (RAM capped by `--max-resident`, default 8). Every rebuild goes through **one
de-duplicating queue**, so a burst of saves turns into a single re-index and CPU use stays
near one core. Watches ignore `.ripple/`, `.git/` and `node_modules/` so the daemon's own
writes don't trigger it again.

Clients talk to it with newline-delimited JSON over a Unix socket (under `$XDG_RUNTIME_DIR`
by default), which means a **systemd user unit**
([`contrib/systemd/`](contrib/systemd/ripple-daemon.service)) works out of the box:

```bash
cp contrib/systemd/ripple-daemon.service ~/.config/systemd/user/
systemctl --user enable --now ripple-daemon
ripple daemon register .
```

See the [daemon reference](docs/src/content/docs/reference/daemon.md) for details.
Linux/systemd comes first; launchd and other OS wrappers aren't done yet.

## How it works

The core (graph model, git overlay, risk scoring, impact/review queries, MCP server) doesn't
know about languages. Below it is a **thin adapter per language**. tree-sitter parses source
into a normalized IR. A resolve pass links imports and calls across files. A git overlay adds
churn, co-change, bug-density and ownership. Queries walk the in-memory graph and rank
results by a risk-weighted score. Details in
[`04-architecture.md`](docs/src/content/docs/design/04-architecture.md).

```
crates/
  ir/       normalized graph vocabulary — the decoupling seam, zero deps
  parse/    tree-sitter driver: source → IR + pre-resolution records
  lang/     LanguageAdapter trait + per-language adapters (queries as .scm data)
  resolve/  cross-file linking: discover → index_defs → link
  overlay/  git-history signals (churn, co-change, bug-density, ownership)
  store/    GraphStore trait + redb snapshot + in-memory graph
  query/    impact / review / locate / path, budget-aware ranking
  engine/   the assembled pipeline
  lsp/      optional language-server verification tier
  cli/      the `ripple` binary + MCP server
```

## Documentation

The docs live in [`docs/`](docs/) as an Astro site. Run `cd docs && bun install && bun dev`
to read them locally. Start with **[Getting started](docs/src/content/docs/getting-started.md)**
(every command run against a real repo, with the output pasted as is), then the
[CLI](docs/src/content/docs/reference/cli.md) and
[MCP](docs/src/content/docs/reference/mcp.md) references.

The design docs in [`docs/src/content/docs/design/`](docs/src/content/docs/design/) explain
the reasoning: the [architecture](docs/src/content/docs/design/04-architecture.md) and its
invariants, [risk scoring and query semantics](docs/src/content/docs/design/06-risk-and-queries.md),
[cross-service resolution](docs/src/content/docs/design/10-cross-service-resolution.md),
[LSP integration](docs/src/content/docs/design/11-lsp-integration.md), and the
[dogfood log](docs/src/content/docs/design/12-dogfood-log.md). That last one records what
ripple got wrong in real use, and it has led to more fixes than the roadmap has.

## Contributing

Read `CLAUDE.md` before writing code. It covers the working conventions and the architecture
invariants that matter. The short version: `cargo fmt --all` and
`cargo clippy --all-targets` clean, `cargo test` green, small focused PRs, and every behavior
change comes with a test. Run `git config core.hooksPath .githooks` once per clone so the
pre-push hook matches CI.

## License

[Apache-2.0](LICENSE).
