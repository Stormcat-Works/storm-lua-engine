# Member-cache key identity: numeric keys compare by value (cycle 94) — verification record

Codex adversarial review run #2, finding 3 (reproduced by w9 cycle
54 at both the reviewed tip and the then-integration tip).

## What changed

One implementation commit (`52acb36`): `key_text` in
`crates/storm-lua-minify/src/passes/loop_member_reads.rs` now
canonicalizes numeric member keys **by value** instead of by
literal spelling. `crates/storm-lua-minify/src/passes/straightline_member_reads.rs`
shares the same key computation, so both member-read caches (MR
and SMC) and both loop and straightline forms are covered.
Regression tests for the MR straightline/loop forms and the SMC
split forms are in the same commit.

## Root cause

Lua table keys compare by value: `t[1]` and `t[1.0]` are the same
slot (in Lua 5.3, a float key with an integral value is the integer
key). The caches keyed read/write groups by the literal text of the
key, so a write spelled `t[1.0] = 3` did not close the group of a
cached read spelled `t[1]`, and the loop (or straightline segment)
kept returning the pre-write cached value. Codex's case: expected
(9, 3), produced (9, 9); loop form expected 30, produced 54.

## The fix and why it is sound

`key_text` returns a canonical key string:

- Integer literals and integral float literals map to `i:<i64>`,
  so `1`, `1.0`, and `1e0` share one key — exactly Lua's value
  identity for those keys.
- The fold is bounded strictly below 2^63: `i64::MAX as f64`
  rounds to 2^63, which has no Lua integer conversion, so integral
  floats at or above that bound are not folded to an integer key.
- Non-integral floats map to their bit pattern (`f:<bits>`), so
  distinct floats never merge and equal floats always do.
- A NaN literal yields no key (unresolved), which keeps the
  conservative path: no group identity is claimed for it.

String keys keep the existing spelling-based identity, which is
value identity for strings.

Scope note: the `0x1` sub-variant from the Codex report did **not**
reproduce in the project harness (w9 cycle 54): hex integer
literals are already absorbed by the parser into integer values
before this layer, so there was nothing to fix for that spelling;
this is recorded rather than claimed.

## Verification (base `2f906671b` = round 197; cumulative over the
cycle-94 stack, head `12ea749`)

- Differential execution (canonical diffexec, plain/opt/safe +
  target32/target128/abs8192): the cycle-54 cases f03, f03c, and
  f03loop reproduce the wrong trace at base in the target
  configurations and agree across all six configurations at head.
  f03b (the `0x1` form) already agrees at base, consistent with
  the non-reproduction note above.
- Tier FULL (no-target corpus, 16,620 rows, cumulative for the
  whole cycle-94 stack): byte-identical to base in all four
  conditions (identical 4,155 per condition; status pairs
  (ok, ok) 4,128 / (error, error) 27; no flips).
- Target-delta: at both slack32 and slack128, one transition
  (file index 911, S1→S1, −10 chars), lost-S1 0, free-side
  differences 0 — the fix changes no corpus winner beyond that
  single S1-shape change, whose file is a proven pairs-order
  self-unstable case (see the cycle-94 round entry).
- Workspace tests: 1,459 pass, 0 fail; fmt clean; no new clippy
  warnings in the changed crates.

## Re-evaluation conditions

- If the key layer ever handles additional literal forms (for
  example, floats spelled with hexadecimal mantissas), the
  canonicalization must be extended by value, not by spelling.
- The 2^63 bound follows Lua 5.3's float→integer conversion rule;
  if the dialect basis changes, re-derive the bound.

Semantic reference dialect: Lua 5.3 (Stormworks).
