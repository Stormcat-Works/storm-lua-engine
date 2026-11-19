# Addon continuation bridge — 2026-11-04

Branch: `muse/vm-snapshot-addon-bridge`, based on `muse/vm-snapshot`.
This extends the VM snapshot evidence in `vm-snapshot-20261104.md`.

## Public contract

`Addon::snapshot() -> Result<Vec<u8>, VmError>` captures the full Segment A VM image plus the Addon's private lifecycle state. `Addon::from_snapshot(AddonConfig, &[u8]) -> Result<Addon, VmError>` creates fresh host bindings and restores that image. It neither loads the retained source nor calls `start`/`onCreate`. The retained source remains available for a later explicit reload.

The versioned, bounded binary envelope retains Empty/Loaded/Ready/Destroyed stage, the tick-disabled error latch, world-create flag, typed properties and menu definitions, source identity, and the quiescent HTTP allocation cursor. Busy/failed lifecycle states, development require caches, outstanding built-in HTTP requests and undrained logs are explicit errors. Old HTTP response tokens belong to a different generation after restore.

The host supplies equivalent API bindings, profile and limits. Host-owned services and their state (for example the CRT random stream) remain the host's continuation responsibility. Supplying savedata or a development require loader to the restore constructor is rejected. Segment A's unsupported heap objects remain unsupported; this bridge does not broaden the VM format.

Addon-installed server/property/matrix functions and scoped callback wrappers are registered as symbolic host references. Restored references bind the fresh host slots, including stored aliases to scoped functions. The Game profile unregisters the unavailable `select` symbol. `LogReader::has_pending`, `Vm::unregister_host_path` and the HTTP cursor helpers are the small supporting APIs.

## Pins and results

Final command: `env -u CARGO_TARGET_DIR CARGO_INCREMENTAL=0 cargo test -p storm-lua-vm -p storm-lua-addon -p storm-lua-microcontroller`.

| Suite | Before | After |
| --- | ---: | ---: |
| VM unit tests | 54 passed | 54 passed |
| Addon unit tests | 21 passed | 26 passed |
| Addon synchronous integration | 10 passed | 10 passed |
| Microcontroller unit tests | 0 tests, compiled | 0 tests, compiled |
| Doctests | 0 tests | 0 tests |

All final results have zero failures. The five bridge pins cover a counter shared by two closures, an external onCreate side effect executed exactly once, Loaded/Destroyed stages and the tick-error latch, truncation and pending HTTP rejection, scoped host aliases rebinding fresh callbacks, and pending log rejection. The restored counter continues after the original instance is dropped. The external create counter remains one.

Campaign logs are outside the repository at `~/codex-campaign-20261104/runs/item4-segment-b2/logs/`, notably `final-engine2.log`. Compat's defining mixed vehicle/Addon test supplies the end-to-end integration evidence on its corresponding branch. No Steam process was used.
