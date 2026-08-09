---
name: ripple-orient
description: Orient in a codebase with ripple before editing — turn a task into the symbols to start from, the blast radius of a change, and the hunks to review first. Use at the start of an "implement X / add X / fix X" task to find where to start, when asked "what breaks if I change X", "who calls X", "how does A reach B", or "what should I review in this PR". Works across languages and across repos indexed together (TypeScript, TSX, Elixir, GraphQL, Rust, Go, Gleam).
---

# Orient with ripple before you edit

Ripple answers questions about *relationships* — where a task lives, what a change
breaks, what to review — over a graph of the code, ranked by risk. Reach for it
instead of grepping the whole repo and reading files uniformly: one call returns a
ranked, reasoned starting set, so you spend attention (and tokens) where they matter.

It is **not** a grep replacement for "find the string `foo`". Use it when the
question is structural: entrypoints, callers, blast radius, cross-service reach.

## Install

**One command** (prebuilt binary + this skill + a CLAUDE.md note on when to use
ripple, patched into the target repo). Re-runnable; add `--mcp` to also register the
MCP server:

```
curl -fsSL https://raw.githubusercontent.com/qwexvf/ripple/main/install.sh \
  | bash -s -- --target /path/to/your/repo --mcp
```

**By hand instead:**

```
# 1. binary — prebuilt, no compile (macOS: aarch64-apple-darwin / x86_64-apple-darwin)
curl -fsSL https://github.com/qwexvf/ripple/releases/latest/download/ripple-x86_64-unknown-linux-gnu.tar.gz \
  | tar -xz && install -m755 ripple ~/.local/bin/ripple
# 2. this skill, available in every repo
mkdir -p ~/.claude/skills/ripple-orient && curl -fsSL \
  https://raw.githubusercontent.com/qwexvf/ripple/main/.claude/skills/ripple-orient/SKILL.md \
  -o ~/.claude/skills/ripple-orient/SKILL.md
```

Contributors hacking on ripple itself build from a clone instead:
`cargo install --path crates/cli`.

Then either wire it as an MCP server (an agent calls the tools directly)…

```
claude mcp add ripple -- ripple mcp --root /path/to/your/repo
```

…or skip MCP and use the CLI (`ripple locate …`, `ripple impact …`). The MCP server
**indexes on first tool call**, so pointing it at a never-indexed repo just works; from
the CLI, run `ripple index <path>` once first.

Make this skill available everywhere (not just inside the ripple repo) by copying it
into your user skills dir — otherwise it only loads for repos that carry it under
`.claude/skills/`:

```
cp -r .claude/skills/ripple-orient ~/.claude/skills/
```

Verify: `ripple --help` lists `locate`, and `/ripple-orient` is offered as a skill.

## The one rule

**Start every implement/change task with `locate`, not with reading files.** You are
usually handed a task in words ("add rate limiting to login"), not a symbol. `locate`
maps the words to the symbols to start from; then `impact`/`neighbors` drill in from a
name it hands you. Guessing a name and grepping wastes turns.

## Setup (once per repo)

The MCP server indexes on first use, so a tool call just works. To index by hand, or
to trace across repositories, use the CLI:

```
ripple index <path>...          # one graph → <path0>/.ripple/graph.redb
ripple index <web> <api>        # several repos → ONE graph (db under the first root)
```

Over MCP (`ripple mcp --root <path>`) the tools below are exposed directly. From a
shell, every tool except `search`/`locate`-first-time is also a CLI subcommand with
`--json`.

## Workflow

```
1. locate "<the task in plain words>"      → ranked seed symbols, each with why + a
                                              1-hop blast preview (touches)
2. impact <seed>                           → full risk-ranked blast radius of changing it
3. neighbors <sym> --in | path <a> <b>     → drill: exact callers, or how A reaches B
4. …make the edit…
5. reindex                                 → the graph is now stale until you do
6. review [<base>]                         → rank the changed hunks to check first;
                                              flags untested changes + missing co-change
```

You rarely need all six. For "where do I start" step 1 is often enough — each seed
already carries its top dependents. For "what will I break" do 1→2. For "review my
change" do 5→6.

## The tools

| Tool | Give it | Get back |
|---|---|---|
| `locate` | `task` (plain words), `budget`(10) | Ranked start symbols across all repos; each with `why` (which field matched), `centrality`, `repo`, and `touches` (top dependents). **Call first for a task.** |
| `search` | `query`, `limit`(30) | Symbols/files by name — to **disambiguate a known name** for the other tools. |
| `impact` | `symbol`, `budget`(20) | Risk-ranked blast radius (what depends on it), across languages/services. |
| `neighbors` | `symbol`, `direction`(in/out), `depth`, `limit` | One hop: `in` = callers/importers, `out` = callees/imports. |
| `path` | `from`, `to`, `depth`(6), `limit`(3) | Routes A→B along dependency direction, shortest first, with edge-confidence product. |
| `review_focus` | `base` (default = working tree vs HEAD), `budget` | Changed symbols ranked by review priority, + untested + missing-co-change flags. |
| `risk` | `target` (symbol or file) | churn / bug-density / ownership / composite. |
| `explain_edge` | `from`, `to` | Why two nodes connect: edge kind, confidence, provenance, site. Use when an edge looks wrong. |
| `reindex` | — | Rebuild from current source. **Call after editing** — staleness is never auto-detected. |

CLI names differ slightly: the CLI tool is `ripple review` (not `review_focus`);
`reindex` and `explain_edge` are MCP-only; everything else is a `ripple <name>`
subcommand. Every CLI query takes `--root <p>` and `--json`.

## Read the payload — it carries more than values

- **`why` on a `locate` seed** — `name:login`, `route:login`, `doc:login`, `module:auth`
  — tells you *why* it surfaced. A `route:`/`name:` hit is a stronger start than a lone
  `doc:` hit.
- **`touches` on a seed** — the top dependents, so you see the blast radius before
  opening the file. If a seed touches something surprising, that is the thing to check.
- **`confidence` on every edge** — `1.0` extracted from source; lower is inferred; where
  resolution is ambiguous, confidence is split across candidates rather than one edge
  fabricated. A `0.3` edge is a three-way guess — verify before acting on it.
- **`{shown, total}` / `reason`** — truncation is always declared. `shown < total` means
  the budget cut the rest, *not* that nothing else matters — raise `budget` to see more.
- **`ambiguous` on `locate`** — the budget cut through a run of equally-ranked
  candidates; the tail order is arbitrary, so widen the task words or the budget rather
  than trusting rank N.

## Honest limits

- **Staleness is not detected.** After any edit the graph is whatever `index`/`reindex`
  last wrote. `reindex` before trusting a post-edit answer.
- **Call resolution depth varies by language.** Usable for TypeScript and Elixir;
  thinner for Rust, Go, GraphQL. Thin is not empty — file-level signals still work, but
  function-level `impact` will be shallower. `ripple lsp doctor` shows what is indexed.
- **Under-links rather than invents.** Dynamic dispatch, `dataloader(...)`, inline
  resolvers, and inline GraphQL fragments produce no edge. A missing edge is not proof
  of no dependency — fall back to `search` + reading when a seed's `touches` look thin.
- **A bare name is often ambiguous.** `impact run` may hit several `run`s across
  languages and seed them all; the answer says so. Use `search`/`locate` to pin the one
  you mean, or `--in-file <substr>` (CLI) to narrow.

## Example

Task: "add a per-IP rate limit to the login endpoint."

```
locate "rate limit login endpoint"
  → seed: guard  crates/api/auth.ex  [route:login, doc:limit]  touches: login_controller, router
  → seed: sign_in  web/src/auth/api.ts  [name:login]           touches: LoginForm
impact guard --budget 10        # everything that depends on the handler you'll change
# …edit guard + add the missing test the review flags…
reindex
review                          # is the change ranked where the danger is? untested?
```

One `locate` told you the change spans both the Elixir handler and the TS caller — a
diff-local reviewer could not have seen that.
