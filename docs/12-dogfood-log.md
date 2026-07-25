# 12 — Dogfood log

Gaps found by *using* ripple rather than reasoning about it. Each entry: what was
asked, what ripple said, what was true, what it implies. Newest first.

The point: every entry below turned into a committed fix, and none of them were on
the roadmap beforehand. Using the tool finds different bugs than reading it.

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
