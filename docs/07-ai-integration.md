# 07 — AI integration: optimizing decisions, not tokens

The 2025–2026 code-graph MCP servers converged on one metric: **fewer tokens**. That was the right first fight (indexed retrieval beat grep). But token compression is table stakes now. The next axis is **decision quality** — does the thing you hand the model actually change what it does?

## The shift: what gets ranked

Every existing server ranks by *structural proximity* or *semantic similarity* and then compresses. ripple ranks by **risk** and then compresses. Same token budget, different payload:

| | ranked/compressed thing | what the LLM does with it |
|---|---|---|
| retrieval MCPs (claude-context, Repomix) | most-similar chunks | reads relevant code |
| graph MCPs (code-graph-mcp, codebase-memory) | nearest dependents | traces structure |
| **ripple** | **riskiest impacted nodes / hunks, with reasons** | **knows where to look and why** |

For a fixed budget, ripple's list is the one that changes the model's next action.

## How an LLM reviewer uses it

```
1. agent gets a PR
2. agent → review_focus(pr, budget)
3. server returns 5 ranked hunks + missing_cochange smells + low_risk skip-list
4. agent spends its attention top-down: deep-reads hunk #1 (highest review_priority),
   pulls neighbors() / risk() to drill in, skims the low_risk list
5. agent flags the missing test that co-change predicted but the PR omitted
```

Without ripple the agent either reads the whole diff uniformly (wastes budget on `docs/*` and `format.ts`) or guesses which parts matter. With it, the *ordering itself* is the value — and it's grounded in git history the model can't see from the diff alone.

## How a coding agent uses it

```
before editing verifyToken:  impact("verifyToken", budget)
→ "requireAuth (direct caller, untested), config/auth.yaml (co-changes 7/10)"
→ agent updates the caller, checks the config, adds the missing test — proactively,
  because it saw the blast radius before making the change, not after CI failed.
```

This is the difference between an agent that reacts to broken tests and one that avoids breaking them.

## Why the git signal matters *specifically* for AI

An LLM reading a diff sees the code but **not the repo's history**. It cannot know that `charge.ts` and `charge.test.ts` always move together, or that `token.ts` has a 40% bug-fix rate, or that this author has never touched this file. Those are exactly the signals a good human reviewer carries in their head. ripple externalizes them into the context window — **it gives the model the senior reviewer's memory**, at file granularity, for any language (Tier 0).

## Uncertainty and provenance as first-class output

Following the AI-native-protocol idea from the start of this project — outputs carry more than values:

- **confidence** on every edge (`EXTRACTED` vs `INFERRED`), so the agent can discount shaky call resolution.
- **provenance** on every risk claim (`why: "changed 14× last quarter"`, `via: ChangesWith, coupling_score 0.7`), so the agent can explain its reasoning to a human and a human can audit it.
- **explicit truncation** (`{shown, total, reason}`), so the agent never mistakes a budget cut for "nothing else matters."

This is what "AI-native" should mean concretely: not a chat wrapper, but **semantics + uncertainty + cost + reasons** in the payload — the protocol-design thread that opened this whole discussion.

## Synergy with qa-agent (independent, but complementary)

ripple is a standalone project, but it plugs cleanly into an AI QA pipeline like qa-agent:

- **Test planning:** qa-agent's planner can ask `impact(diff)` to focus generated E2E scenarios on the actually-affected surface instead of the whole app.
- **Incident reproduction:** map a stack trace's symbols through the graph to the code paths and their co-changed neighbors — narrowing where the regression was introduced.
- **PR-check gating:** `review_focus(pr)` feeds qa-agent's GitHub PR-check with "which areas need the most test coverage this PR," turning a pass/fail gate into a targeted one.

The boundary stays clean: ripple emits a graph + risk + queries over MCP; qa-agent consumes them. Neither owns the other.

## New capabilities this unlocks

- **Risk-ranked auto-review** that explains itself and points to untested blast radius.
- **Proactive blast-radius-aware editing** by coding agents.
- **"Missing co-change" detection** as a queryable bug smell.
- **Same-day support for any language** an agent throws at it (Tier 0 + git overlay).
- **Cross-service impact** (a frontend change's blast radius reaching a backend handler over `HttpCall`), which no diff-local reviewer — human or LLM — can see.
