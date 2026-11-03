# Verification: immutable-table flattening dropped nil / NaN constructor keys

Date: 2026-10-30
Status: complete
Change: `key_is_valid_table_key` guard in `table_literal_pure`
(`crates/storm-lua-minify/src/passes/tables.rs`). A table constructor
literal now counts as a pure, flattenable literal only when every
computed (`[key] = value`) field key is provably a valid table key:
a static key form (string / integer / non-NaN number literal) or a
boolean literal. Other keys (nil, `0 / 0` expressions, names) can be
nil or NaN at runtime, and inserting a nil or NaN key raises
`table index is nil` / `table index is NaN` when the constructor
evaluates, so the construction is effectful and must not be removed.
No validation was weakened.

## Defect (bug #21)

`immutable-table-flattening` collects local table constructors whose
initializer passes `table_literal_pure`, folds static reads of the
table, and removes the binding. The purity check examined key and
value expressions only for expression-level side effects; it did not
model that the constructor itself raises for nil / NaN keys. When all
reads of the table were static, the constructor — and its runtime
error — disappeared:

```lua
function onTick()
  local t = { [nil] = 1, b = 2 }
  output.setBool(1, t.b == 2)
end
```

Plain compiles and raises `table index is nil` at runtime; the safe
and smallest outputs reduced to `output.setBool(1, 1 > 0)`. Affected
forms reproduced before the fix: literal `[nil]` and `[0 / 0]` keys
(both output modes), a local bound to `0 / 0` or `nil` used as the
key (smallest, via single-use sinking / propagation), and a `nil`
argument specialized into the constructor. Unaffected forms checked:
statement writes `t[nil] = 1` / `t[0 / 0] = 1` (errors kept), reads
with NaN keys (nil, no error, unchanged), infinite keys (valid, kept
working), and tables that escape opaquely (never flattened).

## Regression coverage

Unit tests in `passes::tables::scalarization_tests`:
`immutable_table_with_nil_key_is_not_flattened`,
`immutable_table_with_nan_key_is_not_flattened`,
`immutable_table_with_name_key_is_not_flattened` (pins the
conservative scope), and
`immutable_table_with_bool_key_is_still_flattened` (boolean keys
stay flattenable). The nil / NaN tests fail if the guard is removed
(verified both directions). Differential probes: boundary suite v8
(table-key value classes: NaN / -0.0 / integral floats / huge and
infinite keys / nil / booleans / numeric-vs-string keys /
constructors) reports 20/20 configuration matches after the fix,
versus one exec-status divergence before it, and all hand probes for
the affected/unaffected forms above agree across plain/safe/opt
after the fix.
