# Verification: immutable-table flattening dropped the binding for non-literal scalar values

Date: 2026-10-30
Status: complete
Change: the use scan in `flatten_immutable_tables`
(`crates/storm-lua-minify/src/passes/tables.rs`) now marks a table
non-flattenable when a static read chain cannot be substituted by
`rewrite_static_table_chains` (new helper
`static_chain_substitutable`, mirroring the rewrite walk). The
previous scan rule only rejected table/function values in
non-index positions; it assumed every other static read is
substituted. The unused `static_table_chain_value` helper was
removed. No validation was weakened.

## Defect (bug #22)

The rewrite substitutes a static read only when the walk ends at a
scalar literal (`Num` / `Str` / `Bool` / `Nil`, copied into the read
site) or misses at the final key (replaced by nil). A field whose
value is a compound pure expression is left in place — and the
constant folder materializes every finite compound value before
flattening runs, EXCEPT non-finite ones, whose materialization it
refuses by design. So a constructor field holding `0 / 0` or
`1 / 0`, when read, survived the rewrite while the scan still
allowed the binding to be removed:

```lua
function onTick()
  local t = { a = 0 / 0, b = 2 }
  output.setBool(1, t.a ~= t.a)  -- plain: true; compiled: vm-error
  output.setBool(2, t.b == 2)
end
```

The compiled output folded `t.b`, removed the local, and left
`t.a` reading a nil global, raising "attempt to index a nil value"
at runtime where plain succeeds (safe and smallest, both). The
same shape with a missing intermediate key (`t.inner.b` with no
`inner` field) was left unsubstituted by the rewrite for the same
reason and is covered by the same check. Unaffected, verified:
the NaN/inf field when it is never read (dropping stays allowed),
closed-namespace globals with NaN values (a different code path,
already correct), statement writes with nil/NaN keys, and all
Round 175 key-side cases (boundary suite v8 re-checked 20/20).

## Regression coverage

Unit tests in `passes::tables::scalarization_tests`:
`immutable_table_with_nan_value_read_is_not_flattened`,
`immutable_table_with_inf_value_read_is_not_flattened`,
`immutable_table_with_missing_intermediate_key_is_not_flattened`,
and `immutable_table_with_nan_value_unread_is_still_flattened`
(the precision control). The three "not flattened" tests fail if
the scan check is disabled (verified both directions).
Differential probes: boundary suite v9 (neighborhood of the
Round 175 fix: closed-namespace scalarization with erroring keys,
nested constructors, NaN value vs NaN key, uniform fill,
multi-value constructor tails) reports 15/15 configuration matches
after the fix, versus two exec-status divergences before it
(v9-nan-value-flat, v9-nested-nan-value).
