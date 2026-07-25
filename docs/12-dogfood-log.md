# 12 — Dogfood log

Gaps found by *using* ripple rather than reasoning about it. Each entry: what was
asked, what ripple said, what was true, what it implies. Newest first.

The point: every entry below turned into a committed fix, and none of them were on
the roadmap beforehand. Using the tool finds different bugs than reading it.

---

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
