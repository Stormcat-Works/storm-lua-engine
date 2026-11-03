# Verification: seed-6801 campaign + implicit-self audit — candidates AA–AD (w9 cycle 68, 2026-11-01)

Base: integration tip `e3c46c6` (docs) / FULL-VERIFY-TIP `96024c3`
(Round 276). Harness: rawexec (raw Lua 5.3 via mlua), 7 configurations
per program (src/plain/opt/safe/target32/target128/abs8192).

## Driver fix (pre-campaign)

The campaign driver's single-class classification dropped compound
oracle classes into TRACE. `trace_class` now tries, in order:
annotation-erasure alone → ORACLE-NAME; tolerance alone → TOLERANT;
tolerance on the annotation-erased texts → ORACLE-NAME+TOLERANT;
else TRACE. The compound class is live in practice (4 rows this
campaign; pilot: 4 of 100 programs).

## Generative campaign, seed 6801

4,000 generated programs × 2 runs × 7 rows (28,000 rows per run).
200 programs carried at least one non-agree verdict row:

- ORACLE-NAME 465 rows, TOLERANT 216 rows, ORACLE-NAME+TOLERANT 4 rows
  (oracle classes, closed by the classifier rationale).
- UNSTABLE 34 programs: pairs-iteration order of the harness's fresh
  Lua states; the plain configuration shuffles the same way while the
  source row is stable across runs (spot-checked f000058) — oracle
  artifact class, as in earlier cycles.
- RUNFAIL 1 program (f002423): rawexec completes standalone
  (rc=0) but emits a 1.56 GB trace; the campaign capture limit
  rejected it. Harness resource artifact, not a divergence.
- TRACE 76 rows / 16 programs, triaged to closure:
  - RESOURCE 54 rows / 11 programs (f000330, f000760, f001126,
    f001256, f001918, f002053, f002391, f002639, f002905, f003180,
    f003647): event prefixes are exact; one side terminates on a
    harness resource limit (instruction budget / stack overflow) —
    smaller compiled frames recurse deeper or compiled loops reach
    the budget at a different repetition count. Same closed class as
    cycle 67 (b).
  - OUTPUT-STORE 5 rows (f003866): a same-channel output write
    overwritten later in the same tick is removed by
    redundant-output-store-elimination; disabling the pass restores
    agreement. Closed under the 2026-10-31 output certification
    (last-executed-wins per channel; game-observable state identical),
    same as cycle 67's f003747.
  - TOLERANT+OUTPUT-STORE 5 rows (f002649): 1e-9 draw-value rounding
    plus the same output-store removal.
  - Real divergences: f002850 → candidate AB, f002874 → candidate AC,
    f002446 → candidate AD (4 rows each). All three programs agree in
    every configuration after the fixes.

## Candidate AB — single-use sinking reorders multi-value siblings

`local a, b, c = math.max(2, #T), (T .. "x"), math.max(3, 4)` followed
by a use of `a`: the components of a multi-value declaration evaluate
in source order, but sinking the may-throw first component into its
use moved it after the second component's initializer, which stayed
in the declaration — the concatenation error surfaced instead of the
length error. Fix (passes/locals.rs): when a sinking candidate can
throw and its declaration has siblings, every sibling must be
unobservable (no throw, call, write, or ordered effect).

## Candidate AC — carrier synthesis evaluates a trapping expression

Immutable carrier synthesis hoisted `(v9.x or 0)` — repeated inside
two never-called functions and dependent only on the single-write
numeric global `v9 = -0.5` — into a carrier evaluated unconditionally
at the top of the chunk, where indexing a number immediately traps;
the source runs cleanly. Fix (passes/immutable_synthesis.rs):
candidates reject subexpressions that certainly trap under the
numeric premise (index read whose base is not a table constructor,
length of a non-string/non-table operand, integer `//` or `%` by a
literal zero). Residual, recorded: a divisor that is zero only after
derivation (e.g. `b - b`) is not detected.

## Candidate AD — literal approximation through a rebound member

In a pairs loop, `math.min(-2.999999999, math.exp(27))` is followed by
`math.min = math.ceil`. The first iteration prints -2.999999999 and
later iterations print -2 (ceil), but literal approximation rounded
the literal to -3, which the rebound discretizer turned into a
whole-integer change. The parameter route into chunk-local functions
is already protected (verified: a literal flowing into a local
function whose parameter reaches math.ceil stays exact); the open
route was rebinding of the callee itself. Fix
(passes/numeric_literals.rs): a member name assigned anywhere in the
chunk (plain or method definition) makes calls through that member
name exact routes, chased through single-definition aliases like the
existing exact-member set. Residual, recorded: writes through a
dynamic (non-literal) key are not tracked.

## Task (2): implicit-self global model audit → candidate AA

Root cause, confirmed by compiled-code dumps: the AST does not bind
the implicit method parameter (NameSite::Parameter: "no implicit
self"), so `self` occurrences inside a method body resolve to the
enclosing binding named `self` — a global, a chunk local, or an
enclosing function's parameter — while code generation prints colon
sugar, so at run time those occurrences denote the receiver. Any pass
that substitutes a value for such an occurrence, or renames it by
binding id, miscompiles. Audit principle: passes may keep the
occurrence as a name read; they must not substitute values for it,
move it across the method boundary, or rename it.

Reproduced at the base tip (24 probes: opt/safe/targets diverge,
plain agrees) through three stacked mechanisms:

1. Closed-namespace scalarization folded the enclosing `self` table
   (global and local forms) into method bodies, reads and writes
   alike (`function T:m() return self.x end` compiled to return the
   global's field, 1000 instead of the receiver's 11).
2. Immutable-table flattening folded a literal local `self` table's
   fields into method bodies.
3. Scope renaming renamed the enclosing `self` binding, reprinting
   phantom occurrences under the new name while the method kept its
   colon form (including the enclosing-function-parameter form, where
   the host was renamed `function b(b)` and the body became
   `return b.x`).

Fix: scalarization and immutable-table flattening skip bindings
named `self`; rename plans never rename a binding named `self`
(applied in both plan builders, so every consumer — application,
lexical winners, and renamed-size measurement — stays consistent).
The rename check compares symbol ids rather than interner strings:
measurement plans also run over trial arenas whose identifier
registry may not hold every id, where reading a binding's name string
is not safe (this surfaced as compile panics on 50 corpus files
during this cycle's first Tier FULL attempt and is fixed by the
id-based check).

Clean audit results (no reproduction; soundness basis recorded):
api-alias seeding excludes `self` at both seed sites (the cycle-67
candidate X fix and the mutated-builtin-roots constructor) and
closed_fields keeps its own self-free alias map, so
`resolve_builtin_reference` never resolves through `self`; the
LICM / MR / S1 `builtin == "self"` hits are all inside
`is_disqualifying_builtin` (barrier direction); reuse-into-method is
not substituted when the carrier is defined at chunk level and read
inside a method; inline methods, reuse across self-calls, loop
caching inside methods, alias writes in loops, bare `self =`
rebinding, known-call writes through the parameter, method-call
loops, closures over method self, explicit `function (self)`,
cross-method devirtualization, and the flattening dynkey/escape
variants all agree. After the fix, all 24 audit probes agree in
every configuration.

## Tier FULL verification (cumulative from FULL-VERIFY-TIP 96024c3)

Full corpus 4,155 files × 4 configurations, base vs final head:

- Status changes: 0 in every configuration (ok→ok 4,128,
  error→error 27).
- Hard (code) differences: 64 files × 4 configurations; map-core
  differences 0; missing rows 0; fallback files 0.
- Total code size 26,617,440 → 26,620,868 bytes (+3,428).
- Sorted-row SHA-256 fingerprints: base `b18001a6…`, final head
  `49519461…`. (Candidate AA alone, measured mid-cycle: 6 hard
  files, status changes 0, fingerprint `bfcba2d3…`.)
- Workspace tests: 1,709 passed, 0 failed (includes 7 new regression
  tests for AA–AD). The changed files are rustfmt-clean; clippy
  warnings on the crate are unchanged from the base.
