# Addon non-optimizing build

This additive API does not imply that Addon optimization is implemented. `buildAddon(project, {environment, metadataTag})` uses the shared Rust include-once linker, private module locals and explicit development-section stripping. It produces readable Lua, a Source Map v3 with original sources, used module names and diagnostics. Calls do not execute Lua or host functions.

No Vehicle callback-phase diagnostics, property specialization, 8192 objective or minification passes run in this path. Unknown option fields including minify/targetSize are rejected. Output size is reported as UTF-16 code units; the host owns any game-editor size policy and must not silently truncate output.

`metadataTag` optionally identifies the exact first content line of a leading Lua long comment. The actual lexer identifies the comment. Its bytes except newline delimiters are blanked before linking, keeping all source positions. Metadata inside strings, non-leading comments and different markers is not stripped. Embedded sources in the diagnostic map still contain the original metadata, so exporting a map is not the same as exporting only the game script.

Only literal direct include names are statically linked. Cycles follow the explicit Engine include-once contract, returned values are discarded, unknown includes fail, and a local function named require retains ordinary lexical behavior. Dynamic/aliased global require is rejected for game export. The existing return-valued Vehicle linker is unchanged.

The game profile rejects unavailable base functions, including select, while preserving local/global definitions and nil/type probes. Extended explicitly supplies select.

This is syntax/link/environment checking, not a complete Addon lint implementation, a proof of every server API, or a game-world simulator. Host-provided server functions and lifecycle remain independent.
