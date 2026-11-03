# Verification: cycle-66 campaign T family + candidates U–Y (w9 cycle 67, 2026-11-01)

Base: integration tip / FULL-VERIFY-TIP `7e0d28a` (Rounds 260–268),
rebased onto `2d720ce` (Round 269, Tier INVARIANT) before merge.
Harness: rawexec (raw Lua 5.3 via mlua) over the retained cycle-66
campaign programs, 7 configurations per program
(src/plain/opt/safe/target32/target128/abs8192), 2 runs each.

## T-family triage (17 programs + f000543)

Class (b) — oracle-precision / harness classes, closed with rationale:

- f000543: agrees in all configs at the base tip (cycle-66 note
  confirmed: oracle-side table-address normalization, not a divergence).
- f001178 (orig): instruction-limit artifact — identical event prefix,
  both sides die on harness limits with different repetition counts.
  Its minimized form additionally exposed pairs-iteration-order
  instability of the harness's fresh Lua states (the plain config
  diverges in the same way; the source row is stable across processes
  while compiled-row hashes shuffle) — oracle artifact.
- f001526: source dies with stack overflow after 8,475 events; the
  compiled program reaches the instruction limit at 249,993 events with
  an identical prefix. The source declares 115 locals with a giant
  constant initializer and recurses; dead-local elimination and
  constant folding shrink the per-frame cost ~30x, so the compiled
  recursion runs deeper and hits a different harness limit.
- f001957 / f002197 / f002540 (orig) / f003287: stack-overflow event
  counts differ (+/- small multiples) with identical prefixes —
  smaller compiled frames recurse deeper before the same limit.
  f002197.min differs only in the error-annotation name (oracle class).
- f003747 (orig + min): the only difference is one dropped
  intermediate `output.setBool(7,false)` overwritten by
  `output.setBool(7,true)` in the same tick, with the identical
  subsequent error. Designed redundant-output-store elimination; the
  game-observable state is identical under the certified
  last-executed-wins output semantics (user certification 2026-10-31).
- f001089.min, f001785 (safe config): only the error-annotation name
  differs ('format' renamed) — oracle class.

Class (a) — real divergences, fixed this cycle:

- Candidate T core (f000761, f002526, f003291, f003954, f001089 orig):
  the printer lost left-nested right-associative grouping —
  `((A..B)..C)` and `((x^a)^b)` printed without parentheses once
  redundant-parentheses-elimination removed the explicit Paren nodes
  (e.g. `math.log(3)^2^.5` for `((ln 3)^2)^.5`, changing
  1.0986122886681096 into 1.1422542398868292). Fixed in the printer:
  a bare left-nested same-operator child of `..` / `^` is wrapped.
- Candidate U (f003108, f001785): numeric-literal approximation fed
  integer-coercing / exact-rendering callees. Three root defects:
  the exact-call set missed string.format / string.rep / tostring and
  kin; the member-name fallback compared the raw quoted key token
  (`"ceil"`) against the bare name, so it never fired (dead code);
  and builtin resolution is poisoned chunk-wide by any mutation of
  the builtin root, while alias-rewritten callees are invisible to
  it. Fixed: widened exact set, decoded member keys, and a
  single-definition alias chase for the callee check.
- Candidate V (f003639): aggressive single-use-local sinking moved a
  throwing `string.format` initializer past an earlier throwing
  operand (`true * i4`), changing which error surfaces first. Fixed:
  aggressive sinking now requires the target statement's evaluation
  prefix (up to the sole read) to be non-throwing.
- Candidate W (f002540.min): the function-summary fixpoint "proved"
  a self-recursive function total (its body read its own optimistic
  initial summary), so dead-local elimination dropped a call whose
  stack overflow was observable. Fixed: post-fixpoint peeling —
  summaries on, or reaching, a call cycle (and all summaries when
  the fixpoint does not converge) are downgraded to
  non-pure / non-total / may-throw.
- Candidate X (f000193): `self` appears in the vehicle environment
  roots and was seeded as a stable API root, so method-body `self.n`
  reads were treated as builtin member reads and hoisted to the
  chunk head (`b,c=input.getBool,self.n` evaluated before any code
  ran; the global `self` is nil). The AST does not bind the implicit
  method self, so those reads resolve to the global binding and must
  never be seeded. Fixed in the effect analyzer's root seeding.
  Follow-up audit point: the implicit-self global model could in
  principle affect other passes; only the alias path has a repro.
- Candidate Y (f000864 orig + min): captured-single-use hoisting
  moved a use (`T2 = G3`) to before an unconditional `goto` that was
  supposed to jump over it, executing an assignment the original
  never reached (T2 became 59 instead of nil; a nil comparison that
  should have raised proceeded). Fixed: goto / label / break /
  return are control barriers for both intervening-statement scans.

## Post-fix verification

- All 18 originals + 16 minimized forms + 6 probe programs re-run at
  the fixed tip: every real divergence above agrees; the only
  remaining differences are the documented class-(b) items
  (resource-limit event counts, oracle annotation names,
  pairs-order instability, the f003747 redundant store, and the
  f003954 last-ulp respelling of 2.9999999999999996 inside math.log,
  relative difference ~2e-16, inside the campaign's tolerant class).
- Workspace tests: 1,700 passed / 0 failed at the fixed branch;
  clippy warning set identical to base (9 findings); rustfmt clean
  on all changed files.
- Tier FULL corpus (cumulative from FULL-VERIFY-TIP `7e0d28a`):
  base fingerprint `83eeb485…` (matches the Round-269 record), head
  `34ae2452…`; status pairs unchanged (ok 4,128 / error 27 per
  config); no missing rows, no map-core or explanation-only diffs.
  149 files' generated code differs; per-commit bisection attributes
  the first change of 147 files to candidate V's sinking gate
  (may-throw call initializers no longer sink into targets whose
  evaluation prefix can throw first — a pervasive shape in real
  code) and of 2 files to the printer fix (one of those is also
  touched by the sinking gate). Candidates U / W / X / Y change no
  corpus file: their shapes (adversarial literals under coercing
  callees, dead recursive-call initializers, hoisted `self` member
  reads, goto-skipped hoists) do not occur in the corpus — the
  corpus harness does not execute code, and a grep confirms the
  corpus holds no approximable literal in a position the widened
  exact routes re-protect. Aggregate generated size over the whole
  corpus grows by 1,517 bytes (+0.023%), the cost of the sinking
  gate and the restored parentheses.

## Campaign seed 2 (seed 6701, at tip `470a86d`)

Same generator and bias as cycle 66 (gen-c66, program i uses
`Random(6701 * 1000003 + i)`), 4,000 programs x 2 runs x 7 rows:
COMPILE / LOAD / STATUS divergences 0; TRACE 103 rows over 22
programs; oracle classes ORACLE-NAME 357 / TOLERANT 213 /
UNSTABLE 37 rows. Triage of the 22 TRACE programs:

- One real divergence — **candidate Z** (f003718): statement effects
  for a `Local` node merged only the initializer effects and never
  recorded the declared bids as writes (unlike Assign / Funcstat
  definitions), so captured-use hoisting moved `T1 = ACC` to before
  `local T1 = {table}`; the declaration overwrote the assigned 0 and
  later arithmetic saw the table. Fixed at the root in the effect
  analyzer (declarations record their bids as writes), Round 276,
  Tier FULL from `470a86d`: status changes 0, only 4 corpus files
  change (expression reuse across a declaration is now suppressed —
  the conservative direction), workspace 1,702 tests green.
- The other 21 programs are documented class-(b) items: 15
  resource-limit event-count differences (identical prefixes, the
  compiled side recurses deeper or shallower before the same
  harness limit); 2 composites of a within-tolerance numeric event
  plus an oracle annotation-name event (f002411, f003316 — the
  driver's single-class refinement cannot label the combination);
  1 single-event tolerant difference in 799,970 events (f000267);
  3 redundant output stores dropped before a same-tick overwrite
  on the same channel, with identical continuations (f003300,
  f003580, f003992).

Re-running the identical seed at the post-Z tip `96024c3`:
TRACE drops to 99 rows over 21 programs — f003718 agrees in all
configurations, the remaining set is exactly the class-(b)
programs above, and no new divergence appears.
