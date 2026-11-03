# Verification: mapping-aware print primitive (w7 cycle 104, Round 285, Tier INVARIANT)

Base: integration tip `92721fe` (docs) / FULL-VERIFY-TIP `0615e05`
(round 280). Implementation: `feat(minify): mapping-aware print
primitive for deferred rename (w7 cycle 104, task 1)` (branch
`muse/w7-c104`, 2 files, +200/-6, taken verbatim from the cycle-103
implementation, patch-identical to `c103/c103-impl.patch`).

## What it is

`print_with_rename_mapping` (`scope_rename.rs`): a measurement-only
primitive that prints a scope-renamed rendering of a tree WITHOUT
materializing the rename — it computes the rename plan on the input
tree and prints with the plan as a spelling substitution
(mapping-aware print, Route A from cycle 102/103). `Printer` gains an
optional `SizeNameResolver` (`print.rs`); local-merge decisions in
`block()` compare post-substitution spellings when a resolver is
present. The function carries `#[allow(dead_code)]` until the deferred
two-phase design's caller lands (cycle 104, task 2); it is covered by
its unit test (`mapping_aware_print_tests`, 7 snippets x 2 newline
modes) and by the corpus-wide print-equivalence gate below.

## Stock-path invariance (construction argument)

- `Printer::new` sets `name_resolver: None`; the `name()` resolver
  branch and the `block()` mapped merge branch execute only when a
  resolver is present. With `None`, both fall through to the original
  code verbatim (the `None` arm of the `block()` match is the original
  `group_names` / `overlap` / `rhs_mentions` code, unchanged).
- `scope_rename.rs` gains only new code: +77/-0. No existing function
  is modified there.
- `print.rs` deletions (6) are the replaced lines inside the `block()`
  merge branch, guarded by the same `None`/`Some` match; the `None`
  path is byte-identical to stock by inspection.
- `rustfmt --check` clean on both touched files (pre-existing fmt drift
  in 9 unrelated pass files at the tip is untouched).

## Gates (all on the merged tree, 開発ホスト1, nice)

- Unit tests: `storm-lua-minify --lib` 1,111 passed, 0 failed;
  `storm-lua-syntax --lib` 144 passed, 0 failed (each run twice for
  stability; one transient syntax-lib failure on the first run after
  the sccache TMPDIR restoration did not reproduce).
- FULL production gate: corpus 4,155 files x 4 configs = 16,620 rows,
  0 fallbacks, sorted rows byte-identical to the same-tip stock
  baseline (`c102/full-rows.tsv`, SHA-256
  `b3c4b90c8aaa9e0ca5131e2eebc7b1765fa3157218385a9fd34a58b2e912674a`),
  status ok 16,512 / error 108 (4,128 ok / 27 error per config).
- Gate (a) re-confirmation (probe-only hook in `scope_rename_fast`,
  never merged): FULL run with `C103_MAPPRINT=1`, 16,620 rows,
  193,000+ rename comparisons, **0 MISMATCH lines**; probe rows
  byte-identical to the same baseline. The mapping-aware print equals
  stock print of the renamed tree wherever the pipeline renames, on
  the FULL corpus, on the merged tree.

## Tier

Tier INVARIANT: output-invariant by construction (argument above) +
targeted tests + FULL-equivalent gates byte-identical. The next Tier
FULL round runs its corpus comparison cumulatively from
FULL-VERIFY-TIP and updates the marker; this round does not move
FULL-VERIFY-TIP (still `0615e05`).

## Rebase note (round renumbering)

w9 cycle 69 merged first (rounds 281–284: candidates AF / AE,
link L1–L3, dialect M1/M2), advancing the integration tip to
`a0fc65d` and FULL-VERIFY-TIP to `d97f193`. This round is therefore
**285**, and this branch was rebased onto `a0fc65d` (conflict-free:
the w9 changes touch `link.rs` / `dialect.rs` / `numeric_literals.rs`
/ `immutable_synthesis.rs`, disjoint from this round's files).
Post-rebase re-verification on the rebased tree:

- Unit tests: `storm-lua-minify --lib` 1,119 passed, 0 failed
  (1,111 + 8 regression tests from w9 cycle 69); `storm-lua-syntax
  --lib` 144 passed, 0 failed.
- FULL production gate vs the NEW tip: corpuscheck built at `a0fc65d`
  (baseline) and at the rebased head; sorted rows byte-identical,
  SHA-256 `9713a05d5ab6f9d497510fa48a8aafc9693a13742cab8e1f98f1bfd0fe4b8548`,
  16,620 rows, 0 fallbacks.
- Gate (a) re-confirmation on the rebased tree (probe hook, never
  merged): FULL run, 194,000+ rename comparisons, 0 MISMATCH lines;
  probe rows byte-identical to the new-tip baseline.
