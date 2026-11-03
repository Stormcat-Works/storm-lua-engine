# S1 alias resolution: definition order and chain order (cycle 94) — verification record

Codex adversarial review run #2, finding 1 (reproduced by w9 cycle
54), plus its chain form found by w6 cycle 47 (suite v14,
`v14-ga-chain-reversed` and probe cases).

## What changed

Two implementation commits in
`crates/storm-lua-minify/src/passes/single_member_cache.rs`:

- `3844bef` — `alias_table_root` returns the resolved table plus a
  **dominates** flag, and only hoisting callers (the preheader
  candidate path) may use a resolution when the flag holds.
- `9f9d3ba` — a chain-order verification (`alias_chain_ordered`)
  conjoined into `alias_table_root` itself, so an order-unsound
  resolution is refused for every caller.

Regression tests: three in the first commit (defined-after-loop,
defined-before-loop control, defined-after-enclosing-function
conservative case) and four in the second (chain reversed,
mid-chain reversed, top-level-loop chain reversed, ordered-chain
control).

## Root causes

**(a) Definition after the loop (Codex form).** The chunk-wide
alias rule resolved a global alias from its single definition
anywhere in the chunk, with no positional relation to the loop.
S1 then hoisted a member read (`S.drawLine`) into the loop
preheader. When the definition follows the loop — a chunk-level
loop, or a loop in a function that can be called before the
definition executes — the preheader read runs while the alias is
still nil and errors, where the original program reads the member
only after the definition (or never, in a zero-trip loop).

**(b) Chain link order (w6 form).** Alias resolution follows name
chains (`B = A`, `A = screen`) through the effect analyzer's alias
map. That map is the fixpoint of final values and is
order-insensitive: with `B = A` written **before** `A = screen`,
`B` captured the pre-write value of `A` (nil), yet the chain still
resolved `B` to `screen`. Checking only the final alias's
dominance over the loop cannot see this — every statement precedes
the loop, and the resolution is still wrong. The same flaw applies
to the middle link of a three-link chain and to chains whose
capture sits in the loop's own block (top-level loops).

## The fixes and why they are sound

**(a) Dominance for hoisting.** For a chunk-top definition,
`dominates` holds exactly when the defining statement precedes
the chunk statement containing the loop and the chunk block
contains no goto/label outside functions (a goto could skip the
definition). When it fails, resolution falls through to the
cycle-65 same-block arm instead of returning. The local-decl arm
(lexical scope) and the cycle-65 arm (same-block, preceding, with
its own call/write and goto checks) dominate by construction.
Barrier-classification callers ignore the flag and stay
chunk-wide conservative.

Deliberate conservative case, recorded: a loop inside a function
whose alias definition sits after the enclosing function statement
is rejected for hoisting, although at callback time the alias
would normally be defined — the same shape is unsound when the
function is called during chunk load before the definition, and
distinguishing the two needs a call-site proof S1 does not have.

**(b) Per-link order.** `alias_chain_ordered` walks the resolved
value's name chain and requires each link's defining write to
precede the statement that captures it: chunk-statement order for
chunk definitions (void when the chunk contains a goto/label, so
no order is claimed), same-block index order within the loop's
block, and — from positions inside a function — a chunk
definition preceding the loop's chunk statement. A name with no
writes is the host builtin itself and terminates the chain. Any
step that cannot be proven (multiple writes, a definition in an
unrelated block, a local declared in an incomparable position)
fails the check, and `alias_table_root` then resolves nothing —
which is also the correct outcome for classification callers,
since in a reversed chain the alias denotes the pre-write value,
not the builtin table.

## Verification (base `2f906671b` = round 197; cumulative over the
cycle-94 stack, head `12ea749`)

- Differential execution (canonical diffexec, plain/opt/safe +
  target32/target128/abs8192):
  - Cycle-54 case f01: at base, all three target configurations
    error (vm-error); at head, all six configurations agree.
  - w6 cycle-47 cases at head: `v14-ga-chain-reversed`,
    `p-chain-mid-reversed`, and `p-chain-toplevel` agree across
    all six configurations (at the pre-extension head they
    diverged in the target configurations); `p-chain-link-after`,
    `v14-ga-chain-ok`, and `v14-ga-control-before` agree, and the
    generated code for `v14-ga-chain-ok` still contains the S1
    cache local (the ordered chain still fires — positive
    control), while the reversed case's output contains no
    hoisted read.
- Tier FULL (no-target corpus, 16,620 rows, cumulative for the
  whole cycle-94 stack): byte-identical to base in all four
  conditions (identical 4,155 per condition; status pairs
  (ok, ok) 4,128 / (error, error) 27; no flips).
- Target-delta: at both slack32 and slack128, one transition
  (file index 911, S1→S1, −10 chars), lost-S1 0, free-side
  differences 0 — no corpus winner was relying on an order-unsound
  resolution.
- Workspace tests: 1,459 pass, 0 fail (single_member_cache module
  83 tests, including the seven new ones); fmt clean; no new
  clippy warnings in the changed crates.

## Re-evaluation conditions

- The conservative rejection in (a) can be revisited only with a
  call-site proof that the enclosing function cannot run before
  the definition.
- If the effect analyzer's alias map ever becomes flow-sensitive
  (recording per-position values), the consumer-side chain check
  in (b) becomes redundant for chains it proves — but it should
  be removed only after the analyzer's order guarantee is itself
  verified, not assumed.
- Related consumers that chase aliases independently of
  `alias_table_root` (for example the barrier classification's
  builtin chase) were not implicated by the reproduced cases;
  if a future hunt reproduces a chain-order divergence there, it
  needs the same per-link treatment.

Semantic reference dialect: Lua 5.3 (Stormworks).
