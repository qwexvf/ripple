---
title: "MCP reference"
description: "Wire ripple into Claude Code, Cursor, or any MCP client — the eight tools and the contracts they honour"
sidebar:
  label: "MCP"
  order: 2
---

`ripple mcp` speaks MCP over stdio. It reports itself as `ripple`, and **indexes on first
use** if `.ripple/graph.redb` is missing — so you can point an agent at a repository that
has never been indexed and the first tool call still works.

## Setup

Claude Code:

```bash
claude mcp add ripple -- /path/to/ripple mcp --root /path/to/your/repo
```

Or by config, for any client that reads the standard shape:

```json
{
  "mcpServers": {
    "ripple": {
      "command": "/path/to/ripple",
      "args": ["mcp", "--root", "/path/to/your/repo"]
    }
  }
}
```

`--root` is where the graph lives. To trace across repositories, index them together
first (`ripple index repo-a repo-b`) and point `--root` at the root that holds the graph.

## Tools

| Tool | Required | Optional | What it answers |
|---|---|---|---|
| `locate` | `task` | `budget` (10) | "Where do I start for this task?" Plain-words task in, ranked starting symbols across every repo out — each with why it matched and a blast-radius preview. **Call this first for an implement/feature task** |
| `search` | `query` | `limit` (30) | Find symbols and files by substring. **Call this first to disambiguate a known name** — the other tools want exact names |
| `impact` | `symbol` | `budget` (20) | Risk-ranked blast radius across languages and services |
| `review_focus` | — | `base`, `budget` | Rank the symbols in a diff by review priority. No `base` means working tree vs `HEAD` |
| `neighbors` | `symbol` | `direction` (`in`/`out`), `depth`, `limit` (50) | One hop of callers/importers or callees/imports |
| `risk` | `target` | — | Churn, bug-density, ownership, and the composite for a symbol or file |
| `path` | `from`, `to` | `depth` (6), `limit` (3) | How does A reach B? Shortest routes first, with the product of edge confidences |
| `explain_edge` | `from`, `to` | — | Why are these two connected? Edge kind, confidence, provenance, and site |
| `reindex` | — | — | Rebuild from current source. Call this after the agent edits code |

There is no `search` subcommand on the CLI — it exists only here, because picking the
right symbol out of several same-named candidates is an agent problem.

## Why `locate` first for a task

An agent handed "implement rate limiting on login" has no symbol yet, and the task's
words rarely appear in a function name. `locate` maps the task to code: it matches the
words against symbol names, module paths, endpoint routes, and doc comments, then fuses
that lexical recall with graph centrality and risk (Reciprocal Rank Fusion) so a word
landing on a central, risky symbol outranks a bare substring hit. Each returned seed
carries its `repo`, the fields that matched (`why`), its dependent count, and a one-hop
blast-radius preview (`touches`) — so one call answers "start here, and this is what it
touches" instead of a search followed by a read of every candidate and a separate
`impact`. On a cross-repo index the seeds span both repos in one ranked list.

## Why `search` for a known name

On a real repository a bare name is usually ambiguous. `search` returns each candidate
with its kind, qualified name, and module, so the agent picks deliberately instead of
guessing at the first hit and editing the wrong `getPath`. Use it once you already know
the name; use `locate` when you only know the task.

## Contracts worth relying on

**Truncation is declared.** Every budgeted result reports `{shown, total}`. Nothing is cut
silently, so an agent can tell "these are the 20 that mattered" from "these are all of
them".

**Confidence is on every edge.** `1.0` means extracted from source. Lower means inferred,
and where resolution is ambiguous ripple splits confidence across candidates rather than
emitting one fabricated edge. `explain_edge` also returns provenance — `Extracted`,
`LspVerified`, or `CoChange` — so a statistical association is never mistaken for a call.

**Output is deterministic.** Rankings sort by weight, then risk, then symbol id — a total
order, so two runs over the same graph agree and a diff of two outputs means something.

**Staleness is not detected.** The graph is whatever `index` last wrote. After editing
code, call `reindex`; nothing will warn you otherwise.

## What it will not tell you

`impact` is only as good as the resolution underneath it. Call resolution is at a usable
level for TypeScript and Elixir; Rust and GraphQL are shallower. If a language is barely
supported, the file-granular git signals still work — but the function-level answers will
be thin, and thin is not the same as empty. Run `ripple lsp doctor` to see what is indexed
and what could be sharpened.
