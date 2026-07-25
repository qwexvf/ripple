# 12 — Dogfood log

Gaps found by *using* ripple rather than reasoning about it. Each entry: what was
asked, what ripple said, what was true, what it implies. Newest first.

The point: every entry below turned into a committed fix, and none of them were on
the roadmap beforehand. Using the tool finds different bugs than reading it.

---

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

**Implication (open).** Either renormalize the blend over the terms that actually
carry signal, or populate `fanout` at query time (the graph already knows it) and
`complexity` at parse time. Until then the docs overstate what risk means, and the
weights have never been fit to anything.

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
