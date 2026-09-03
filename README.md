# ripple

**An AI-native code impact & review-targeting engine.** Give it a change, get back a
risk-ranked blast radius. Give it a PR, get back the hunks to review first. Built in Rust,
language-agnostic core over a thin per-language adapter seam.

```bash
ripple locate "add a new language adapter"   # where do I start for this task?
ripple impact <symbol>                        # if I change this, what likely breaks?
ripple review [<base>]                         # for this PR, what do I read first?
ripple path <from> <to>                        # how does A reach B?
```

Ripple runs three ways over the same graph: a **CLI**, an
**[MCP server](docs/src/content/docs/reference/mcp.md)** so an AI agent can orient in an
unfamiliar codebase before it edits — find the right symbol, see the blast radius, rank the
review surface — instead of grepping blind, and a
**[resident daemon](docs/src/content/docs/reference/daemon.md)** that keeps the graph hot
and re-indexes on save, turning a query into a sub-millisecond socket round-trip.

## The two questions

Everything here exists to answer two questions AI-assisted development actually needs, and
that no shipping tool answers well:

1. **Blast radius** — "if I change `X`, what is likely to break?" — a *risk-ranked* set of
   impacted code, not a flat reachability dump.
2. **Review targeting** — "for this PR, where do I look first?" — the risky hunks ordered by
   how likely they are to hide a defect, each with its downstream impact and a reason.

The whitespace ripple fills is the **join + ranking function** across a function-level
static graph, git co-change, and defect-risk scoring — served through a budget-aware
interface an agent can actually consume. See [`02-gap.md`](docs/src/content/docs/design/02-gap.md).

## Languages

TypeScript · TSX · Python · Go · Ruby · PHP · Rust · Elixir · Gleam · Java · Scala · Kotlin · C# · C · C++ · Svelte · Vue · GraphQL · HTML

Adding one touches only `crates/lang/` — a module plus `.scm` query files plus one
registry line. Every layer above the [IR boundary](docs/src/content/docs/design/04-architecture.md)
is blind to which language produced a node, and git-history signals work at *file*
granularity, so a barely-supported language still gets useful impact/review from day one.

## Install

**Prebuilt binary** — Linux x86_64 and macOS arm64, from the latest release:

```bash
# Linux x86_64
curl -L https://github.com/qwexvf/ripple/releases/latest/download/ripple-x86_64-unknown-linux-gnu.tar.gz | tar xz
# macOS (Apple silicon): …/ripple-aarch64-apple-darwin.tar.gz
sudo mv ripple /usr/local/bin/            # or anywhere on your PATH
ripple --help
```

**From source** — Rust 1.85+ (edition 2021):

```bash
git clone https://github.com/qwexvf/ripple && cd ripple
cargo install --path crates/cli          # installs `ripple` onto PATH
# …or just: cargo build --release        # binary at target/release/ripple
```

## Quick start

```bash
ripple index .          # build the graph → .ripple/graph.redb  (add .ripple/ to .gitignore)
```

Indexing ripple's own repo — 130 files, ~2k nodes / 2.6k edges — takes a fraction of a
second; warm incremental re-index is faster still. Then ask it things:

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

Every ranked line carries a score and a confidence (`Calls<0.81>`) — inferred edges never
fabricate certainty. Add `--json` to any query for machine output, `--root <path>` to query
from elsewhere, and `--budget N` to cap the answer to what fits.

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

Ripple also cross-links **across repositories** indexed together — an HTTP call site in one
service matched to the route that serves it in another — and can **upgrade call edges from a
language server** (`--verify lsp`) when precision matters. See the
[CLI reference](docs/src/content/docs/reference/cli.md) for the full flag surface.

## Daemon

The one real cost of a query is *startup* — compiling every language adapter's tree-sitter
queries (~0.8s) before it can answer. `ripple daemon` pays that once, keeps each project's
graph resident in RAM, and **re-indexes on save** via a file watcher, so a query is a
sub-millisecond socket round-trip instead of a cold build. One daemon serves many projects.

```bash
ripple daemon                     # run it (foreground; a service manager keeps it up)
ripple daemon register .          # build + start watching this project
ripple daemon status              # which projects are resident, node/edge counts
ripple daemon stop
```

It stays bounded on a machine full of repos: graphs are **demand-loaded and LRU-evicted**
(RAM capped by `--max-resident`, default 8), every rebuild goes through **one
de-duplicating queue** (a burst of saves collapses to a single re-index, CPU near one
core), and watches ignore `.ripple/`/`.git/`/`node_modules/` so the daemon's own writes
don't loop.

Clients speak newline-delimited JSON over a Unix socket (under `$XDG_RUNTIME_DIR` by
default), so a **systemd user unit** ([`contrib/systemd/`](contrib/systemd/ripple-daemon.service))
just works:

```bash
cp contrib/systemd/ripple-daemon.service ~/.config/systemd/user/
systemctl --user enable --now ripple-daemon
ripple daemon register .
```

Full details in the [daemon reference](docs/src/content/docs/reference/daemon.md).
(Linux/systemd first; launchd and other-OS wrappers are future work.)

## How it works

A **language-agnostic core** — graph model, git overlay, risk scoring, impact/review
queries, MCP server — sits above a **thin per-language adapter** seam. Source parses to a
normalized IR via tree-sitter; a resolve pass links imports and calls across files; a git
overlay layers churn / co-change / bug-density / ownership; queries traverse the in-memory
graph and rank by a risk-weighted score. See
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

Prose lives in [`docs/`](docs/), an Astro site — `cd docs && bun install && bun dev` to read
it locally. Start with **[Getting started](docs/src/content/docs/getting-started.md)** (every
command run against a real repo, output pasted verbatim), then the
[CLI](docs/src/content/docs/reference/cli.md) and
[MCP](docs/src/content/docs/reference/mcp.md) references.

The design docs under [`docs/src/content/docs/design/`](docs/src/content/docs/design/) cover
the reasoning in depth — the [architecture](docs/src/content/docs/design/04-architecture.md)
and its invariants, [risk scoring and query semantics](docs/src/content/docs/design/06-risk-and-queries.md),
[cross-service resolution](docs/src/content/docs/design/10-cross-service-resolution.md),
[LSP integration](docs/src/content/docs/design/11-lsp-integration.md), and the
[dogfood log](docs/src/content/docs/design/12-dogfood-log.md) — what ripple got wrong when
used for real, which has produced more committed fixes than the roadmap has.

## Contributing

`CLAUDE.md` documents the working conventions and the load-bearing architecture invariants —
read it before writing code. In short: `cargo fmt --all` clean, `cargo clippy --all-targets`
clean, `cargo test` green; small focused PRs; behavior ships with a test. Run
`git config core.hooksPath .githooks` once per clone so the pre-push hook mirrors CI.

## License

[Apache-2.0](LICENSE).
