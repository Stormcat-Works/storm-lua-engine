# Verification — Cycle 66: generative differential campaign (raw Lua execution)

Date: 2026-11-01. Worker: w9 (correctness). Base: integration tip
`c9c41fe` (Round 255). Method: c65's `rawexec` (project compiler +
raw mlua Lua 5.3 executor, no dialect rewriting) driven over a
grammar-based random program generator, plus ddmin minimization and
per-pass toggle attribution for every real divergence.

## Campaign

- Generator: `gen-c66.py` (deterministic; program *i* uses seed
  `6601 * 1000003 + i` for the full run), biased toward multi-target
  assignment, tail calls, multi-value expressions, goto/label,
  closures, table constructors, method calls, builtin
  redefinition/shadowing, large declaration lists, and a boundary
  literal pool routed into exact contexts (`//`, `%`, equality,
  `%.0f`, numeric-for bounds, parameter passing).
- Scale: 4,000 generated programs × 2 runs (self-stability) × 7 rows
  (src + plain/opt/safe/target32/target128/abs8192).
- First run (base tip): 166 divergent rows in the real classes
  (COMPILE 0, LOAD 5, STATUS 14, TRACE 147) across 37 programs;
  oracle classes TOLERANT 288 rows, ORACLE-NAME 374 rows, UNSTABLE 39
  rows (all pairs()/address oracle-environment cases), SRC-INVALID 2
  (generator artifact: >200 locals).
- Every real divergence was minimized (ddmin, line-level) and
  attributed by single-pass toggle hunts against the verified pass
  registry.

## Candidates found and fixed in this cycle

- **K** (`0f2ec89`) — value substitution into Funcstat definition
  targets (tiny-literal-inlining, single-use-global-forwarding,
  constant-argument-specialization and the literal-fold clone paths)
  could place a literal at the root of a `function T:m()` target
  chain, which the printer cannot parenthesize: valid programs were
  refused with "No semantically valid optimization candidate
  survived verification."
- **L** (`754a1a1`) — root-local-globalization's member-base timing
  exclusion did not descend into function bodies, on a recorded
  premise that upvalue bases materialize like globals; they locate
  through their cell at store time, so a base rebound by the RHS
  call was indexed in its new (non-table) value.
- **N** (`c184d6e`) — one-use-function-fusion spliced a callee's
  parameter/local declarations between a `goto` and its label,
  producing code raw Lua refuses to load.
- **M** (`3b280bb`) — final-dead-store-elimination's aggressive
  waiver dropped a dead global store whose right-hand side could
  throw, waiving `may_throw` wholesale instead of requiring the
  existing total-numeric proof.
- **O** (`1750618`, `ed81784`) — `may_throw` is fed by a
  definitely-throws oracle that leaves dynamically typed arguments
  unjudged; elimination consumers (dead-local, unused-argument,
  global-store cleanup) read "unjudged" as "cannot throw" and
  dropped computations that throw at run time. New
  `EffectAnalyzer::is_droppable` requires statically acceptable
  arguments for every total-API call in the dropped expression.
- **P** (`5643c2f`) — numeric-literal rounding fed comparisons: a
  within-tolerance rounding of either operand can flip the discrete
  result, so all six comparison operators are exact routes.
- **Q** (`a2c57a0`, `5c41d31`, `fc301a7`) — the same rounding fed
  discretizing builtins. Root cause: constant folding spelled fold
  results with a shorter within-tolerance approximation in
  aggressive mode (`aggressive_literal_node`), with no route
  analysis: `math.max(2.5, 3.0000000000000004)` folded to the
  literal `3`, and `math.ceil` of it computed 3 instead of 4. Fold
  results are now always spelled exactly; the numeric-literals pass
  (which performs the exact-route analysis) additionally treats
  `math.floor`/`math.ceil`/`math.tointeger` calls — including
  through api-alias member reads — as exact calls.
- **R** (`91371ad`, `423cf59`) — Lua performs the stores of one
  assignment right-to-left, so with a repeated target the binding
  holds the FIRST part's value. Three consumers modeled a repeated
  target as last-wins: single-use-global-forwarding built one plan
  per part and forwarded a later part's value; global-store cleanup
  dropped the self-assignment part that executes last and decides
  the final value. Both now refuse repeated targets.
- **S** (`8df246f`) — closed-namespace scalarization discarded a
  binding's non-recorded initializers (a local declaration part,
  other assignments) without checking their evaluation; after
  fusion supplied a pure initializer, a constructor element calling
  a nil global was discarded and its error vanished. Candidates
  whose discarded initializer is not a pure table literal are now
  rejected.

## Carried over (recorded, with repros in the evidence package)

Seventeen programs still diverge, all TRACE class (no COMPILE,
LOAD, or STATUS divergences remain): source and compiled code both
fail, but a different operation's error surfaces first (the
first-error identity is observable in the harness trace). They are
recorded as candidate family T for the next cycle, with repros
preserved. For four of them (f001089, f001526, f002197, f003108) the
ddmin-minimized form now agrees, so the trigger lives in parts the
minimizer removed and the repro is the original program; f001957
could not be minimized in-cycle. f000543's apparent divergence is
the harness's table-address normalization gap (oracle class), not a
compiler divergence.

## Final validation

- Campaign re-run at the fixed head (same 4,000 programs):
  COMPILE 0, LOAD 0, STATUS 0 (run 1: 0 / 5 / 14), TRACE 77 rows
  over 17 programs (run 1: 147 over 33), TOLERANT 224, ORACLE-NAME
  387, UNSTABLE 39, SRC-INVALID 2 (unchanged generator artifact).
- Corpus Tier FULL (4,155 files × 4 configs, cumulative from
  `c9c41fe`): status pairs unchanged on every row (4,128 ok / 27
  error per config in both); 14,404 of 16,620 rows byte-identical;
  2,216 rows changed in code only (+42,740 characters in total,
  2,024 rows longer, 8 shorter) — the expected cost of exact fold
  spellings (candidate Q) and of the elimination suppressions
  (candidates M/O/S), which keep computations whose evaluation the
  source performs.
- Workspace tests: 1,680 passed / 0 failed (50 suites).
- Clippy (`storm-lua-minify`, `storm-lua-analysis`): 9 warnings at
  the head, 9 at the base — no new warnings. rustfmt clean on all
  changed files.
