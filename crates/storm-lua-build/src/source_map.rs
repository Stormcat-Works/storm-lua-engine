//! Source Map v3 生成（設計 §6.1 / ロードマップ P4）。
//!
//! `link_project`（`link.rs`）が返す `LinkedRange` 区間表を正本にし、`rust-sourcemap`
//! （crates.io 名 `sourcemap`）で JSON へエンコードする。トークン・行・コピー範囲境界の位置を記録し、
//! `sources` は `/` 区切り + `.lua` のパス形式・`sourcesContent` は常に埋め込む（設計 §6.1）。
//! `link.linked_source` が `None`（リンク失敗）の場合は生成しない。

use std::collections::{BTreeMap, BTreeSet};

use sourcemap::SourceMapBuilder;

use crate::link::{lookup_source_byte, LinkResult};
use storm_lua_analysis::project::{AmbientMember, LuaProject};
use storm_lua_syntax::{lexer::Lexer, source_position::LineIndex};

/// モジュールキーを `/` 区切り + `.lua` のパス形式へ変換する（設計 §6.1）。
/// `key = segment ("." segment)*`（§2.1）という文法により変換は常に可逆。
/// ambient 合成キー（`"root.member"` 形式。`link.rs` の `Expander::ambient_sources` 参照）も
/// 同じ `.` 区切りの文字列であり、モジュールキーと文法上区別が無いため同じ規則を適用する。
pub fn module_key_to_path(key: &str) -> String {
    format!("{}.lua", key.replace('.', "/"))
}

/// ambient 合成キー `"root.member"` の元ソーステキストを取得する
/// （`kind: environmentOnly` はエクスポート対象コードから参照されたら `environment-only-api`
/// エラーになりリンク自体が失敗するため、`link_project` が成功した結果にはここへ来ない）。
fn ambient_source<'a>(project: &'a LuaProject, key: &str) -> Option<&'a str> {
    let (root, member) = key.split_once('.')?;
    let namespace = project.ambient.get(root)?;
    match namespace.members.get(member)? {
        AmbientMember::Module { source } => Some(source.as_str()),
        AmbientMember::EnvironmentOnly => None,
    }
}

/// `LinkedRange::module` に現れるキー（通常モジュール or ambient 合成キー）の元ソース全文を取得する。
/// Generated declarations are not assigned an origin. Ambient member bodies
/// resolve here exactly like ordinary module bodies.
pub(crate) fn source_text<'a>(project: &'a LuaProject, key: &str) -> Option<&'a str> {
    project
        .modules
        .get(key)
        .map(String::as_str)
        .or_else(|| ambient_source(project, key))
}

/// Encode the exact non-minified artifact as Source Map v3.
/// Anchors are generated token starts, line starts and verbatim-slice boundaries.
/// Columns are UTF-16 units; Lua diagnostic byte columns are not interchangeable.
/// Token interiors resolve to their preceding anchor, not to an inferred column.
/// Generated spans and EOF have explicit unmapped anchors. Names are not encoded.
#[expect(
    clippy::expect_used,
    reason = "Linking validated Lua preserves valid tokens and UTF-8 slice boundaries; all origins belong to project snapshots; serialization writes only to an in-memory Vec"
)]
pub fn generate_source_map(project: &LuaProject, link: &LinkResult) -> Option<String> {
    let generated = link.linked_source.as_deref()?;
    let generated_index = LineIndex::new(generated);
    let mut builder = SourceMapBuilder::new(None);
    let mut sources = BTreeMap::new();
    for range in &link.ranges {
        sources.entry(range.module.as_str()).or_insert_with(|| {
            let text = source_text(project, &range.module).expect("linked source snapshot");
            let id = builder.add_source(&module_key_to_path(&range.module));
            builder.set_source_contents(id, Some(text));
            (id, LineIndex::new(text))
        });
    }
    let mut anchors = BTreeSet::from([0, generated.len()]);
    anchors.extend(
        generated
            .bytes()
            .enumerate()
            .filter_map(|(i, b)| (b == b'\n').then_some(i + 1)),
    );
    for range in &link.ranges {
        anchors.insert(range.output_start_byte);
        anchors.insert(range.output_end_byte);
    }
    anchors.extend(
        Lexer::new(generated)
            .all()
            .expect("linker emitted valid Lua tokens")
            .into_iter()
            .map(|token| token.p),
    );
    for output_byte in anchors {
        let (line, col) = generated_index
            .utf16_position(output_byte)
            .expect("generated character boundary");
        if let Some((range, source_byte)) = lookup_source_byte(&link.ranges, output_byte) {
            let (source_id, index) = &sources[range.module.as_str()];
            let (source_line, source_col) = index
                .utf16_position(source_byte)
                .expect("original character boundary");
            builder.add_raw(
                line,
                col,
                source_line,
                source_col,
                Some(*source_id),
                None,
                false,
            );
        } else {
            builder.add_raw(line, col, 0, 0, None, None, false);
        }
    }
    let mut buf = Vec::new();
    builder
        .into_sourcemap()
        .to_writer(&mut buf)
        .expect("in-memory source map writer");
    Some(String::from_utf8(buf).expect("source map JSON is UTF-8"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::link::link_project;
    use std::collections::BTreeMap as Map;
    use storm_lua_analysis::project::AmbientNamespace;

    fn project(entry: &str, modules: &[(&str, &str)]) -> LuaProject {
        LuaProject {
            entry: entry.to_string(),
            modules: modules
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ambient: Map::new(),
        }
    }

    #[test]
    fn returns_none_when_link_fails() {
        let p = project("main", &[("main", "local x = require(\"missing\")\n")]);
        let link = link_project(&p);
        assert!(link.linked_source.is_none());
        assert_eq!(generate_source_map(&p, &link), None);
    }

    #[test]
    fn generates_v3_shape_with_sources_and_contents() {
        let p = project(
            "main",
            &[
                (
                    "main",
                    "local util = require(\"lib.util\")\nreturn util.x\n",
                ),
                ("lib.util", "local x = 1\nreturn { x = x }\n"),
            ],
        );
        let link = link_project(&p);
        assert!(link.diagnostics.is_empty(), "{:?}", link.diagnostics);
        let map_json = generate_source_map(&p, &link).expect("source map");

        let value: serde_json::Value = serde_json::from_str(&map_json).expect("valid JSON");
        assert_eq!(value["version"], 3);
        let sources = value["sources"].as_array().expect("sources array");
        let source_strs: Vec<&str> = sources.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(source_strs.contains(&"main.lua"));
        assert!(source_strs.contains(&"lib/util.lua"));

        let contents = value["sourcesContent"]
            .as_array()
            .expect("sourcesContent array");
        assert_eq!(contents.len(), sources.len());
        // main.lua の位置に project.modules["main"] の全文がそのまま入っている。
        let main_idx = source_strs.iter().position(|&s| s == "main.lua").unwrap();
        assert_eq!(
            contents[main_idx].as_str().unwrap(),
            p.modules["main"].as_str()
        );
        let util_idx = source_strs
            .iter()
            .position(|&s| s == "lib/util.lua")
            .unwrap();
        assert_eq!(
            contents[util_idx].as_str().unwrap(),
            p.modules["lib.util"].as_str()
        );

        let mappings = value["mappings"].as_str().expect("mappings string");
        assert!(!mappings.is_empty());
    }

    #[test]
    fn generates_map_with_ambient_source_included() {
        let mut p = project(
            "main",
            &[("main", "local clamp = sim.clamp\nreturn clamp(5)\n")],
        );
        let namespace = AmbientNamespace {
            members: Map::from([(
                "clamp".to_string(),
                AmbientMember::Module {
                    source: "return function(x) return x end\n".to_string(),
                },
            )]),
        };
        p.ambient.insert("sim".to_string(), namespace);

        let link = link_project(&p);
        assert!(link.diagnostics.is_empty(), "{:?}", link.diagnostics);
        let map_json = generate_source_map(&p, &link).expect("source map");
        let value: serde_json::Value = serde_json::from_str(&map_json).expect("valid JSON");
        let sources = value["sources"].as_array().expect("sources array");
        let source_strs: Vec<&str> = sources.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(source_strs.contains(&"sim/clamp.lua"));
        let contents = value["sourcesContent"]
            .as_array()
            .expect("sourcesContent array");
        let idx = source_strs
            .iter()
            .position(|&s| s == "sim/clamp.lua")
            .unwrap();
        assert_eq!(
            contents[idx].as_str().unwrap(),
            "return function(x) return x end\n"
        );
    }

    #[test]
    fn output_line_start_matches_the_exact_byte_origin() {
        // trace-mapping 相当の逆引きを Rust 側でも検証する（外部ライブラリ検証テストと対の内部確認）。
        let p = project(
            "main",
            &[
                ("main", "local util = require(\"util\")\nlocal y = util.x\n"),
                ("util", "local z = 1\nreturn z\n"),
            ],
        );
        let link = link_project(&p);
        assert!(link.diagnostics.is_empty(), "{:?}", link.diagnostics);
        let map_json = generate_source_map(&p, &link).expect("source map");
        let decoded = sourcemap::SourceMap::from_slice(map_json.as_bytes())
            .expect("decode generated source map");

        let src = link.linked_source.as_deref().unwrap();
        let mut output_byte = 0;
        for (i, text) in src.split_inclusive('\n').enumerate() {
            let output_line = i as u32;
            let expected =
                lookup_source_byte(&link.ranges, output_byte).map(|(range, source_byte)| {
                    let original = source_text(&p, &range.module).unwrap();
                    (
                        range.module.as_str(),
                        LineIndex::new(original)
                            .byte_position(source_byte)
                            .unwrap()
                            .0,
                    )
                });
            output_byte += text.len();
            let token = decoded.lookup_token(output_line, 0);
            match expected {
                Some((module, source_line)) => {
                    let token = token
                        .unwrap_or_else(|| panic!("expected mapping at output line {output_line}"));
                    assert_eq!(
                        token.get_source(),
                        Some(module_key_to_path(module).as_str())
                    );
                    assert_eq!(token.get_src_line() + 1, source_line);
                }
                None => {
                    let token = token.expect("generated lines have an explicit unmapped segment");
                    assert_eq!(token.get_dst_line(), output_line);
                    assert_eq!(token.get_source(), None);
                }
            }
        }
    }
    #[test]
    fn generated_glue_never_points_past_source_and_does_not_inherit_an_origin() {
        for module in [
            "value=7",
            "return {value=7}",
            "if flag then return 7 end\nreturn 8",
            "",
        ] {
            let p = project(
                "main",
                &[
                    (
                        "main",
                        "local m=require(\"lib\")\nfunction onTick()output.setNumber(1,7)end",
                    ),
                    ("lib", module),
                ],
            );
            let link = link_project(&p);
            let generated = link.linked_source.as_ref().unwrap();
            let map = sourcemap::SourceMap::from_slice(
                generate_source_map(&p, &link).unwrap().as_bytes(),
            )
            .unwrap();
            for token in map.tokens() {
                if let Some(file) = token.get_source() {
                    let source = if file == "lib.lua" {
                        module
                    } else {
                        &p.modules["main"]
                    };
                    assert!(
                        token.get_src_line() < source.split('\n').count() as u32,
                        "{module:?}: {file} maps beyond its source"
                    );
                }
            }
            for (line, text) in generated.lines().enumerate() {
                if text == "do"
                    || text.starts_with("if __stormmin_link_")
                    || text.starts_with("local __stormmin_link_")
                {
                    let token = map.lookup_token(line as u32, 0).unwrap();
                    assert_eq!(
                        token.get_source(),
                        None,
                        "generated-only line was assigned a source: {text}"
                    );
                }
            }
        }
    }

    #[test]
    fn multiline_return_body_has_exact_original_line_not_generated_prefix_line() {
        let source = "return function(x)\n  return x+1\nend";
        let p = project(
            "main",
            &[
                (
                    "main",
                    "local f=require(\"lib\")\nfunction onTick()output.setNumber(1,f(6))end",
                ),
                ("lib", source),
            ],
        );
        let link = link_project(&p);
        let code = link.linked_source.as_ref().unwrap();
        let map =
            sourcemap::SourceMap::from_slice(generate_source_map(&p, &link).unwrap().as_bytes())
                .unwrap();
        let generated = code
            .lines()
            .position(|line| line == "  return x+1")
            .unwrap();
        let token = map.lookup_token(generated as u32, 0).unwrap();
        assert_eq!(token.get_source(), Some("lib.lua"));
        assert_eq!(token.get_src_line(), 1);
    }

    fn assert_token_origin(
        map: &sourcemap::SourceMap,
        generated: &str,
        needle: &str,
        source_file: &str,
        original: &str,
    ) {
        let output = generated.find(needle).expect("generated token");
        let source = original.find(needle).expect("original token");
        let (line, col) = LineIndex::new(generated).utf16_position(output).unwrap();
        let token = map.lookup_token(line, col).unwrap();
        // Expected coordinates come directly from the original snapshot, not
        // from the linker's own range table or its line-lookup helper.
        let expected_line = original[..source].bytes().filter(|b| *b == b'\n').count() as u32;
        let start = original[..source].rfind('\n').map_or(0, |i| i + 1);
        let expected_col = original[start..source].encode_utf16().count() as u32;
        assert_eq!(token.get_dst_line(), line);
        assert_eq!(token.get_dst_col(), col);
        assert_eq!(token.get_source(), Some(source_file));
        assert_eq!(
            (token.get_src_line(), token.get_src_col()),
            (expected_line, expected_col)
        );
    }

    #[test]
    fn same_line_module_boundaries_and_generated_return_prefix_have_exact_columns() {
        let main = "local f=require('lib');output.setNumber(1,f())";
        let lib = "return function() return 7 end";
        let project = project("main", &[("main", main), ("lib", lib)]);
        let link = link_project(&project);
        let code = link.linked_source.as_ref().unwrap();
        let map = sourcemap::SourceMap::from_slice(
            generate_source_map(&project, &link).unwrap().as_bytes(),
        )
        .unwrap();
        assert_token_origin(&map, code, "function()", "lib.lua", lib);
        assert_token_origin(&map, code, "return 7", "lib.lua", lib);
        assert_token_origin(&map, code, "output.setNumber", "main.lua", main);
        let (line, col) = LineIndex::new(code)
            .utf16_position(code.find("function()").unwrap())
            .unwrap();
        assert!(
            col > 0,
            "fixture must have a generated prefix on the function line"
        );
        assert_eq!(map.lookup_token(line, 0).unwrap().get_source(), None);
        let (line, col) = LineIndex::new(code).utf16_position(code.len()).unwrap();
        assert_eq!(map.lookup_token(line, col).unwrap().get_source(), None);
        for range in &link.ranges {
            let original = source_text(&project, &range.module).unwrap();
            assert_eq!(
                &code[range.output_start_byte..range.output_end_byte],
                &original[range.source_start_byte
                    ..range.source_start_byte + range.output_end_byte - range.output_start_byte]
            );
        }
    }

    #[test]
    fn token_columns_use_utf16_after_unicode_and_preserve_crlf_snapshots() {
        let main = "local banner='😀あ';local f=require('lib');output.setNumber(1,f())\r\n";
        let lib = "return function()\r\n local caption='雪😀';return 7\r\nend";
        let project = project("main", &[("main", main), ("lib", lib)]);
        let link = link_project(&project);
        let code = link.linked_source.as_ref().unwrap();
        let map = sourcemap::SourceMap::from_slice(
            generate_source_map(&project, &link).unwrap().as_bytes(),
        )
        .unwrap();
        assert_token_origin(&map, code, "output.setNumber", "main.lua", main);
        assert_token_origin(&map, code, "return 7", "lib.lua", lib);
        assert_eq!(
            map.get_source_contents(
                map.sources().position(|name| name == "lib.lua").unwrap() as u32
            ),
            Some(lib)
        );
    }

    #[test]
    fn lifeboat_blanked_unicode_sections_are_unmapped_but_retained_tokens_are_exact() {
        let main = "---@section __LB_SIMULATOR_ONLY__\nprint('😀雪')\n---@endsection\n---@section unused\nfunction unused() return '😀' end\n---@endsection\nlocal title='あ😀';function onTick()output.setNumber(1,7)end\n";
        let project = project("main", &[("main", main)]);
        let link = crate::lifeboat::link_lifeboat(&project);
        let code = link.linked_source.as_ref().unwrap();
        let map = sourcemap::SourceMap::from_slice(
            generate_source_map(&project, &link).unwrap().as_bytes(),
        )
        .unwrap();
        assert_token_origin(&map, code, "output.setNumber", "main.lua", main);
        for token in map.tokens() {
            if token.get_source() == Some("main.lua") {
                assert!(
                    ![1, 4].contains(&token.get_src_line()),
                    "removed code inherited an original location"
                );
            }
        }
        for range in &link.ranges {
            assert_eq!(
                &code[range.output_start_byte..range.output_end_byte],
                &main[range.source_start_byte
                    ..range.source_start_byte + range.output_end_byte - range.output_start_byte]
            );
        }
    }
}
