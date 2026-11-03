# Verification: seed-6901 campaign, cycle-68 residuals AE/AF, link candidates L1–L3, dialect candidates M1/M2 (w9 cycle 69, 2026-11-01)

Base: integration tip `92721fe` (docs) / FULL-VERIFY-TIP `0615e05`
(Round 280). Harness: rawexec (raw Lua 5.3 via mlua), 7 configurations
per program (src/plain/opt/safe/target32/target128/abs8192). Tier FULL
is cumulative over all four fix commits of this cycle.

## Generative campaign, seed 6901

4,000 generated programs × 2 runs × 7 rows (28,000 rows per run).
178 programs carried at least one non-agree verdict row (625 rows):

- ORACLE-NAME 363 rows, TOLERANT 168 rows (oracle classes, closed by
  the classifier rationale; the compound ORACLE-NAME+TOLERANT class of
  the cycle-68 driver produced 0 rows this seed).
- UNSTABLE 53 programs: the source (oracle) trace itself differs
  between the two runs (spot-checked f000085: src trace hash
  b10a08c1… vs 0e6a6477… across runs) — pairs-iteration order of the
  harness's fresh Lua states, as in earlier cycles.
- SRC-INVALID 2 programs (f001969, f003761): the generated source
  itself fails to load in raw PUC ("too many local variables (limit
  is 200) in function") — generator artifact, not a divergence.
- RUNFAIL 1 program (f002061): rawexec is SIGKILLed (rc=137) in both
  runs on a 1,481-byte program generating unbounded work — oracle-side
  resource exhaustion, not a divergence.
- TRACE 38 rows / 7 programs, all triaged to the RESOURCE class:
  event prefixes are exact up to the termination point and both sides
  end in a runtime error; one side reaches a harness resource limit
  (stack overflow / rawexec instruction budget) at a different
  repetition count than the other.
  - stack-overflow reach: f000377 (5 rows), f000394 (5), f002371 (5),
    f002563 (5). f002371's compiled forms recurse ~133× deeper before
    the limit (smaller compiled frames); prefix identical for 7,518
    events.
  - instruction-limit reach: f000933 (6 rows), f003313 (6),
    f003539 (6).

No real divergence was found in this seed's campaign.

## Candidate AF (cycle-68 residual (a), expanded): immutable carrier synthesis trap guard

The cycle-68 fix (AC) rejected synthesis candidates whose hoisted
subexpressions trap on a *literal* zero divisor. Probing the residual
— divisors that only become zero (or operands that otherwise trap)
after derivation — confirmed STATUS divergences (source ok, compiled
runtime-error; aggressive path only) for: a bare read of a zero-valued
global as divisor; `v9 - v9`, `v9 % v9`, `v9 * 0` as divisors;
bitwise `&` on a derived non-integer float; arithmetic on a
non-numeric string literal; arithmetic on an index into a table
constructor holding a string. The guard only rejected literal-zero
divisors and a few base shapes.

Fix (`immutable_synthesis.rs`): the literal/identity guard is replaced
by a premise evaluator. Each binding's numeric-premise value is
tracked as a `literal_folding::Literal` alongside the existing
numeric initializers and folded with the same Lua-5.3-faithful
`eval_binary`/`eval_unary` used by literal folding; index into a
table constructor evaluates the matching field (absent key = Nil).
A candidate is rejected when any hoisted arithmetic/bitwise/
relational operation is a *certain* trap under the premise, when a
`//`/`%` divisor is not premise-evaluable, or when an integer-zero
divisor meets a dividend that is not provably float (float division
by 0.0 stays legal and still synthesizes). 6 new unit tests;
11/11 pass in the module. All probes agree post-fix.

## Candidate AE (cycle-68 residual (b), expanded): numeric-literal exact routes vs dynamic keys

`collect_written_member_names` recorded only Str-literal member
writes, and callee identity checks only recognized Str-literal member
keys, so a rebinding performed (or a call performed) through a
dynamic key escaped the exact-route protection and the literal
ceil/floor approximation misfired (TRACE: `math.min` returning the
amplified-ceil result −3 instead of −2). Confirmed at base for:
writes via a global key variable, writes via a pairs-loop key, direct
calls through dynamic keys (global and local), constructor-field
aliases of exact members, and constructor fields holding a chunk
function that discretizes.

Fix (`numeric_literals.rs`): written members are now classified as
`WrittenMembers { names, dynamic_bases, dynamic_anywhere }`. A
dynamic-key write is chased to its single-definition table-constructor
root binding when it has one (recorded per binding) and otherwise
sets `dynamic_anywhere`. `is_exact_call` additionally treats as exact
any call whose callee is read through a non-Str key, any indexed
callee while `dynamic_anywhere` is set, any callee whose base root
was dynamically written, and constructor-field callees whose field
value is itself an exact/dynamic read or a chunk function. A negative
control (dynamic write into a fresh local table) confirms unrelated
`math.min` calls are not poisoned. 2 new test functions (7
assertions); 12/12 pass in the module. All probes agree post-fix
(except pb6, which is TOLERANT by design before and after).

## Candidates L1–L3 (w6 cycle 62, suite v25): link expansion

All three candidates root in the Round-241 link expansion design:
vararg modules fell back to textual inline expansion, and expansion
(glue/inline) was emitted only at the first require site.

- **L1 — chunk-level `...` not preserved by inline expansion.**
  The Iife form wrapped the body in a non-vararg function (syntax
  error, b03/g03), the do-block form inside a wrapper module did the
  same (b06), and the chunk-head do-block form silently rebound `...`
  to the *entry* chunk's varargs (b02: ref `1|m|5`, linked `0|nil|5`).
  Fix: vararg modules now use the wrapper form like every other
  module — the wrapper is itself vararg (`function(...)`) and the
  glue calls it with the module key, reproducing require's chunk
  argument exactly. Ambient members, which must stay inline at the
  injection point, use a vararg IIFE called with the synthetic
  `root.member` key.
- **L2 — label namespace leak of inline expansion.**
  `expand_inline` spliced module text into the importer's chunk, so a
  module goto with no visible label in its own chunk bound to the
  entry's same-named label (c03b; c03 was masked in plain but ran in
  min). Fix: project modules are all wrapper-expanded now (function
  scope isolates labels), and the remaining inline path (ambient
  injection) always uses the IIFE form, so its labels are
  function-scoped too. c03/c03b now load-error with the same class as
  the reference ("no visible label 'L'") in plain and min.
- **L3 — glue/inline re-execution at a top-level goto loop.**
  Expansion is emitted at the first site only, but a chunk-level goto
  loop re-reaches that statement: the wrapper glue then called the
  module *result* (a10: "attempt to call a number value"), and the
  inline body re-executed (b07: counter 2 instead of 1). Fix: the
  glue is guarded by a per-module done flag (a fresh reserved
  global, generated with the same collision-checked `fresh_var_name`
  machinery): `if not <done> then <done> = true <var> = <var>(...)
  ... end`. The flag is separate from the result variable, so a
  module returning a function value is never re-called. Inline
  re-execution disappears with L1/L2 (no inline expansion remains at
  require sites; ambient injection sits before the entry's first
  statement, unreachable from its goto loops).

Design notes: the Shape/do-block machinery of `expand_inline` is
removed; `expand_inline` is ambient-only and always IIFE. Result
variables and done flags add one extra reserved global per module.
6 new unit tests in link.rs (148/148 in storm-lua-build).

Suite v25 re-run (w6link, all 44 cases, this tree): the 8 candidate
cases (a10, b02, b03, b06, b07, c03, c03b, g03) are resolved and the
b05 agree-control still agrees; the 14 fail-closed cases are
unchanged (same diagnostics); the 22 agree cases are unchanged with
two text-level exceptions that are not semantic changes: (i) c04's
load-error message embeds the linked-output line number of the
entry goto, which shifted because the guarded glue is longer — its
status and its LinkedRange-mapped line (main:4) are identical to the
reference; (ii) for c03/c03b the load error is now reported by the
loader at the wrapper's generated closing line, which carries no
source range (mapped=NONE) — the error class and failing module are
the reference's, but line attribution for load-time goto errors
inside wrapper bodies remains a cosmetic gap of the range table
(pre-existing property of generated wrapper lines).

## Candidates M1/M2 (w6 cycle 63, suite v27): dialect rewrite

- **M1 — scaffold drops stores for syntactically duplicate index
  targets whose runtime locations differ.** `target_identity` is the
  target's token text; the store loop skipped any store whose
  identity was already stored. With an impure key (`t[k()], t[k()] =
  10, 20`, k returning 1 then 2) the two targets freeze to different
  locations and the first store was silently lost. Sibling form
  p04b: a sibling call mutating the key variable between the two
  address evaluations splits the locations the same way. Fix
  (`dialect.rs`, `collect_assign_edits`): Name targets keep the
  static last-occurrence dedup (a name's location is the variable
  itself). For Index targets, the dedup is decided at runtime
  against the frozen address table: a store is emitted under a guard
  `if not (pair == later-pair or ...)` comparing the frozen
  (base, key) pair with every later same-identity pair via `==`
  (value equality for scalar keys is slot identity; table bases/keys
  compare by identity). The scaffold design is unchanged otherwise
  (do-wrap, two table temporaries, ≤2 locals per statement,
  collision-checked generated names). Two documented boundaries:
  (1) p04b's reference is PUC's register model (name keys re-read at
  store time: `nil|10|20|2` after nil-collapse `10|20|2`); the
  dialect's documented model freezes addresses in appearance order,
  and the fix follows the documented model — p04b now rewrites to
  `10|30|20|2` (both stores), which matches neither the buggy output
  (`30|20|2`, first store lost) nor raw PUC, by design. (2) `==` is
  used because this dialect environment provides no raw-equality
  function for generated code to call; the only corner it leaves is
  distinct objects that the program's own `__eq` metamethod declares
  equal, in which case the program's own equality definition governs.
  2 new unit tests.
- **M2 — goto-wrap search aborted at the first non-wrappable goto.**
  Three `?` early returns in `find_goto_wrap` (unnamed goto, label
  not later in the same block, goto crossing no locals) aborted the
  whole scan, so at most one wrap was ever applied per chunk
  (q04/q04b: second wrappable goto left as a load error; q04f: a
  clean goto first made the scan report changed=false). All three
  sites now `continue` to the next goto; the driver's re-scan loop
  wraps the rest. 3 new unit tests.

Suite v27 re-run (w6dialect, all 39 cases, this tree): **0 flags**
(p04b judged against the documented freeze model above; the other 38
against w6's pre-registered expectations verbatim). Line counts and
the rewrite fixed point hold for every case.

## Tier FULL (cumulative, base = FULL-VERIFY-TIP `0615e05`)

- Corpus 4,155 files × 4 configs = 16,620 rows per side, batch mode,
  0 fallback files.
- Base side reproduces the previous head exactly: raw TSV SHA-256
  `b3c4b90c8aaa9e0ca5131e2eebc7b1765fa3157218385a9fd34a58b2e912674a`
  (byte-identical to w6 cycle 62's independent .us reproduction of
  the Round-280 head rows) and total output 26,620,868 bytes.
- Head side: raw TSV SHA-256
  `9713a05d5ab6f9d497510fa48a8aafc9693a13742cab8e1f98f1bfd0fe4b8548`;
  total output 26,620,904 bytes (**+36** vs base — the AF/AE fixes
  cost characters, which the rules allow for bug fixes).
- Status changes: **0** (ok 4,128 / error 27 per config on both
  sides). Hard diffs: 14 files, the same set in all 4 configs —
  attributable to the AF/AE minify fixes (the link and dialect
  changes do not affect single-file corpus compilation).
- Workspace tests: **1,728** passed, 0 failed, 50 suites (cycle 68:
  1,709; this cycle adds 19 test functions: 8 minify, 6 link,
  5 dialect).

## Integration

Four fix commits on top of `92721fe`, merged by fast-forward under
MERGE.lock (rounds assigned at merge time): AF (immutable synthesis
trap guard), AE (dynamic-key exact routes), L1–L3 (link expansion),
M1/M2 (dialect scaffold + goto wrap). New FULL-VERIFY-TIP is the
dialect commit.
