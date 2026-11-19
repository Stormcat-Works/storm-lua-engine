# Snapshot host capture paths — 2026-11-04

Engine base: `40aaca06dfdb83ce5c96d3739be1d6e26cfd2a01` on
`muse/vm-snapshot`. This extends the snapshot and Addon bridge records without
changing the Addon facade, mlua or vendored Lua.

## Operational contract

`HostBindings::snapshot_capture_paths` is a map from original registered host
paths to live capture paths (both are vectors of UTF-8 string segments). It is
installed through the existing `Vm::install_bindings`, including through the
unchanged `AddonConfig.bindings` forwarding path. Installing bindings replaces
the VM's capture-path map. The map can be supplied before the corresponding host
slots are registered, as happens during normal Addon construction.

For example, a host can map `["server", "privateSlot"]` to
`["extension", "operation"]`, then use its trusted source bootstrap to move the
actual function and delete the temporary server entry. Snapshot resolves the
live path anew on every capture but writes the original registered path as the
symbol. This is an explicit host policy, not a search for script-created aliases
or a cache of old function pointers. It does not execute code or change the Lua
namespace. Only lookup configuration is permitted in the Game profile; actual
custom values/functions continue to require Extended.

Restore already resolves all image symbols against the fresh environment before
reconciling the root and host tables. It therefore obtains fresh original host
functions, then reconstructs the saved extension table and removes the temporary
server slots through ordinary heap restoration. No pre-restore code hook or
synthetic callback is required. Hosts must seed the original symbol paths at
restore and supply equivalent capture-path configuration for later captures.

Invalid empty paths/segments and more than 512 mappings are rejected during
configuration validation. Capture rejects a mapping whose key is not registered,
an unresolved live path, or a mapped value that is not a table/function. There is
no fallback to the original path when an explicit live path fails. These errors
leave the VM usable and unchanged. Restore's existing unresolved-symbol refusal
still happens before live heap writes. Later restore errors retain the existing
nontransactional contract: discard a failed candidate; this change does not claim
that all restore failures are atomic.

## Format and compatibility

No binary format, envelope, ABI tag or fingerprint change. VM `FORMAT_VERSION`
remains 1; Addon and compat envelopes also remain unchanged. The new policy is
host configuration, not image metadata. Canonical symbols use the existing
symbol encoding. The public Rust struct gains a field; exhaustive `HostBindings`
literals must supply it or use `..Default::default()` (the conformance fixture is
updated accordingly).

## Pins

Four new VM tests cover:

- A moved host function and cached alias restore to fresh callbacks after dropping
  the source VM; the private entry stays deleted; a second capture/restore works.
  The decoded image explicitly retains the original symbol and version 1.
- Missing parents/entries, non-table parents and scalar destinations refuse
  capture without changing saved state, calling host functions or latching the
  VM. Repairing the path permits a later capture on the same VM.
- A missing original restore symbol refuses before overwriting live nested/global
  state. The same target VM remains runnable and accepts a repaired restore.
- Invalid configuration and unregistered keys refuse; actual Game host binding
  restrictions remain enforced without changing live state.

Compat's corresponding record measures the four enabled checkpoint acceptance
pins, including calling-VM continuation, vehicle state and joined observers.
Error-result retrieval still follows the inherited InvalidArgument/Failed
lifecycle; path lookup does not change it.

## Verification

Commands use the campaign's inherited `CARGO_HOME`/`TMPDIR`,
`CARGO_INCREMENTAL=0` and repo-local `target/`, with `CARGO_TARGET_DIR` unset.

| Suite | Baseline passed / failed / ignored | Final passed / failed / ignored |
| --- | ---: | ---: |
| `cargo test -p storm-lua-vm -p storm-lua-addon`: VM | 54 / 0 / 0 | 58 / 0 / 0 |
| Same command: Addon unit | 26 / 0 / 0 | 26 / 0 / 0 |
| Same command: synchronous integration | 10 / 0 / 0 | 10 / 0 / 0 |
| Same command: both doctest targets | 0 / 0 / 0 | 0 / 0 / 0 |

The baseline and final commands both exit 0. The final current-source log is
`local-runs/vm-snapshot-hostpath-hook/verified-engine.log`; the earlier successful
`final-engine.log` precedes the explicit same-path mapping type check.

Additional completed checks:

- `cargo test -p storm-lua-conformance --test environments`: 10 passed, 0 failed,
  0 ignored, exit 0. This compiles the updated exhaustive HostBindings literal.
- `cargo check -p storm-lua-vm --features debug,backend-mlua`: exit 0.
- `cargo xtask check`: exit 0 (dependency ownership/DAG, generated data and
  repository checks). No dependency declarations change in the engine.
- Targeted rustfmt checks for the four changed Rust files, and
  `git diff --check`: exit 0. No workspace-wide clippy result is claimed.

No Steam/game process, listener or game-install write was started. Build
preflight passed and observed root-space samples stayed above 4 GiB. Required
logs/exit files are retained on persistent storage, also copied with hashes into
the campaign run's `runs/vm-snapshot-hostpath-hook/` directory. The external
`REPORT.md` records exact tested commit/patch identities and push delivery status;
the companion compat checkout's Git directory is read-only in this run.
