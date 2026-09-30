# Source maps and source locations

## Published and working-tree boundaries

Published **v0.2.1** returns detailed Source Map v3 JSON `code`/`map` pairs for non-minified normal and LifeBoat builds. Token/column anchors and exact internal byte ranges identify copied original slices. Standalone `minify` and `minify: true` still do not return a post-optimization map.

The **unpublished v0.3.0 work** adds internal optimizer provenance and candidate/Worker transfer. It does not yet expose the final optimized Source Map v3 SDK contract. The stages and pass audit are in [Source provenance](../design/source-provenance.md). Published SDK tags and artifacts remain fixed.

A failed build has no artifact. Hosts must check the build result before using `code` or `map`. Compilation does not create a VM or execute Lua.

## Published non-minified origin and location model

The linker owns ordered, disjoint `LinkedRange` intervals for verbatim source slices. Each has generated `[startByte,endByte)` and original `startByte` in a module snapshot. Line fields are a coarse projection retained for low-level line lookup, not the authority for column correspondence.

The mapper records generated token starts, line starts and verbatim-slice boundaries. Mapping anchors resolve to their exact original line and column. An arbitrary position inside a token resolves to the preceding anchor; this is not a character-by-character mapping. Internal verbatim ranges can translate byte offsets exactly without guessing from the encoded map.

Generated `do/end`, loader scaffolding, return-normalization prefixes, hoisted variables, injected namespace declarations and EOF receive explicit unmapped anchors. A generated prefix on the same line as original text remains unmapped until the original slice begins. Greatest-lower-bound consumers must not inherit a preceding origin across generated gaps.

LifeBoat development blocks, unused sections and removed directive comments are recorded when they are blanked. Their intervals are subtracted from the origin table. Preserving byte length and newlines while replacing Unicode with ASCII spaces does not make the replaced text original source. No final-string comparison is used to infer these origins.

`lib.util` maps to `lib/util.lua`; injected ambient members use the same logical path convention. `sourcesContent` retains the exact original snapshots of mapped sources, including original line endings. `names` is still empty. Optimized local names, expression transformations and eliminated-variable values are not encoded in this phase.

## Coordinates

Internal ranges are UTF-8 byte offsets. Lua/parser diagnostics use one-based lines and byte columns. Serialized map coordinates use zero-based lines and UTF-16 columns; common JS consumers expose one-based lines and zero-based columns. Convert deliberately rather than incrementing or reusing byte columns blindly.

The shared syntax `LineIndex` handles LF and CRLF without normalizing source text, validates character boundaries and supports EOF. It indexes non-ASCII width differences so mapping many tokens on a long line does not repeatedly scan that line. UTF-16 coordinates do not imply UTF-16 encoding of the Lua source itself.

## Host connection

Keep the exact map with the exact generated code loaded under a host-chosen chunk name, for example `@mapped-program.lua`. The chunk name is not a source filename from the map. The map neither carries a runtime handle nor automatically installs breakpoints.

To bind an original breakpoint, enumerate exact entries for its source path and line, deduplicate generated lines and use the loaded chunk identity. Missing entries remain unmapped; do not silently select a nearby original line. A mapped blank/comment-only line is not evidence that Lua can stop there. Executable-line binding belongs to the host/debugger.

When the runtime provides a generated column, map that actual column after converting coordinate units. When it provides only a line, do not invent column zero as the actual execution position. Enumerate origins on the exact generated line: a unique source/line may be displayed as a line-level association; multiple origins remain ambiguous and generated-only frames remain generated. Existing simple examples with original text at column zero continue to work, but are not a general column-recovery mechanism.

Recognized runtime error source/line information can be associated using the same rules. Preserve unrecognized errors. Code edits, minification, a different bundle or source snapshots invalidate the pairing. A code hash alone cannot distinguish two original inputs that compile to identical Lua.

Imported maps are not authorization to read files. Embedded `sourcesContent` supplies the source view; hosts must not automatically fetch arbitrary listed paths. Publishing embedded original source is an explicit host decision.

## Diagnostics versus debugging

`analyze` diagnoses original modules directly and returns module/range without requiring an output map. Build-time diagnostics after linking translate their byte positions through the exact verbatim intervals; generated-only or invalid positions lose their source attribution rather than snapping to another statement. End positions are preserved only while the whole diagnostic range remains within the same original slice. An end crossing generated glue or another source is omitted, not fabricated.

Pre-build source transformations must preserve or compose their own origin ranges. The in-engine LifeBoat blanking operations do so. External host transformations remain the host's responsibility.

This map does not restore optimized-away variables, original evaluation order, inlined stack frames or per-iteration origins of data-driven generated loops. These are separate compiler/debugger capabilities.

## Executable evidence

- `crates/storm-lua-syntax/src/source_position.rs`: byte/UTF-16 conversion, Unicode boundaries, CRLF and EOF.
- `crates/storm-lua-build/src/source_map.rs`: column anchors, multi-file boundaries, synthetic prefixes, exact original snapshots and LifeBoat exclusions.
- `crates/storm-lua-build/src/public_api.rs`: byte-column and end-range diagnostic composition.
- `packages/lua-engine/tests/wasm/compiler-provenance.test.mjs`: independent trace-mapping consumer, real compiler WASM and canonical target-search continuation across WASM instances.
- `conformance/tests/source_maps.rs`: actual-Lua Native execution of source-qualified breakpoints, caller locations and runtime errors.
- `packages/lua-engine/tests/wasm/source-map.test.mjs`: actual runtime WASM, generated gaps, original lint locations and omitted optimized maps.
- `examples/consumer/source-map.mjs`: independent installed SDK consumer of the non-minified path.

Test existence is not a claim that a particular revision passed; executed commands and results belong in verification records. The trace-mapping package is an example/test dependency, not a runtime dependency of the SDK.


## v0.3.0開発中: 構文/印字レイヤーの範囲記録

`parse_source_with_positions`のNodePositionsは、同じパース済みASTの全node byte spanと、NameSiteで識別した名前出現の範囲を保持する。従来の診断用pointも保持する。位置はUTF-8バイトで、最適化後のASTにはそのまま流用できない。

`Printer.output_with_positions`は`PrintedSource { code, emissions }`を返す。NodeEmissionは最終生成コード上の半開UTF-8 byte範囲、同じAST内のNodeId、任意のNameSiteを示す。範囲は入れ子になり得る。省略されたnodeには生成範囲を捏造しない。通常のPrinterと同じコードと改行数を生成する。

これは最適化後Source Mapの完成APIではない。入力ASTが変換されている場合、元範囲は各変換の由来情報から取得する必要がある。`minify`/`build(minify:true)`がmapを返すようになったとは扱わない。


## v0.3.0開発中: 内部の最適化由来

低レベルRustの`CompileOptions.origin_source: Some(label)`は、原文snapshotを初期由来として、返却する`CompileCodeResult.origins: Some(GeneratedOrigins)`まで追跡する。Noneは通常の非追跡経路である。labelは表示用で、任意ファイルを開く権限や実runtime chunk IDではない。このoptionは高レベルbuild/SDKのmap設定としてまだ公開していない。

内部のSourceSpanは原文snapshot番号と半開UTF-8 byte範囲。OriginはSource / Derived / Synthetic、precisionはName / Token / Expression / Statement / Group、主な範囲と関連範囲・元の名前を保持する。UnknownはOriginなしとして扱い、Syntheticの別名にしない。Sourceは元構文への帰属を表すもので、印字の文字単位一致を保証しない。

GeneratedOriginsは原文snapshot、internされたorigin一覧、最終コードの非重複byte区間を返す。子の由来が失われた区間は、位置が残っている親や隣の区間へ誤って帰属させない。name slotの由来がない場合に対応するnode範囲を使うときは、そのnodeの粗いprecisionを維持する。区間のunknown_bytesは未帰属byte数であり、残りがすべて精密な原文位置へ戻れるという指標ではない。SyntheticやGroupへの帰属も残りに含まれる。

NodeArenaは既存Arenaのnode storageと独立したoptionalな由来テーブルを所有する。構文Eqと最適化の評価値から由来を除外し、clone/rollbackで同じcandidateの位置情報を保持する。直接mutable indexingはslotの由来を無効化し、mutable全体iterationは保守的に全slotを無効化する。新node/切り詰め後の再利用には古い由来を付けない。監査した変換のみが元node/名前slot/関連範囲の引き継ぎを指定する。

同じファイル名でも内容が違うsnapshotは別sourceとして扱う。別ArenaのNodeIdが一致しても同一nodeと解釈しない。JSONの未追跡ASTは従来のnodes/strings形を維持するが、Workerのbinary contextは同compiler版専用のopaque形式であり、0.2.1との互換性は保証しない。低レベルRustでAst.nodesをVecとして直接構築するconsumerはNodeArenaへの更新が必要。

転送時は元spanのindex/UTF-8境界/範囲、名前slot重複、slot数を検証する。最終生成範囲も連続性/UTF-8境界/原文index/生成長を検証するが、同長の別コードとの組み違いをこの検証だけで識別できるわけではない。code/map/snapshot識別はP4の成果物契約で追加する。

67パスの分類と未対応範囲は[台帳](../design/source-provenance-pass-audit.json)へ保存する。パスがmetadata非依存で完走し同じLuaを返せることと、その全出力を精密に原文へ戻せることを区別する。
