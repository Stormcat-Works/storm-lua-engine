# Verification: immutable-table flattening dropped raising field values

Date: 2026-10-30
Status: complete
Change: in `table_literal_pure`
(`crates/storm-lua-minify/src/passes/tables.rs`), compound
expressions (`Bin` / `Un`) no longer count as pure constructor
values. A constructor literal is flattenable only when every field
value is a scalar literal or a recursively pure nested table.
No validation was weakened.

## Defect (bug #23)

`table_literal_pure` treated a binary/unary expression over
literals as pure. But evaluating such an expression can raise, and
evaluating the field values is part of evaluating the constructor:
arithmetic or concatenation on nil, integer division or modulo by
zero, arithmetic on a non-numeric string, length of nil, unary
minus on nil, comparing nil with a number, and so on. When all
reads of the table were static, flattening removed the constructor
and the error with it:

```lua
function onTick()
  local t = { a = 1 + nil, b = 2 }
  output.setBool(1, t.b == 2)
end
```

Plain raises a vm-error at the constructor; the safe and smallest
outputs returned normally. Reproduced for Name fields, array
fields, and static-string-keyed fields, with `1 + nil`, `1 // 0`,
`5 % 0`, `"x" + 1`, `"x" .. nil`, `#nil`, `-nil`, and `nil < 1`
values (all ten probes diverged before the fix, in both output
modes; all agree after it). Compound values the constant folder
can prove finite are materialized into literals before flattening
runs in the pipeline, so a surviving `Bin`/`Un` value is exactly
one whose evaluation is not provably safe here (raising, or
non-finite like `0 / 0` — legal as a value, but no longer dropped;
see the test note below). The sibling scalarizers
(closed-namespace, table-parameter, uniform-fill) were probed for
the same class and agree across configurations.

## Regression coverage

Unit tests in `passes::tables::scalarization_tests`:
`immutable_table_with_raising_value_is_not_flattened` (`1 + nil`),
`immutable_table_with_int_div_zero_value_is_not_flattened`, and
the updated `immutable_table_with_nan_value_unread_is_not_flattened`
(the cycle-37 precision control inverted: an unread compound value
now also keeps the table, because compound values are not
provably non-raising). All three fail if the compound-value rule
is reverted (verified both directions). Boundary suites v8
(table keys) and v9 (scalarization neighborhood) re-checked after
the fix: 20/20 and 15/15 configuration matches.
