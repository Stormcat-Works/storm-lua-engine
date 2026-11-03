# S1 map.* certification: pure coordinate transforms (cycle 93) — verification record

## What changed

One file: `crates/storm-lua-minify/src/passes/single_member_cache.rs`
(implementation commit `3c86bf7`; five unit tests added in the same
commit).

The S1 barrier gate keeps a list of builtins that disqualify a loop
outright (`is_disqualifying_builtin`). Until now the list rejected
every `map.*` member via `builtin.starts_with("map.")`. This change
narrows the rejection: `map.*` members stay disqualifying **except**
the two members certified as pure (below). A new predicate,
`is_certified_pure_map_member`, names the certified set exactly:

- `map.mapToScreen`
- `map.screenToMap`

The list is consulted by both S1 call sites (the loop-barrier check
`call_is_barrier` and the B3/B3b helper-body safety closure), so the
certification applies inside chunk-defined helper bodies exactly as
in loop bodies. The LICM copy of the list (`loop_invariant`) is
unchanged: this is an S1-side relaxation only. `server.*` and
`matrix.*` are not certified and remain untouched by this change.

## Certification basis (user decision, 2026-10-31)

Unlike the relaxations provable from program structure alone,
`mapToScreen`'s purity rests on host-implementation knowledge: the
map display state does not change during a single loop execution.
The user certified it in chat on 2026-10-31:

> 「mapToScreenはただの座標変換で、同じ入力には同じ出力を返します。」
> ("mapToScreen is just a coordinate transform; the same inputs
> give the same outputs.")

`map.mapToScreen` is certified on the user's statement.
`map.screenToMap` — the inverse transform, two occurrences in the
cycle-85 census — is included on the same grounds, as recorded by
the parent at decision time.

## Why the relaxation is sound

- S1 never moves, adds, or removes a call. The only code it inserts
  is the preheader: a pure field read `B.K`, total under the
  existing grounds (a builtin root is a host table installed before
  the script runs; a plain-table lookup cannot error and mutates
  nothing). The certified calls keep their call sites, arguments,
  order, and counts.
- The certified calls themselves mutate no Lua-visible state and
  return the same outputs for the same inputs within a loop (the
  certification premise), so their presence in a loop body cannot
  change what a cached field denotes.
- Every other mechanism that could change what a cached field
  denotes stays closed: the root-write scan (`map` written anywhere
  still disqualifies — covered by a dedicated unit test), the
  field-write gate, unknown calls, host-table arguments, and method
  calls.
- Writes and side effects are unaffected in the other direction
  too: no `map.*` member other than the two certified ones is
  exempted, and the exemption removes a barrier only — it never
  introduces a new transform.

Re-evaluation condition: if the host's map semantics change (the
certified members gain observable state or input-dependent behavior
beyond a pure coordinate transform), this certification must be
revisited. Semantic reference dialect: Lua 5.3 (Stormworks).

## Census reconciliation (cycle 85 vs measured)

Cycle 85 measured, on the singles-only opportunity set at the
then-tip, 39 loops / 36 files disqualified by `map.*` alone, of
which 34 loops had non-S1 winners (`map.mapToScreen` bystander 30,
candidate itself 9; `map.screenToMap` 2). The transition measurement
below uses the full target-mode population at the current tip
(round 196, after B1/B3/global-alias chase/B3b landed), so a larger
realized count is expected and consistent: 63 files transition at
slack128 (46 at slack32, a subset), 37 of them newly reaching
S1-family winners at slack128 (32 at slack32). Every one of the 63
transitioned files contains a `map.mapToScreen` or
`map.screenToMap` call.

## Verification (base `804aa1f07` = round 196, head = this change)

- Unit tests: module tests 75 pass (5 new: bystander firing for
  both certified members, uncertified `map.*` still disqualifying,
  write through the `map` root still disqualifying, certified call
  inside a B3-eligible helper body). Workspace: all suites pass
  (1,447 tests total across the workspace run; the only clippy
  warning is the pre-existing `literal_folding` one, identical to
  base).
- Tier FULL (no-target corpus, 16,620 rows): base and head
  byte-identical, SHA-256
  `1e5ff998e98e67edd08ff3ff579ae65f9646a2bca3d7f959584509996df53f2a`
  on both sides.
- Target-delta: slack32 — 46 transitions (32 new-to-S1, 14 S1→S1),
  +532 chars; slack128 — 63 transitions (37 new-to-S1, 26 S1→S1),
  +915 chars. lost-S1 0 at both slacks, free-side differences 0,
  no other transitions.
- Differential execution over all 109 transition cases (63 files ×
  slacks): raw tracecheck harness (stub-less microcontroller VM)
  gave 34 exact matches and 75 trace differences, all triaged:
  - 58 agree across plain/base/head after normalizing line numbers
    (error-message and traceback line attribution only);
  - 17 are an artifact of that harness's environment, which does
    not install a `map` global: the head winner's preheader reads
    `map.<member>` unconditionally, while the base/plain winners
    never execute the read inside zero-trip loops, so only the
    head side reports "attempt to index a nil value (global
    'map')". The real host always installs the `map` table, and
    the project's canonical differential harness (diffexec)
    installs a `map` stub; all 17 cases were re-run under diffexec
    and match exactly across plain/base/head.
  Semantic differences: 0.
- .us2 spot check (MEASURE.lock, slack32 transitions): 7 of 46
  files pass the vmbench smoke on both sides (39 excluded
  symmetrically — the vmbench environment likewise lacks `map`,
  so both winners error); tick totals over the 7 measured files
  54.826 µs → 52.698 µs (−3.88%), per-file paired differences
  mixed at noise/tradeoff scale (largest paired worsening +0.45 µs
  on a 3.78 µs file). As with cycles 89/92, the effect's character
  is S1 coverage expansion under target budgets rather than a
  per-loop speed guarantee.

## Carryover

- input.* / property.* barrier treatment (host-semantics
  certifications recorded 2026-10-31: input values constant within
  a tick; property values constant during script execution) is the
  next design material on this axis; it starts from a census and
  was not started in this cycle.
