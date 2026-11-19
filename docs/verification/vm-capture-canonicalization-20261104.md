# VM capture canonicalization — 2026-11-04 campaign

Base: `5d52ef8c072017505a846af4ebf94b2b24760df2` (`muse/vm-snapshot`).
Implementation branch: `muse/vm-capture-canonicalization`.
The existing [snapshot](vm-snapshot-20261104.md),
[Addon bridge](vm-snapshot-addon-bridge-20261104.md) and
[host capture paths](vm-snapshot-hostpath-hook-20261104.md) contracts still apply.

## Guarantee and boundary

Capture produces identical bytes for independently constructed equivalent heaps
with the same profile, registered path set, live host alias relationships,
prototype/debug bytes, scalar bit patterns and object/cell sharing, provided
identity-key ordering does not encounter the unresolved ties described below.
Insertion order, allocation addresses, Lua hash seeds and host registration order
do not determine bytes within that domain. The fresh-process pin also changes
construction order between parent and child.

This is not general graph-isomorphism canonical labeling. Lua table and function
identities have no intrinsic value order. Two or more still-unreached,
unregistered keys of the same identity type in one deferred table batch retain
Lua's raw encounter order. This is a named residue, not a canonical ordering
claim. Distinct values or sharing behind these keys can make both recapture
after restore and independently constructed heaps differ in bytes.
The residue can propagate into subsequent graph IDs, prototype IDs and cell IDs.
A graph with one such key per identity type/batch has no same-type tie there;
other batches can still introduce ties. Arbitrary unanchored keys remain
supported for semantic restore.

## Key order and traversal

The type order is **boolean, number, byte string, table, function**. The following
rules apply to ordinary tables, the environment root, and traversed host tables.

| Key class | Order / disposition |
| --- | --- |
| Boolean | `false`, then `true`. |
| Number | Ascending numeric value, including infinities; integers and floats share this class. Integer/float comparisons avoid converting large integers to rounded f64 first. Lua normalizes representable integral float keys, including signed zero, to integers; NaN cannot be a Lua table key. Values retain the existing exact integer/f64-bit encoding. |
| String | Unsigned lexicographic byte order, including embedded zeroes and non-UTF-8 bytes. No locale or display formatting. |
| Registered host table/function | Within its respective identity type, before ordinary graph anchors; ordered by the selected original symbol's index in the sorted path sequence. |
| Already-reached ordinary table/Lua closure | Within its respective identity type, after host anchors and before unreached keys; ordered by its graph ID, assigned through the traversal below. This is independent of addresses when earlier traversal has no unresolved tie. |
| Still-unreached ordinary table/Lua closure | After anchors, stable first-encounter order within each identity type. The unresolved-tie residue above applies. No pointer, allocation timestamp, dump hash, or invented structural order is called canonical. |
| Thread, full/light userdata | Unsupported, including in key positions; existing typed rejection is retained. |
| Table with a metatable, real globals table, unregistered C function | Existing typed rejection is retained, including key positions. The host-table `HostEntry` exception is only for values. |
| Nil / NaN | Lua itself refuses these as table keys, so no successful-capture key ordering exists for them. Other unsupported mlua value variants also remain errors. |

Capture collects raw pairs without converting them, partitions scalar and
identity keys, and sorts scalar keys **before** converting keys/values and
assigning IDs. Tables use a FIFO worklist. Closure upvalue cells use a separate
FIFO worklist, with each closure's upvalues visited by numerical slot. All
pending scalar table work and upvalue work is exhausted before sorting an
identity-entry batch. Thus a key named through a later sibling table or an
upvalue already has a stable reach anchor. Identity batches are visited in table
encounter order; each batch is sorted using the anchors available at its start.
Work newly discovered through one batch is drained before the next batch.
Converted entries are appended to their table definition in the defined type
order. Capture does not run script callbacks or mutate the Lua namespace.

## Symbol sequence and aliases

Every capture sorts and deduplicates the complete registered path set by
lexicographic vectors of UTF-8 segments before live resolution. Scalar-resolving
paths remain excluded from the symbol sequence, as before. All object-resolving
paths remain in the image, including aliases and unreferenced registered paths.
The path set is therefore part of equivalent capture input; adding an extra
registered alias can intentionally change bytes.

If multiple original paths resolve to one live table/function, the
lexicographically first original path is selected for graph references. Sorting
and first-entry selection replace the previous last-registration-wins pointer
map. All original symbols still resolve on restore. Equivalent host alias
relationships must be supplied at restore, as with the existing host contract.
Capture-path relocation changes lookup only: the original symbol is emitted,
and restore still resolves originals against fresh host instances.

## Capture-order census

| Source | Disposition and conditions |
| --- | --- |
| Seed registry walk through Lua tables | **Fixed at consumption.** Seed discovery may remain unordered, but the complete registered path set is sorted/deduplicated before symbol assignment and pointer alias selection. |
| Explicit `register_function` / `register_host_path` order | **Fixed.** Same path-set normalization; registration APIs retain their existing behavior. |
| Multiple paths for one host pointer | **Fixed.** First lexicographic original path wins, independent of registration order. |
| Live capture-path map | **Already ordered.** It is a BTreeMap. Validation walks ordered keys; resolution uses lookup while iterating sorted originals. Every capture resolves live; no pointer cache is retained. |
| Root, ordinary and host table pair walks | **Fixed for scalar keys and anchored identities.** Sort before graph conversion, then defer identity batches until scalar/upvalue reachability is drained. |
| Unreached table/function key ties | **Named residue.** Stable Lua encounter order, under the exact trigger above. Later IDs can inherit it. |
| Table/host-table/closure IDs and worklists | **Reach-order canonical within the guarantee.** FIFO discovery from sorted entries. Addresses serve only as equality keys in maps; maps are not enumerated to emit nodes. |
| Closure prototype deduplication | **Reach-order canonical within the guarantee.** Full `Function::dump(false)` bytes are equality keys, first closure reach assigns the prototype ID; no hash-map iteration is serialized. Different source names, debug metadata or bytecode are different captured input. |
| Upvalue enumeration and sharing | **Already positional.** Slots are enumerated 1 upward; cell IDs follow first closure/slot reach. Cell-pointer maps only detect sharing. No reordering of positional closure slots is allowed. |
| Host symbols, graph IDs and cell IDs stored in hash maps/sets | **No independent ordering dependence.** Membership/lookup only. The encoder walks ordered vectors, never map buckets. |
| HostEntry values | **Existing symbolic behavior.** Only the key and HostEntry tag are emitted; callback addresses and opaque Rust state are not serialized. Fresh host entries are retained on restore. |
| Restore reconciliation walks | **No successful-capture ordering dependence within the guarantee.** Current-key enumeration only determines removal order; canonical recapture ignores resulting scalar hash layout. Unanchored identity ties retain the named residue. |
| Encoder | **Already ordered.** Serializes vectors, exact scalar bits and byte strings; adds no time, RNG, pointer or process metadata. |

Dumped Lua chunks retain their existing runtime/build compatibility requirements;
this work does not promise cross-ABI bytecode portability. Opaque host state,
including the existing default random-provider seed limitation, remains outside
the heap image. Neither issue is an additional capture-order algorithm.

## Format discipline

`FORMAT_VERSION` remains **1**. The wire tags, decoder, reference semantics and
restore implementation are unchanged. Version 1 never required sorted table
entries or symbols: references carry explicit indices, and table definitions
represent key/value sets. Only the producer's ordering and index selection
change. Existing images retain their original symbol choices when restored.
Deliberate rejection/version bump is unnecessary because no interpretation of
an existing image changes.

`version_one_noncanonical_symbol_and_entry_orders_still_restore` builds a
version-1 image with reversed symbol order, consistently remapped symbol
references, and reversed table entries. Restore accepts it and recapture equals
the canonical image exactly. This is a synthetic order-compatibility pin, not a
claim that a historical binary fixture was tested. Existing corrupt/wrong-version
rejection pins remain active.

## Added pins

- `independent_construction_orders_produce_identical_images`: repeated capture
  and independently allocated heaps with opposite table insertion, table/closure
  allocation and custom host registration orders compare byte-for-byte. Covers
  cycles, shared subtables, exact mixed numeric keys, binary strings, host table
  mutations, host identity keys, sibling/upvalue-anchored identity keys, shared
  and separate upvalues, prototype deduplication and host aliases. Decoded keys
  pin the stated type/value order, and decoded symbols pin sorting and alias
  selection.
- `canonical_capture_matches_a_fresh_process`: the test executable launches one
  exact child test. The child uses reverse construction and writes its complete
  image; the parent compares every byte, waits for success and removes the file.
- `canonical_image_round_trips_bit_for_bit_and_preserves_state`: restore into
  the opposite construction, exact recapture equality before state mutation,
  followed by checks of cycles, identities, fresh host calls and upvalue sharing.
- `version_one_noncanonical_symbol_and_entry_orders_still_restore`: version-1
  unsorted-order acceptance and canonical recapture, described above.
- `numeric_key_comparison_preserves_integer_precision`: mixed numeric comparison
  at fractional, integer-precision, i64 and infinite boundaries.
- `unanchored_identity_keys_round_trip_without_claiming_canonical_order`:
  multiple unanchored table and function keys preserve their state; no byte
  equality assertion is made for this residue.
- `unsupported_key_classes_remain_typed_errors`: threads, userdata,
  light userdata, metatable-bearing keys and unregistered C keys still refuse;
  host-table key positions do not acquire the value-only HostEntry exception.

## Verification

Counts are passed / failed / ignored. Engine baselines were run in this run;
consumer baseline counts below are the supplied, prep-measured bases, not a
separate before-change consumer run by this agent.

| Command / target | Base | Final |
| --- | ---: | ---: |
| `cargo test -p storm-lua-vm --lib` | 58 / 0 / 0 | 65 / 0 / 0 |
| `cargo test -p storm-lua-addon`: unit | 26 / 0 / 0 | 26 / 0 / 0 |
| Same: synchronous integration | 10 / 0 / 0 | 10 / 0 / 0 |
| `cargo test -p storm-compat-world --lib --all-features` | 1290 / 0 / 70 | 1290 / 0 / 70 |
| `cargo test -p storm-compat-server --all-targets`: lib | 54 / 0 / 0 | 54 / 0 / 0 |
| Same: main | 67 / 0 / 11 | 67 / 0 / 11 |

All commands in this table exit 0. The VM fresh-process child separately reports
one passed test; it is the same test, not an additional parent-suite test. Addon
doctests and the server's other zero-test targets compile but supply no executed
behavior pins. Ignored tests retain their existing status and are not counted
as passes.

Changed-file rustfmt and `git diff --check` pass. Workspace
`cargo fmt --all --check` exits 1 on unchanged pre-existing formatting in Addon,
minify, spec and VM logging files; it reports no diff for snapshot.rs. Those
unrelated files are unchanged from the base. No all-workspace formatting success
is claimed.

Supplemental completed checks:

- `cargo xtask check`: exit 0 (including dependency ownership/DAG and repository
  Markdown/generated-data checks).
- `cargo check -p storm-lua-vm --features debug,backend-mlua`: exit 0.
- `cargo clippy -p storm-lua-vm --lib`: exit 0, with one existing
  `same_item_push` warning in unchanged dialect.rs; no snapshot.rs warning.
  This is not an all-targets or warnings-as-errors lint claim.
- The consumer's
  `characters_vehicle_and_running_addon_survive_drop_and_fresh_process` test,
  rerun with `--nocapture`, passes (parent and fresh child). Its existing
  observational comparison now prints
  `suspend opaque VM whole-image byte equality after independent restores: true`.
  This is evidence for that fixture, not an assertion covering the unanchored-key
  residue. No timing/throughput conclusion is drawn from this concurrent run.

Completed-command logs, exit codes, source checksum, review patch and an evidence
manifest are retained in the campaign's `runs/vm-capture-canonicalization/`.
The external `REPORT.md` records the delivered Git tip and final clone states.

All Cargo commands use the campaign-provided CARGO_HOME/CARGO_TARGET_DIR/TMPDIR
and CARGO_INCREMENTAL=0. The campaign target resides on persistent ext4,
not tmpfs. No dependency declarations or downstream source files change.
