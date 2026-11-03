# S1 barrier resolution through chunk-defined global functions (cycle 86, candidate B3) — verification record

## What changed

One file: `crates/storm-lua-minify/src/passes/single_member_cache.rs`.

A loop that `loop_disqualified` rejects is rescued when *every*
barrier in the loop body and headers is a call to a chunk-defined
global function whose identity and body safety are provable
(`b3_rescue_loop`). The rescue changes only the barrier decision;
nothing is rewritten on its basis, and a rescued loop then goes
through exactly the same candidate collection and transform as a
loop that was never disqualified.

Identity — the callee binding must satisfy all of:

- the chunk passes the dynamic-global gate shared with
  `single_use_forwarding` / `closed_fields` / `global_stores`
  (no `_ENV` reference, no call spelled `rawget` / `rawset` /
  `load` / `loadstring` anywhere), so the resolver's write counts
  cover every write to a global binding;
- exactly one recorded write, and that write is a plain-name
  `Funcstat` that is a direct statement of the chunk block;
- the definition precedes the loop's top-level chunk statement
  (dominance), and the chunk block's non-function subtree has no
  goto/label, so no jump can skip the definition.

This is the same identity standard as the global form of
`alias_table_root`, applied to a function value instead of a table
value. The cycle-65 "no calls between definition and loop"
condition is not needed: a function value is immutable, and only a
write to the binding — excluded by the write count — could change
what it denotes.

Body safety — starting from the loop's B3 callees, every
chunk-defined function transitively called must satisfy, under the
same vocabulary as the loop scan:

- no goto/label in its body (outside nested functions);
- field writes only through bases proven harmless by
  `write_base_harmless`, evaluated at the write's own statement
  position inside its own function (the position generalization of
  the cycle-83/65 proofs: the proof is about the write site, so
  the site's own context is the right one);
- global name writes only to bindings that are neither builtin
  roots nor reserved names nor any callee of the closure;
- every call is a known builtin (direct, through the cycle-84
  local chase, or through a proven alias base) that is not
  stateful and passes no bare root/alias table as an argument, or
  another chunk-defined function admitted to the closure (a B3
  global, or a write-free local function read from its
  declaration), under the same argument rule; method calls are
  disqualifying;
- function values occur only in definition positions — a function
  value passed, returned, or stored could run later through paths
  the call graph cannot follow (higher-order builtins such as
  `table.sort` comparators included).

Calls in the loop body itself keep the plain argument rule: a B3
call passing a bare root or alias table stays a barrier.

18 new unit tests (4 positive, 14 negative) pin each condition
individually: definition position (before/inside a block/after the
loop's function), reassignment, double definition, stateful and
unknown callees inside bodies, root-passing at both call sites,
root-field and builtin-name writes inside bodies, goto in a body,
function values in argument position, method calls, and the
`rawset` chunk gate.

## Census (design scoping, diag-only builds)

Diagnostic clones of `8b8a260` with env-gated instrumentation
(`c86_gate_patch.py`, throwaway) reproduced the cycle-84 census
exactly at the same head: U3 (global function calls in opportunity
loops) = 680 occurrences / 611 distinct call sites, external or
undefined globals 622 (= cycle-84 U6 357 + U5n 265), Index callees
200 (= U8 188 + U9 12), direct local-function callees 2 (= U1 2).
Validations: diag-run winners match the cycle-84 head census on
all 4,128 files; the barrier-kind replica agrees with the real
`loop_disqualified` on all 10,510 rows; the analyzer's and the
resolver's function records agree on every U3 entry.

Strict-form funnel over the 611 sites:

- identity by write count: 21 sites fail (reassigned or multiply
  defined);
- definition shape: 15 more fail (unique write is a `Funcstat`
  but not a direct chunk statement);
- dominance: 250 of the remaining 575 fail — the dominant shape
  in the corpus is helpers defined *after* the callback that
  calls them;
- fully released: 36 loops in 33 files (25 with non-S1 winners).

A relaxed variant — drop the direct-statement and dominance
requirements, keep the dynamic gate, the unique-`Funcstat`
identity, and the closure conditions — passes identity for
590/611 sites and releases 66 loops in 59 files (49 non-S1). Its
soundness basis is the body-set argument: with exactly one
definition in the chunk, a call can only ever run that one body,
or fail on a not-yet-defined (nil) callee, which aborts or is
caught identically in both versions because S1 moves no calls.
It is *not* implemented here: dropping dominance changes the
proof's character away from the dominance standard this codebase
uses for every comparable gate (`alias_table_root`, cycle 65),
so it is left as a separately-decided extension (recorded as B3b
in the cycle-86 design notes). Implementing it later is a small
delta on this code.

One deliberate difference from the census predicate: in the
implementation, loop-body B3 calls must also pass the argument
rule (the census counted them as admitted without it, matching
how the B1 census preceded its implementation). The realized
transitions below are consistent with that difference being
small.

## Verification results

- Workspace tests: 1,421 passed / 0 failed (1,403 at the base,
  +18 new). `cargo fmt --check` clean; clippy: no errors, no new
  warnings.
- Tier FULL (target-free corpus, base `8b8a260` vs head):
  16,620 rows byte-identical.
- Target-delta (targetbench): slack32 — 29 transitions
  (+193 chars): 21 new to the S1 family, 8 S1→S1 (7 of them to
  the partial variant), 0 lost. slack128 — 34 transitions
  (+420 chars): 25 new to the S1 family, 9 S1→S1, 0 lost.
  Target-free winner sizes/layouts unchanged everywhere.
- Differential execution (tracecheck over all 63 transition
  cases): 56 exact matches; 6 cases where base and head traces
  agree with each other and differ only from the plain compile
  (pre-existing target-mode difference); 1 case (slack128)
  differing solely in the generated-code line number inside an
  error traceback (75 → 77) with identical error text and draw
  stream. Semantic differences: 0. Compile failures: 0.
  Self-unstable: 0.
- .us2 execution comparison (single MEASURE.lock job, slack32
  transitions, 11 alternating pairs per file/metric; 16 of 29
  files passed the harness smoke test — the 13 exclusions are
  the known harness-environment error class and are covered by
  the differential execution above): tick paired-median diffs
  sum to ≈ −0.17% over the 16 files (the raw totals show
  +0.68%, dominated by within-run drift on the three largest
  files; the largest single improvement is −2.05% paired,
  9/11 pairs). Draw shows consistent +0.4–0.6µs regressions in
  four small files (paired sum +2.3% over a 73.7µs base) with
  tick flat-to-better in the same files — the low-trip-phase
  winner-selection trade-off class recorded in cycles 83/84 —
  and one tick regression of +1.6µs paired (+1.74%) on a 94.7µs
  file. Net execution effect is approximately neutral; the
  structural gain is 21–25 files moving to S1 winners.

## Notes

- Target-mode only, like the rest of S1: target-free output is
  byte-identical (Tier FULL above), so the change can only
  affect target-size layouts.
- The branch carrying this change also carries candidates A
  (cycle 83) and B1 (cycle 84); integration is unchanged in
  shape (rebase onto the integration tip under MERGE.lock).
