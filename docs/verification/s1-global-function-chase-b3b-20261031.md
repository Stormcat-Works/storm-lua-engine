# S1 barrier resolution through chunk-defined global functions — B3b relaxation (cycle 92) — verification record

## What changed

One file: `crates/storm-lua-minify/src/passes/single_member_cache.rs`
(implementation commit on top of the cycle-86 strict form; unit
tests updated/added in the same commit).

Cycle 86 (candidate B3, strict form) admitted a call to a
chunk-defined global function as a known call when the callee's
identity was fixed by *single definition plus dominance*: exactly
one recorded write, a plain-name `Funcstat` that is a direct
statement of the chunk block, positioned before the loop's
top-level statement, in a chunk block free of goto/label. In the
cycle-86 census the dominance condition (D3) dropped 250 of the
575 identity-passing call sites (44%) — the corpus's typical
layout defines helper functions *after* the callbacks that use
them.

This change (B3b, designed and measured in cycle 86, approved by
the user on 2026-10-31) relaxes identity to:

- the chunk passes the dynamic-global gate shared with
  `single_use_forwarding` / `closed_fields` / `global_stores`
  (no `_ENV` reference, no call spelled `rawget` / `rawset` /
  `load` / `loadstring` anywhere), so the resolver's write
  counts cover every write to a global binding;
- exactly one recorded write, and that write is a plain-name
  `Funcstat`, **in any position** (a direct chunk statement, a
  nested block, or a function body).

The direct-statement requirement, the dominance (position)
requirement, and the chunk-level goto/label condition are all
dropped from `b3_identity_ok`. The body-safety closure (the
cycle-86 S1–S6 predicate over the transitive chunk-defined call
set) is unchanged, as are the argument rules for calls in the
loop body and in closure bodies. Assign-form definitions
(`g = function() end`) remain out of scope: they record no
`Funcstat` write.

## Why dropping dominance is sound: call-error equivalence

The strict form proved "when the loop runs, the binding denotes
that one function body" from execution order. B3b argues about
the *set of bodies that can run*, which does not need order:

- The binding has exactly one write anywhere in the chunk, and
  the dynamic-global gate makes that write set complete (no
  dynamic writes through `_ENV` / `rawset` / `load`). So at any
  call to the callee, either the single definition has executed
  — then the body that runs is that definition's body, the one
  the closure check has vetted — or it has not executed, in
  which case the binding is still nil and the call itself errors
  immediately, before any body runs.
- The S1 transform never moves, adds, or removes a call. The
  only code it inserts is the preheader: a pure read of a
  root-table field, total under the existing grounds (it cannot
  error and mutates nothing). The nil-call error therefore
  occurs identically — same call site, same error — before and
  after the transform. If a `pcall`-style wrapper catches the
  error, execution continues with the cached value unchanged,
  again identically.
- A chunk-level goto that re-executes the definition only
  creates a fresh closure of the *same* body, so the body set is
  unchanged. Recursion, and loops inside the callee's own body,
  likewise keep the same body set.

The semantic baseline is Lua 5.3 (the target dialect, confirmed
by the user on 2026-10-31): calling a nil value raises an error
immediately, and error propagation through `pcall` is as above.

**Re-evaluation condition.** This relaxation rests on
call-error equivalence — on the VM's error semantics (a call of
a nil value errors immediately, with no other observable
difference) rather than on an ordering fact. If the VM's error
semantics change (how nil calls fail, how errors are caught or
observed), this relaxation must be re-examined before it is
relied on further.

## Reach

Cycle-86 census (same predicate shape as implemented here):
relaxed identity passes 590 of 611 call sites; **66 loops /
59 files released (49 of them non-s1 winners)**, versus 36
loops / 33 files for the strict form.

## Verification (cycle 92; base `e6ba6b6` = Round 194, head
rebased onto `6b7d15d94` = Round 195; cumulative)

- Unit tests: the two strict-form rejection tests (definition
  after the loop, definition nested in a block) are now firing
  assertions; two B3b tests added (definition in a function body
  after the loop statement; chunk-level goto/label with the
  definition after the loop). All other B3 negatives (reassigned,
  defined twice, stateful call in body, unknown global, root
  passed as argument, root-field write, builtin-name write,
  goto in body, function value in a non-definition position,
  method call, dynamic chunk) still reject. Module: 70 tests
  pass.
- Workspace tests: 1,438 passed / 0 failed before the rebase;
  **1,442 passed / 0 failed** on the rebased head (the +4 are
  Round 195's own tests). `cargo fmt --check` clean; clippy
  shows only the one pre-existing warning in
  `literal_folding.rs`, unchanged from base.
- Tier FULL (no-target corpus, 4,155 files × 4 conditions):
  byte-identical to base — both sides 16,620 rows, SHA-256
  `1e5ff998e98e67edd08ff3ff579ae65f9646a2bca3d7f959584509996df53f2a`.
  (The B3 gate is consulted only for target-mode layouts with a
  disqualified loop, so no-target output cannot change; Round
  195 in the cumulative range is generation-invariant by its
  own Tier FULL.)
- Target-delta (winner comparison at target = no-target winner
  size + slack): slack32 **23 transitions** (+250 chars),
  slack128 **32 transitions** (+555 chars). All transitions are
  into the s1 family (newly-s1 20 / 29, s1→s1 3 / 3);
  **lost-s1 0, free-side differences 0, other transitions 0**.
- Differential execution over all 55 transition cases
  (plain / base winner / head winner, each traced twice):
  46 MATCH; 9 DIFF, all classified in-cycle as error-text-only
  differences — the same runtime error with line numbers,
  renamed identifiers, or global-vs-upvalue traceback
  attribution differing (idx 1330 at both slacks: identical
  `drawTextBox: missing argument 6` error on all three sides;
  the remaining 7 cases have base == head and differ only
  against plain, i.e. pre-existing target-mode text, not this
  change). **Semantic differences: 0.** SELF-UNSTABLE 0,
  COMPILE-FAIL 0.
- .us2 execution spot check (MEASURE.lock, slack32 transitions,
  11 interleaved pairs): 12 of 23 files excluded at smoke (the
  harness-environment error class, both sides alike); over the
  11 measured files the tick total moved 90.456 → 91.690 µs
  (+1.36%), with per-file paired differences at noise /
  phase-tradeoff level (largest single worsening +0.44 µs on a
  1.39 µs draw). No material regression; as with cycle 89, the
  effect of this change is s1 coverage expansion under target
  budgets rather than per-loop speed.

## Scope notes

- map.* purity certification (user-approved 2026-10-31) is not
  part of this change; it is a separate follow-up cycle.
- The cycle-89 global-alias chase keeps its own dominance
  conditions: it chases the *value* (the cached read indexes the
  alias itself), where the nil case is not error-equivalent.
  B3b's argument applies only to callee identity for calls.
