# Member caches: the 200-local budget for cache insertions (cycle 94) — verification record

Codex adversarial review run #2, finding 7 (S1 form, reproduced by
w9 cycle 54), extended to MR and SMC by w6 cycle 47's candidate B
and by this cycle's own measurement (below).

## What changed

Three implementation commits:

- `5e3d6ba` — S1 (`single_member_cache.rs`): groups whose cache
  local would exceed Lua's 200 simultaneously-active-locals limit
  stay in original spelling. Introduces `max_active_locals`, a
  conservative whole-function maximum.
- `ae57a89` — MR (`loop_member_reads.rs`): the same gate on both
  insertion paths (loop preheader and per-iteration body insert),
  sharing `max_active_locals` / `MAX_LOCALS` as `pub(crate)`.
- `12ea749` — SMC (`straightline_member_reads.rs`): the same gate
  at the segment-group insertion point.

Regression tests per pass, including both sides of the boundary
(fires at 199 active locals, skipped at 200).

## Root cause

Lua refuses to load a function with more than 200 simultaneously
active locals ("too many local variables (limit is 200)"). The
member caches each insert one local per cached group, but none of
them accounted for that budget:

- Codex's S1 case: a 196-parameter function plus a numeric-for
  loop (loop variable + three internal controls = 200 active)
  received one S1 cache local — the compile reported success and
  the generated code failed to load.
- w6 cycle 47's gg2 probes (candidate B): a 194-parameter
  function with two locals and a numeric-for loop sits at exactly
  200 active locals; an inserted cache local makes 201. At 199
  (the 193-parameter control) every configuration is fine.

## Scope note: why SMC is included

The parent assignment for this cycle named MR (candidate B) and
initially excluded SMC, because w6's same-shape SMC probe had
produced no insertion (undetermined). During this cycle's
verification, the gg2 194-parameter probe still failed to load at
slack32 after the MR gate landed — and the winning layout there
is `joint-api-local-smc`: the 201st local came from SMC's body
insert, not MR's. The exclusion premise no longer held, so the
gate was extended to SMC in the same finding-7 round and is
recorded here rather than left as a known live bug.

## The gate and why it is sound

`max_active_locals(source, params, body)` computes a conservative
maximum of simultaneously active locals over the function's own
body: parameters plus `local`/`local function` declarations
accumulated per block path (branches take the maximum, blocks
restore the count on exit), a numeric for contributing
variable + 3 controls, a generic for contributing its variables
+ 3, and nested functions counted separately (they have their own
budget). Each pass then tracks how many cache locals it has
already inserted into the function and skips a group when
`max + inserted + 1 > 200`.

Composition across passes is sound without a shared counter:
composite layouts run the passes sequentially, each pass computes
its maximum from its own input AST — which already contains the
locals earlier passes inserted, as ordinary `Local` nodes — and
adds only its own insertions on top.

## Verification (base `2f906671b` = round 197; cumulative over the
cycle-94 stack, head `12ea749`)

- Differential execution (canonical diffexec, plain/opt/safe +
  target32/target128/abs8192):
  - Cycle-54 case f07: at base, target128 and abs8192 error
    (vm-error); at head, all six configurations agree.
  - w6 probes `p-gg2-mr-194` (at the limit) and `p-gg2-mr-193`
    (control): both agree across all six configurations at head.
    Before the SMC gate, the 194 case still errored at target32;
    after it, no configuration errors.
- Tier FULL (no-target corpus, 16,620 rows, cumulative for the
  whole cycle-94 stack): byte-identical to base in all four
  conditions (identical 4,155 per condition; status pairs
  (ok, ok) 4,128 / (error, error) 27; no flips).
- Target-delta: at both slack32 and slack128, one transition
  (file index 911, S1→S1, −10 chars), lost-S1 0, free-side
  differences 0 — no corpus winner sits at the local limit.
- Workspace tests: 1,459 pass, 0 fail (new boundary tests: S1 at
  195/196 parameters, MR at 193/194, SMC at 196/197 plus the
  loop-body form at 193/194); fmt clean; no new clippy warnings
  in the changed crates.

## Re-evaluation conditions

- Any future pass that inserts locals (cache or otherwise) into
  generated code must join this budget discipline — gate on
  `max_active_locals` of its input AST plus its own insertion
  count — rather than assuming headroom.
- The counting model follows Lua 5.3's per-function local limit
  and register accounting for loop controls; if the dialect basis
  changes, re-derive both the limit and the per-loop increments.

Semantic reference dialect: Lua 5.3 (Stormworks).
