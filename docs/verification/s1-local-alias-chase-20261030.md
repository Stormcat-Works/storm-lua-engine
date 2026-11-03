# S1 barrier resolution through locals (cycle 84, candidate B1) — verification record

Date: 2026-10-30. Branch: `muse/w4`, commits `4454076` (analyzer
fix) + `18766b2` (B1), stacked on `7e4d6fb` (cycle 83, candidate A).
Integration: held (merge pending the .wsl handling decision);
integration tip at verification time was Round 188 (`70e7a7922`),
whose intervening rounds are all output-invariant, so the
verification against base `7e4d6fb` carries over.

## What changed

`call_builtin_for_barrier` in
`crates/storm-lua-minify/src/passes/single_member_cache.rs` gained
a third resolution step (post-alias form only): when a callee does
not resolve directly, its spelling is chased through
never-reassigned locals — `resolve_builtin_through_locals` follows
local initializers (Name chains and Index bases, depth-capped at
8), trying the analyzer's own resolution at every step, and
rebuilds the dotted builtin spelling. Only dotted (member)
spellings are adopted. The recovered spelling then passes through
exactly the same stateful list and argument rules as a directly
spelled call.

Why this is the right shape: the census (below) showed that the
dominant "unknown call" population in S1 opportunity loops is not
user helper functions but the api-alias layouts' own localized
builtin members (`local H = string.sub` and friends, including
member reads through localized root locals). Those locals are
created by the compiler itself; the barrier simply could not see
through them, because `EffectAnalyzer::resolve_builtin_reference`
propagates aliases only through chunk-top global assignments.

Soundness basis (also in the module docs): each chased binding
has zero recorded writes, so its value was fixed by the single
evaluation of its initializer; a member value captured at the
declaration denotes exactly what the same spelling denotes at
that point — the identity standard the direct resolution already
uses. The chase is used only for the barrier decision; nothing is
rewritten on its basis.

## Accompanying analyzer fix (`4454076`)

Found by a B1 regression test: the `EffectAnalyzer` api-alias
fixpoint propagates an alias from a chunk-top `Assign` when the
target has exactly one *counted* write. A declaration initializer
is not a counted write, so a local with an initializer plus
exactly one reassignment (`local f=string.sub f=string.find`) was
aliased to the reassignment's value — wrong before the assignment
or under a branch. Both fixpoint copies (`new` and
`reference_new`) now skip bindings whose declaration carries an
initializer (`binding_has_initialized_decl`: a `local function`,
or a `local` whose name position has an initializer expression;
position-aware, so `local a,f=1` still propagates for `f`).
Five regression tests pin the boundary, including the
uninitialized-local and global controls, which still propagate.

## Census (design scoping, diag-only builds)

Diagnostic clones of `7e4d6fb` with env-gated instrumentation
(`c84_gate_patch.py` / `c84_gate_patch2.py`, throwaway) dumped
every opportunity loop (a loop holding a single-occurrence
cacheable member call) at each file's slack32 target, with the
full barrier inventory per loop. The inventory replica matched
the real `loop_disqualified` on all 2,952 opportunity loops
(v2 mismatches were exactly the 160 B-rescuable loops, all in
the lifting direction).

Unknown barrier calls, v1 classification: local-function calls
(U1) were essentially absent (1 call in the whole corpus); the
dominant class was calls through single-letter locals (3,952),
identified as localized builtin members. With the chase
implemented in the diagnostic (v2): 3,928 calls resolve to builtin
spellings (string.sub 2,445; string.find 351; screen.setColor
186; table.insert 181; input.getNumber 93; …), 32 resolve to
disqualifying builtins and stay barred, 0 violate the argument
rule. B-rescuable loops: 160; non-S1-winner files with at least
one rescuable loop: 125 (110 fully unblocked); S1-winner files:
20. The effect-summary design for same-chunk local functions
(candidate B2) was measured to have no population (2 U1 calls,
1 clean) and was not implemented; global-function callees
(U3, 680 calls) remain future work (candidate B3: a
single-definition-plus-dominance proof).

## Verification results

- Workspace tests: 1,403 passed, 0 failed (analysis 111 incl. the
  5 new alias-definition tests; minify 876 incl. 6 new B1 tests).
  `cargo fmt` clean; no new clippy findings in the touched files.
- Tier FULL (base `7e4d6fb` vs head `18766b2`, corpuscheck,
  4,155 files × 4 conditions): **byte-identical**, 16,620 rows,
  raw diff 0. Neither the analyzer fix nor B1 changes any
  no-target output.
- Target-delta (targetbench, base vs head):
  - slack32: 128 transitions (+1,370 chars) — 115 files newly
    enter the S1 family, 13 S1→S1 winner changes (additional
    cached groups), 0 S1 losses, 0 non-S1 transitions.
  - slack128: 141 transitions (+1,637 chars) — 123 new to S1,
    18 S1→S1, 0 losses.
  - No-target (free) size/layout differences: 0.
- Differential execution spot check (tracecheck, all 269
  transition cases = 128 slack32 + 141 slack128): 237 exact trace
  matches. The remaining 32 all triaged: differences are only
  generated-code line numbers and renamed identifier spellings
  inside error messages / stack traces; masking those slots makes
  base == head for all 32 (one also equals plain). Error kinds,
  message bodies, and reach points identical; semantic
  differences 0; compile failures 0; self-unstable 0.
- .us2 execution comparison (MEASURE.lock, one job, all 128
  slack32 transition files, 11 alternating pairs): 83 files
  passed the smoke test (45 excluded as harness-environment
  errors; equivalence for those is covered by the tracecheck
  above). Tick totals over the 83 files: 18,668.4 µs → 18,493.5 µs
  (−0.94%; the total is 96% one heavy file at −0.69%, excluding it
  ≈ −1.4%). Per file (paired median diffs): tick improved in 47
  files, worsened in 35; improvements ≥3% in 13 files (largest
  −10.3%, file 267; −7.6%, file 1162; −7.4%, file 3136) vs
  worsenings ≥3% in 7 files, all on 1.6–3.6 µs files with absolute
  deltas of +0.06–1.1 µs. Draw (33 files) is a wash in aggregate
  (13 improved / 20 worsened, mostly sub-µs); two files show a
  consistent ~1 µs single-phase regression (2532 draw +1.32 µs,
  0/11 pairs; 3438 tick +1.11 µs, 3/11) — both are S1→S1
  additional-group winners, i.e. the known winner-selection
  tradeoff of caching a group whose phase trip count is low; the
  mechanism was not isolated further. Net assessment: positive
  in aggregate and in file count for tick (the phase these
  logic loops live in), no material regressions.

## Notes

- The chase runs only inside S1's barrier resolution
  (`call_builtin_for_barrier`'s only caller is `call_is_barrier`);
  SMC/MR keep their own gates and are unaffected.
- Compile-time: target-delta wall time showed no notable change
  (the chase is depth-capped and only runs for callees the direct
  resolution already failed on).
