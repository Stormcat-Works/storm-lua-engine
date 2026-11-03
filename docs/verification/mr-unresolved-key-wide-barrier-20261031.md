# Member caches: unresolved-key writes are base-wide barriers (cycle 94) — verification record

Codex adversarial review run #2, finding 2 (reproduced by w9 cycle
54 at both the reviewed tip and the then-integration tip).

## What changed

One implementation commit (`5c97c96`):
`crates/storm-lua-minify/src/passes/loop_member_reads.rs`.
`record_write` in the loop member-read scan now takes the scan
state and, for a write whose key cannot be resolved to a constant
key, records the written **base** in a new `wide_writes` set. A
base in `wide_writes` closes every read group of that base — the
write is treated as potentially hitting any key. Method-name
write targets (`t:m(...)` shapes encountered as write targets) are
unified into the `s:` key namespace as a robustness measure; that
path was already closed by the base proof, so behavior there is
unchanged. Regression tests are in the same commit.

## Root cause

In Codex's case, a loop writes `t[i] = i` with a dynamic key and
later reads a constant key of `t`. The scan recorded writes per
resolved key; a write with an unresolvable key was recorded
nowhere, so no read group was closed and the cached read of the
constant key survived across an iteration that may have written
exactly that key. Expected total 22, produced 54.

## Why the fix is sound

The change only widens invalidation. A write is now classified as
either (a) a resolved constant key, closing exactly that key's
group, or (b) anything else, closing all groups of the written
base. Case (b) is the conservative completion of the scan's
contract: an unresolved key can alias any group of the base, so
keeping any of them open would be unsound. No read is cached
across a write it could observe, and no new caching is introduced
anywhere.

## Verification (base `2f906671b` = round 197; cumulative over the
cycle-94 stack, head `12ea749`)

- Differential execution (canonical diffexec, plain/opt/safe +
  target32/target128/abs8192): the cycle-54 case f02 reproduces
  the wrong trace at base in all three target configurations and
  agrees across all six configurations at head.
- Tier FULL (no-target corpus, 16,620 rows, cumulative for the
  whole cycle-94 stack): byte-identical to base in all four
  conditions (identical 4,155 per condition; status pairs
  (ok, ok) 4,128 / (error, error) 27; no flips).
- Target-delta: at both slack32 and slack128, one transition
  (file index 911, S1→S1, −10 chars), lost-S1 0, free-side
  differences 0.
- Workspace tests: 1,459 pass, 0 fail; fmt clean; no new clippy
  warnings in the changed crates.

## Re-evaluation conditions

- If key resolution ever becomes flow-sensitive (for example,
  proving a dynamic key equals a specific constant on some path),
  the wide barrier can be narrowed per proof — but only with the
  proof carried at the write site; spelling- or name-based
  narrowing is not sufficient.
- Any new write form routed into the scan must enter through
  `record_write`, so unresolved forms fall into case (b) by
  construction.

Semantic reference dialect: Lua 5.3 (Stormworks).
