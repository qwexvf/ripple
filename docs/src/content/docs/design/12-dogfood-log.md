---
title: "12 — Dogfood log"
description: "What ripple got wrong when used for real, and what each mistake turned into"
sidebar:
  label: "12 — Dogfood log"
  order: 12
---
Gaps found by *using* ripple rather than reasoning about it. Each entry: what was
asked, what ripple said, what was true, what it implies. Newest first.

The point: every entry below turned into a committed fix, and none of them were on
the roadmap beforehand. Using the tool finds different bugs than reading it.

---

## 2026-07-30 — an agent with the raw diff beat ripple at ripple's own job

**Asked:** review the `v0.1.2..v0.2.0` diff (15 files, +542) two ways — `ripple review`,
and an independent agent given the same diff and no tool — then compare.

**Said:** the two agreed at file granularity (`crates/cli/src/verify.rs` was 6 of
ripple's 15 rows and 2 of the agent's top 3) and disagreed at symbol granularity, which
is the granularity `review` sells. The agent's first pick, `reference_file` — the
release's largest new function — was ripple's 11th. Ripple's first pick, `registry`, was
one line: `Box::new(gleam::Adapter::new()),`.

**True:** both, in a way that says what ripple is for. `registry` really does have 46
dependents and the agent could not have known that from the diff. `reference_file` really
was the code most likely to be wrong, and ripple ranked it low because *every* term it
scores with — dependents, churn, bug-density, ownership — is backwards-looking, so code
the diff adds has no history and scores at the floor.

**Implies:** four bugs, all now fixed, and one conclusion.

- 22 of 37 changed symbols were never printed: `--budget 15` truncated silently ([#41](https://github.com/qwexvf/ripple/issues/41)).
- `untested` was true on all 37 rows, including on the test functions themselves, because nothing in the workspace ever constructed an `EdgeKind::Tests` ([#36](https://github.com/qwexvf/ripple/issues/36)). Now 28 of 41, and a repo where tests can't be seen says so instead of flagging everything.
- Test functions counted as dependents, so a well-tested symbol ranked riskier than an untested one ([#42](https://github.com/qwexvf/ripple/issues/42)).
- Ranking rewritten: reach is logarithmic and the diff itself is a term. Same diff: `reference_file` 11 → 2, `start` 12 → 8, `registry` 1 → 6.

The conclusion is the uncomfortable one. On a diff an agent can read whole, ripple's
ranking is a worse version of what the agent already does. What ripple had that the agent
could not get — the 46 dependents, and the co-change warning naming three files that
always move with `registry` and didn't this time — is information from *outside* the
diff. That is the product, and it only pays when the repository is bigger than the
context window, or when the answer crosses a service or a repository boundary.

---

## 2026-07-26 — the differentiator emits nothing on the two repos it was built for

**Asked:** index the two real polyglot repos on this machine and look at the
cross-service edges — the thing no other tool does.

**Said:** `aegis` (848 files, 21 `.graphql`) → `0 graphql, 0 db`. `poker-platform`
(213 files, 44 `.graphql`) → `0 graphql, 0 db`.

**Was true:** both have real operation documents (`query PackageGraph { packageGraph(…) }`)
and real codegen output, and ripple finds the *consumer* side fine. The producer side
is **Gleam**, which has no adapter, so there is nothing to join to. Counting across
`~/projects`: **699 `.gleam` files in 9 repositories**, including `services/api` in both
of these. The 8 graphql edges in ripple's own index all come from its Elixir fixtures.

**Implication (issue #40).** Every quality number for cross-service resolution to date
comes from one language pair on one corpus. `tree-sitter-gleam` is on crates.io, already
ships a `tags.scm`, and `gleam lsp` exists — so Tier 0 plus `index --calls lsp` is the
same 4-touchpoint shape phase 4 just measured on Go.

**Lesson:** "works" measured on the corpus it was written against says nothing about the
corpus you actually own. Cross-service is the headline claim and it had never been run
outside the fixtures.

---

## 2026-07-26 — a language server the repo chooses, and ripple runs it

**Asked:** are language servers usable in this repo? (`ripple lsp doctor`)

**Said:**
```
  go (/bin/touch)
    broken   server exited during initialize
```

**Was true:** `.ripple/lsp.json` is read from the repository *under analysis*, and its
`command`/`args` are spawned. A repo that commits one gets arbitrary command execution
with the user's privileges; `doctor` then prints the injected binary as if it were a
diagnostic. Confirmed by pointing it at `/bin/touch` and watching the file appear. Every
LSP path is affected — `doctor`, `--verify lsp`, `index --calls lsp`.

**Implication (issue #34).** In-repo config has to become untrusted data: read the table
from user config outside the tree, or allowlist known server binaries, or require an
explicit per-root trust step.

**Lesson:** ripple's whole purpose is reading code somebody else wrote. Anything it loads
from the target tree is attacker input, config included — and the MCP server makes an
agent the one who pulls the trigger.

---

## 2026-07-26 — every symbol is untested, because nothing can be tested

**Asked:** review a one-line edit to `src/utils/url.ts` in honojs/hono.

**Said:**
```
  6.66  getPath (src/utils/url.ts)  — high churn (0.86), 4 downstream, untested

⚠ expected co-changes absent (usually changed together):
  src/utils/url.test.ts
```

**Was true:** `src/utils/url.test.ts` imports `getPath` and has a `describe('getPath')`
block. One line called it untested while the next named the file that tests it. The flag
checks for an `EdgeKind::Tests` edge and **nothing in the codebase ever emits one**, so it
is true for every symbol in every repo — 15 of 15 rows on this repo.

**Implication (issue #36).** Either produce the edge (a call from a file matching the
language's test convention into a symbol outside it — cheap at Tier 2, and a better
"you changed this and its test didn't move" signal than co-change) or stop printing it.

**Lesson:** a flag that never varies is not a weak signal, it is a decoration. Grep for
the producer before trusting any boolean in the output.

---

## 2026-07-26 — one name, six symbols, three languages, no warning

**Asked:** `ripple impact run` on ripple's own repo, wanting `verify::run`.

**Said:** `blast radius of run — 1 of 1 hits`, the one hit being `boot` in a
TypeScript test fixture.

**Was true:** six symbols are named `run` — one Rust function and five fixtures across
TS, Elixir and Rust — and all six were seeded. `lookup_or_bail` announces the match rule
only when it had to *widen* past exact, so the ambiguous-exact case, which is the
dangerous one, says nothing. The Rust function the query meant has no callers in the
graph at all, because Rust `use` paths are unresolved.

**Implication (issue #37).** Say how many symbols a name seeded, and accept `--in-file`
on `impact` the way `neighbors` already does. Separately: test fixtures are indexed as
production code with no default exclusion, so ripple's own graph is mostly fixtures.

**Lesson:** the honest-uncertainty work went into the *edges*. The seed set has the same
problem and none of the same reporting.

---

## 2026-07-25 — the reverted fix was right; the metric that rejected it was wrong

**Asked:** re-do issue #18 (calls outside any function are dropped), which had been
tried, measured, and reverted: +1,634 edges for −6.7 points of oracle agreement.

**Said (then):** 87.9% → 81.2% identical caller sets, 64 ripple-only edges, and none
of the 19 known misses fixed. A fair reading at the time: coarser edges, no gain.

**Was true:** the oracle was comparing function-granular names against file-granular
edges, so every correct-but-coarser edge scored as a false positive, and the misses it
"failed to fix" were file-level linkage it had no way to express. With the oracle
attributing by position and stating its granularity, the same fix measures:

| | function granularity | file granularity |
|---|---|---|
| before #18 | 164/165 (99.4%), 1 ripple-only, 0 server-only | 146/165, 0 ripple-only, **19 server-only** |
| after #18 | **165/165 (100%)**, 0, 0 | 153/165, 44 ripple-only, **0 server-only** |

The function row got *better* (the one remaining false positive was itself a
file-granular edge being mis-scored), and every file-level miss closed. The 44 new
file-granularity ripple-only edges were sampled: dexter reports one caller for
`filter_posts` and misses the five real call sites in `lfg_posts_test.exs`, so they
are true edges the oracle cannot see.

**Implication (fixed).** Cross-service linking falls back the way same-file
resolution always did — enclosing definition, else module body, else file — and the
index reports the coarser edges as their own count (`1634 file-granular`) rather than
blending them in. Held-out recall did not move (7.1% static / 10.5% fused): these
edges link tests and module bodies, not the co-changing pairs `eval` samples.

**Lesson:** a fix rejected by a measurement is only as rejected as the measurement is
sound. This one was reverted for six months' worth of the wrong number, and the
reverted diff was closer to right than the metric that killed it.

---

## 2026-07-25 — 87.9% was three different measurements added together

**Asked:** build the piece issue #18 says has to come first — make
`eval --oracle lsp` compare at a stated granularity — then re-run it.

**Said:** the first attempt made agreement *worse*, 87.9% → 62.4%, with 348 call
sites reported as "credited to a function that doesn't contain them" and callers
named `FiveNoobs.Lfgs.LfgPosts` — a module, not a function.

**Was true:** three separate faults, each hidden by the previous number.

1. **Attribution has to be by position.** `incomingCalls` carries `fromRanges`, and
   the caller the server *names* can be a function that does not contain the call.
   Comparing names measured naming conventions; comparing positions measures edges.
2. **A module node spans its whole file**, so "innermost containing node" absorbed
   every call that sits outside a function and called it a module-level call. Only
   callables may contain a call.
3. **The graph had lost every definition site but one.** Identity is
   (path, qualified name), so `def kind(:admin)` and `def kind(_other)` share an id —
   and the store's id-keyed table kept whichever was written last. So a call in the
   second clause looked like it was inside no function at all. This is not an Elixir
   quirk: overloads and reopened classes do the same thing.

**Implication (fixed).** `ir::Node::extra_spans` keeps every definition site,
`resolve` collapses duplicates into one node instead of letting the store do it by
accident, and cross-service linking uses all the spans. With attribution by position
on top:

| granularity | identical caller sets | ripple-only | server-only |
|---|---|---|---|
| function | **164/165 (99.4%)** | 1 | 0 |
| file | 146/165 (88.5%) | 0 | **19** |

and 153 call sites inside no indexed symbol, reported as their own bucket. The two
rows are now issue #18's acceptance test. `impact --verify lsp` additions went
**145 → 0**: every one had been dexter crediting a test-block call to the preceding
`defp`, and yesterday's "145 added" line in this log was wrong about them being
useful.

**Lesson:** the headline 87.9% was summing agreement about *edges*, agreement about
*naming*, and our own missing spans. Splitting them moved one number up and produced
a second number worth fixing — which is what a measurement is for.

---

## 2026-07-25 — the oracle's answers are wrong in both directions

**Asked:** `impact changeset --root <web> --verify lsp` — the first run of on-demand
LSP verification (docs/11 phase 3) against dexter 0.7.1 on 5noobs.

**Said:** `132 files checked, 1815 confirmed, 172 added, 42 contradicted`. Following
docs/11 as written, the 42 would have had their confidence floored and the 172 added
at 1.0.

**Was true:** three separate problems, all found by reading five examples instead of
trusting the counts.

1. **Ours.** Ripple collapses a multi-clause Elixir function into one symbol; dexter
   returns one `documentSymbol` per clause. Reconciling clause-by-clause made the
   callers of one clause look like a denial of the others. Unioning the server's
   answers per name first: 42 → 28 contradictions, 172 → 145 additions.
2. **The server's, on denial.** All 5 remaining contradictions were dexter misses.
   `players.ex:player_in_discord?/1` does call `get_player` (line 1346, second
   clause) — dexter's caller list omits it while including callers from other files.
   Flooring on that would have degraded true edges.
3. **The server's, on addition.** All 5 sampled additions claimed
   `direct_messages_test.exs:create_player` called functions the *test bodies* call.
   Probing dexter directly confirmed it attributes a call inside an ExUnit `test`
   block to the preceding `defp`.

**Implication (fixed).** Answers are unioned per name before any verdict.
Contradictions are counted and printed with examples but **change nothing** unless
`--floor-contradicted`/`--drop-contradicted` is passed. Server-only edges land at
**0.7**, not 1.0 — one unconfirmed extractor is weaker evidence than two agreeing
ones, and invariant 5 forbids stating a guess as fact. The report also prints the
example pairs for both, so the next person can check rather than believe.

**Lesson:** "the language server is the accuracy tier" was an assumption, not a
measurement. It is more accurate *on average* and wrong in specific, systematic ways
— which is exactly why provenance (`EdgeSource`) had to be a stored field rather
than a footnote.

---

## 2026-07-25 — the headline number was 34 points of leakage

**Asked:** `ripple eval --commits 300 --root <web>` — the project's headline recall
figure, quoted as static 6.5% / co-change 35.4% / fused 40.4%.

**Said:** the same, for months. Nothing looked wrong: the static baseline was known
to be leakage-free and the co-change lift was the doc-02 differentiator, "measured".

**Was true:** co-change was scored on the commits its own `ChangesWith` edges were
mined from. `eval` read the edges out of the persisted graph, and the graph is mined
from the newest 3000 commits — which contains the entire 300-commit test window. With
mining restricted to commits older than the test window: **co-change 3.4%, fused
10.5%** (50 test / 500 training commits). The lift is ~3 points, not ~34.

**Implication (fixed).** `overlay::holdout` splits history at the k-th newest
eligible commit and mines only the older side; `eval` scores against that instead of
the graph. It also prints the trained-pair count, which immediately showed the second
trap: at `--commits 300` only 148 training commits remain and they yield **15** usable
pairs, so the 1.3% there is a starved training set, not a worse model. `docs/11`'s
"34 of ripple's 40 recall points come from evolution" was load-bearing for the LSP
plan and is now wrong in the other direction — call-graph coverage is the binding
constraint.

**Lesson:** an evaluation that reads its predictions out of the artifact it is
evaluating will always flatter it. The one number nobody re-derived was the one
number that mattered.

---

## 2026-07-25 — rust path calls: 0 hits → 4, after two wrong turns

**Asked:** `impact link_cross_service --root .` reported 0 hits (see the entry
below). Fix it so ripple can be used on itself.

**Result:** `impact` now returns exactly the four real callers — `index_project` in
the CLI and three tests — and ripple's own graph goes **380 → 607 edges**. The 5noobs
stack is byte-identical at 14,153 edges, which is the control: TS member calls and
Elixir remote calls don't go through this path.

Resolution now honours a qualifier: `Client::new` prefers a `new` defined on
`Client`; a lowercase qualifier (`resolve::link_cross_service`) is a module path
where the bare name is the only handle; a capitalized qualifier that matches no
owner resolves to **nothing**.

**Two wrong turns, both caught by measuring rather than by tests:**

1. First version had no rule for unknown types, so `HashMap::new()` and `Vec::new()`
   fell back to the bare name and linked to an unrelated `Adapter::new`. Ripple
   happens to define exactly four `new`s, so my "at most 4 candidates" cap let every
   one of them through — edges jumped to 1,299, of which **769 were false**. Visible
   only by asking `neighbors new --in` and seeing `roots_by_scope` calling an Elixir
   adapter's constructor.
2. Then `Client::new()` still resolved to the *calling file's* `Local::new`, because
   same-file names were consulted before the qualifier. An explicit qualifier has to
   decide first — including deciding that nothing matches.

Also fixed: `last_segment("Client::")` returned empty, so no owner was ever indexed.

**Lesson:** the false positives were invisible in aggregate (1,299 edges looks like
success) and obvious in one spot check. "Did the number go up" is not the same
question as "is the number right".

## 2026-07-25 — elixir `import`, and a negative result on attribution

**Asked:** the oracle said 20 of ripple's misses looked import-shaped. Fix `import`
and see if the number moves.

**Result: 87.3% → 87.9%** (145/165), +31 edges on 5noobs. `import Mod` is now a fact,
and a bare call that no local definition explains resolves against the imported
modules' functions (candidates split confidence; a local definition always wins).
Real but small — `import` was not the dominant class after all.

**Then a negative result worth keeping.** Digging into a remaining miss showed
`TeamContact.changeset(...)` sitting inside an ExUnit `test` block — a macro, not a
`def` — so `enclosing()` found no function and the cross-service loops *dropped the
edge*, while same-file resolution falls back to the module node for the same
situation. Attributing those to the file instead looked obviously right:

| | before | after attribution change |
|---|---|---|
| edges | 14,153 | 15,787 (+1,634) |
| identical caller sets | 87.9% | **81.2%** |
| ripple-only edges | 1 | **64** |
| server-only edges | 19 | 19 (**unchanged**) |

So it added 1,634 true-but-coarser edges, cost 6.7 points of measured agreement, and
fixed **none** of the misses it was meant to fix — my prediction was simply wrong.
Reverted. The edges are not false: that file really does call that function. They are
file-granular where dexter is function-granular, and the oracle cannot express that
difference, so it scores them as errors.

**Implication (open).** Two separable things: calls outside any function are dropped
by cross-service linking (a real coverage gap, measured at ~1.6k edges on this repo),
and the oracle can only compare at one granularity. Fixing the first honestly needs
the second, or it trades a measurable number for an unmeasurable one.

**Lesson:** having the metric before the change is what turned "obviously right" into
"measurably not yet". Without the oracle this would have shipped as an improvement.

## 2026-07-25 — the oracle's first number: 87.3%, and two real bugs behind it

**Asked:** `eval --oracle lsp --sample 40` against dexter on the Elixir backend —
the first precision measurement the tree-sitter call graph has ever had.

**Said (after three false starts, all mine):** 40 files, 165 symbols judged,
**144/165 (87.3%) identical caller sets**, 1 ripple-only edge, 20 server-only, 98
self-recursive edges excluded, 53 symbols dexter couldn't resolve.

The three false starts are the finding as much as the number is:
1. **0 judged, 46 unknown** — one counter for "server doesn't know it" and "ripple
   doesn't know it" made the failure undiagnosable. Splitting them showed dexter
   names functions `changeset/2`, with the arity ripple doesn't distinguish.
2. **0/5 agreement, every edge in both columns** — dexter returns call-hierarchy
   callers fully qualified (`FiveNoobs.Players.PlayerReport.changeset`) where ripple
   stores `changeset`. Both sides had found the identical call; only the spelling
   differed. A comparison tool that hasn't proven it can *agree* is measuring nothing.
3. **73.3% → 86.7%** once self-recursion was excluded: dexter reports `X → X`, ripple
   drops it by design. 98 of the disagreements were that one documented choice.

**Two ripple bugs it then exposed:**

- **`alias A.B.C, as: X` was ignored.** Aliases were keyed by last segment, so every
  call through a renamed alias was unresolvable and its edges silently missing.
  Fixed; 87.3% and +5 edges on this repo.
- **Elixir `import` is not handled at all** (open). `import FiveNoobs.PlayersFixtures`
  then a bare `player_fixture(...)` is a cross-module call with no qualifier, and
  ripple resolves unqualified calls only against same-file definitions. This is the
  dominant remaining class — test, fixture and Phoenix code lean on `import` heavily.
  Needs `import` recorded as a fact and unqualified calls resolved against the
  imported modules' exports.

**Lesson:** most of the work in building an oracle is proving the two sides are
talking about the same thing. Every one of the three false starts *looked* like a
ripple defect and was a comparison defect — if I'd trusted the first run, I'd have
"discovered" that ripple's call graph was worthless.

## 2026-07-25 — the risk score is git-only, and capped on single-author repos

**Asked:** step 5 of the phase ritual — `risk crates/resolve/src/lib.rs`, the file I
changed most today. Does the number match where the danger actually was?

**Said:** `composite 0.71 | churn 0.95 bug 0.83 ownership 0.00`.

**Was true:** churn and bug-density are believable (that file took most of today's
edits, several commits worded as fixes). Two things behind the number are not:

1. **`ownership` is always 0 on a single-author repo.** It's
   `percentile(1 / author_count)`, and percentile is `count(x < v) / n` — so when
   every file ties, every file scores 0. Since `W_OWN = 0.2`, composite is capped at
   **0.8** and uniformly compressed. Risk scores are only comparable *within* one
   repo's file set, and any degenerate signal silently drags the composite down
   instead of being excluded from the blend.
2. **`complexity`, `fanout` and `test_proximity` are never populated by anything.**
   `composite = 0.4*churn + 0.4*bug + 0.2*ownership` — purely git. The v2 plan said
   risk would score `complexity + fanout + test_proximity` with git terms absent
   until v3; the reality is the exact inverse. So a heavily-depended-on function with
   a calm git history reads as low risk, which is precisely the case the static graph
   was supposed to catch.

**Implication (fixed, `fdf8e2b`).** Both: `score_structure` counts distinct
dependents per symbol after cross-service linking and percentile-ranks them into
`fanout`, and the blend now drops terms that rank nothing and renormalizes, judging
variance per corpus rather than per node. `complexity` and `test_proximity` drop out
by the same rule instead of posing as measured zeros. On ripple itself the file
changed most that day went 0.71 → 0.93, and `SymbolId` — no bug history, many
dependents — went from invisible to 0.42. The weights are still hand-set and unfit to
data.

**Lesson:** the ritual's "does this number make sense?" step found in one query that
the flagship risk formula is missing three of its six inputs. Nothing else today
would have surfaced that — the tests all pass, because they only assert the terms
that are wired.

## 2026-07-25 — `impact` on ripple's own code returns nothing

**Asked:** `impact link_cross_service --root .` on the ripple repo, right after
adding the Rust adapter so ripple could index itself.

**Said:** `blast radius of link_cross_service — 0 hits`.

**Was true:** it is called from `crates/cli/src/main.rs` as
`resolve::link_cross_service(&indexed.files, &nodes)`. The ref *is* extracted —
`refs.scm` captures the last segment of a path call — but call resolution only
consults same-file definitions and resolved imports, and Rust `use`/path resolution
does not exist. In Rust nearly every interesting call crosses a module or crate, so
`--in`/`impact` are close to useless there. (`neighbors resolve_calls --in` was
right, because that call happens to be in the same file.)

**Implication (open).** Two ways out, and dogfooding makes the trade concrete:
- resolve path calls by *qualified* name: `Client::new` should match a definition
  whose qualified name is `Client::new`, and `resolve::link_cross_service` should
  match `link_cross_service` exported from the `resolve` crate. Precise because the
  path prefix disambiguates — unlike a bare last-segment lookup, which would link
  every `new()` in the graph to every `new`. Needs a qualified-name index in
  `DefIndex` (today's `methods_by_class` splits on `.`, so `::` names never land in
  it).
- or let the LSP tier answer it: `rust-analyzer` resolves this correctly and for
  free, at the cost of a server that builds its cache first.

**Lesson:** the "add a language = one folder" claim holds for *symbols* and stops at
Tier 2. A new language's `tags.scm` costs an hour; making its call graph useful is
where the work is — which is the argument for the LSP tier, discovered by using the
thing rather than by reasoning about it.

## 2026-07-25 — three quarters of the graph was other people's code

**Asked:** nothing — noticed that `dexter init` reported 3,194 files for the Elixir
umbrella while ripple reported 3,353 for *both* repos, which didn't add up.

**Was true:** the ignore list covered `node_modules` and JS build dirs but not
`deps/`, `_build/`, `vendor/`, `target/`, `.venv/`. On 5noobs that meant 2,176
dependency source files against the project's own 762.

**Implication (fixed).** Files 3,353 → 1,153, nodes 44,581 → 10,099, edges 38,526 →
14,117, cold index 3.4s → 1.8s. Also corrected a claim: Elixir local-call resolution
adds 2,852 edges on real code, not the 20,450 first measured. `GraphqlCall` stayed at
343, so cross-service resolution had never matched library code — and `eval` recall
was unaffected, since it only ever paired git-tracked files.

**Lesson:** comparing against another tool's numbers is cheap and finds things no
test would. Neither tool had to be *right* for the discrepancy to be informative.

---

## 2026-07-25 — `eval` reported 0.0% recall and called it a result

**Asked:** `ripple eval --commits 300 --root <web>` on a two-root index.

**Said:** `0 same-commit file pairs`, `0.0%` across the board, exit 0.

**Was true:** 4,188 pairs, 6.5% static / 40.4% fused. `eval` looked up raw git paths
with an exact `SymbolId::module()`, but a multi-root index namespaces module paths by
root tag, so every lookup missed.

**Implication (fixed).** The store now persists the `(tag, path)` roots it was built
from, `eval` namespaces paths the same way indexing did, and zero pairs now says so
instead of printing 0.0%. `review`/`risk` had escaped the bug only because
`nodes_in_file` happens to match on a path suffix.

**Lesson:** a silent zero is worse than a crash, and it sat on the one command that
measures quality.

---

## 2026-07-25 — a one-line method lost its call

**Asked:** does the Elixir definition-header guard affect other languages?

**Said:** `class A { foo() { const b = new B(); return b.foo(); } }` → zero edges.
Split across lines, the same code gave the correct `A.foo → B.foo` at 0.85.

**Was true:** the guard dropped any ref whose name matched the enclosing definition
on its first line. That holds for unqualified calls in languages where a definition
*is* a call (Elixir `def f(x)`); applied to member calls it deleted real edges.

**Implication (fixed).** Guard restricted to `RefKind::Call`, TS fixture added. An
Elixir-motivated change had silently damaged the TypeScript path.

**Lesson:** a language-specific fix needs a test in the *other* languages it touches.

---

## 2026-07-25 — the index summary lied about which edges it counted

**Asked:** how many GraphQL edges does the refactor produce?

**Said:** `1218 graphql`.

**Was true:** 343 GraphQL edges and 875 DB edges. The summary printed
`graphql + db` under the label "graphql", which sent me hunting a nonexistent
regression in cross-service resolution.

**Implication (fixed).** The two are reported separately.

**Lesson:** a mislabelled number costs more than a missing one.

---

## 2026-07-31 — an A/B test on escaped defects, and what it actually measured

**Asked:** does handing a review agent ripple's output make it find more real defects?

**Said (by the experiment):** the ripple arm found the escaped defect in 1 of 6 cases,
the control in 0 of 6. That is not evidence. It is one case.

**How the corpus was built,** because the previous attempt at this question was
worthless: a bug is only ground truth if a human later had to fix it. Mine every
focused `fix` commit in 5noobs, blame the lines it removed to find the commit that
wrote them, keep the case when blame converges on one commit and the bug escaped for
at least a week. Six cases survived. Each one gets a worktree checked out **at the
introducing commit**, indexed there, with its `.git` pointer removed so no reviewer can
read forward into the fix. Both arms got the identical prompt; the treatment arm also
got `ripple review` output and the ability to query the index. A blind grader scored
the twelve reviews with the arm labels shuffled.

**Was true, and more interesting than the headline:**

- Both arms found *plenty* of real bugs — a schema pointing at table `notificaitons`,
  a `Repo.insert!` whose `{:ok, _}` branch can never match, a stray `MyAppWeb.Endpoint`.
  The reviewer is competent. It just does not converge on the one defect that later
  broke production, with or without ripple.
- **Ranking does not cause finding.** In C4 ripple ranked the defective symbol
  `paginate` **first of seventeen**, the agent read it, and still did not notice the
  missing `order_by`. Putting the right symbol at the top is necessary and nowhere near
  sufficient.
- The escaped defects are hard *because* they escaped. A bug that survived human review
  is selected for being unobvious.

**Implication.** The honest reading is that this harness measures the reviewer more
than it measures ripple, and six cases cannot separate them. What the same corpus did
measure cleanly, without any agent in the loop, is ripple's own ordering: the defective
symbol ranked 1, 3, 4, 4, 13, 21 out of 17, 8, 17, 9, 24, 32 — mean normalized rank
0.385 against 0.5 for chance. Compare `eval --risk` on the same repo, which scores
fanout at 2.68x lift. Ripple is good at *which files are risky* and weak at *which
symbol in this change is the dangerous one*. Filed as #55; the two worst ranks have a
single diagnosed cause, filed as #54.

**Lesson:** an A/B test whose control also fails tells you about the metric, not the
treatment. Measure the component you control before measuring the pipeline it sits in.
