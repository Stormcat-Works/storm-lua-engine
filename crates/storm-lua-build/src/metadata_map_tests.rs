#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use crate::addon::{build_addon, AddonBuildOptions};
use crate::optimized_source_map::validate;
use crate::public_api::{
    compile_lifeboat, compile_project, ApiCompileOptions, ApiProjectCompileOptions,
};
use crate::source_map::module_key_to_path;
use std::collections::BTreeMap;
use storm_lua_analysis::LuaProject;
use storm_lua_syntax::{provenance::OriginKind, source_position::LineIndex};

fn project(modules: &[(&str, &str)]) -> LuaProject {
    LuaProject {
        entry: "main".into(),
        modules: modules
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        ambient: BTreeMap::new(),
    }
}

fn snapshots(map: &sourcemap::SourceMap, project: &LuaProject) {
    for (key, source) in &project.modules {
        let path = module_key_to_path(key);
        let id = map.sources().position(|name| name == path).unwrap() as u32;
        assert_eq!(map.get_source_contents(id), Some(source.as_str()));
    }
}

fn token_position(
    map: &sourcemap::SourceMap,
    code: &str,
    project: &LuaProject,
    module: &str,
    token: &str,
) {
    let generated = code.find(token).unwrap();
    let original = project.modules[module].find(token).unwrap();
    let (line, col) = LineIndex::new(code).utf16_position(generated).unwrap();
    let mapped = map.lookup_token(line, col).unwrap();
    let (source_line, source_col) = LineIndex::new(&project.modules[module])
        .utf16_position(original)
        .unwrap();
    assert_eq!(
        mapped.get_source(),
        Some(module_key_to_path(module).as_str())
    );
    assert_eq!(
        (mapped.get_src_line(), mapped.get_src_col()),
        (source_line, source_col)
    );
}

#[test]
fn non_minified_metadata_with_explicit_detailed_map_succeeds() {
    for source in [
        "--[[stormmin-metadata\nprivate development note\n]]\nfunction onTick()output.setNumber(1,3)end",
        " \t--[==[stormmin-metadata\r\n雪😀\r\n]==]\r\nfunction onTick()output.setNumber(1,3)end",
        "--[[stormmin-metadata\n雪😀]]function onTick()output.setNumber(1,3)end",
    ] {
        let p = project(&[("main", source)]);
        for build in [compile_project, compile_lifeboat] {
            let options = ApiProjectCompileOptions {
                metadata_tag: Some("stormmin-metadata".into()), minify: Some(false),
                compile: ApiCompileOptions {source_map: Some(true), ..Default::default()},
            };
            let detailed = build(&p, &options);
            assert!(detailed.ok, "{detailed:?}");
            let code = detailed.code.unwrap();
            let text = detailed.map.unwrap();
            let data = validate(&code, &text).unwrap();
            let map = sourcemap::SourceMap::from_slice(text.as_bytes()).unwrap();
            snapshots(&map, &p);
            token_position(&map, &code, &p, "main", "output.setNumber");
            let start = code.find("function onTick").unwrap() - source.find("function onTick").unwrap() + source.find("--[").unwrap();
            let removed = data.mappings.iter().find(|m| m.start <= start && start < m.end).unwrap();
            assert!(removed.copied.is_none());
            assert_eq!(data.origins[removed.origin.unwrap() as usize].kind, OriginKind::Synthetic);
            let (line, col) = LineIndex::new(&code).utf16_position(start).unwrap();
            assert!(map.lookup_token(line, col).unwrap().get_source().is_none());
            let copied = data.mappings.iter().find(|m| m.start <= code.find("output.setNumber").unwrap() && code.find("output.setNumber").unwrap() < m.end).unwrap();
            let span = copied.copied.unwrap();
            assert_eq!(span.start + code.find("output.setNumber").unwrap() - copied.start, source.find("output.setNumber").unwrap());
            let standard = build(&p, &ApiProjectCompileOptions {
                compile: ApiCompileOptions {source_map: Some(false), ..options.compile}, ..options
            });
            assert!(standard.ok); assert_eq!(standard.code.as_deref(), Some(code.as_str()));
            let plain_map = sourcemap::SourceMap::from_slice(standard.map.unwrap().as_bytes()).unwrap();
            snapshots(&plain_map, &p);
            token_position(&plain_map, &code, &p, "main", "output.setNumber");
            assert!(plain_map.lookup_token(line, col).unwrap().get_source().is_none());
        }
    }
}

#[test]
fn metadata_maps_keep_all_module_snapshots_even_when_a_body_is_fully_removed() {
    let main="--[[stormmin-metadata\nmain note\n]]\nrequire('empty');require('lib');function onTick()output.setNumber(1,value())end";
    let lib = "--[=[stormmin-metadata\n雪😀]=]function value()return 3 end";
    let empty = "--[[stormmin-metadata\nempty note\n]]";
    let p = project(&[("main", main), ("lib", lib), ("empty", empty)]);
    for build in [compile_project, compile_lifeboat] {
        for minify in [false, true] {
            for source_map in [false, true] {
                let result = build(
                    &p,
                    &ApiProjectCompileOptions {
                        metadata_tag: Some("stormmin-metadata".into()),
                        minify: Some(minify),
                        compile: ApiCompileOptions {
                            source_map: Some(source_map),
                            ..Default::default()
                        },
                    },
                );
                assert!(result.ok, "{result:?}");
                let code = result.code.unwrap();
                assert!(!code.contains("note"));
                if let Some(text) = result.map {
                    let map = sourcemap::SourceMap::from_slice(text.as_bytes()).unwrap();
                    snapshots(&map, &p);
                    if source_map {
                        validate(&code, &text).unwrap();
                    }
                    if !minify {
                        token_position(&map, &code, &p, "main", "output.setNumber");
                        token_position(&map, &code, &p, "lib", "function value");
                    }
                } else {
                    assert!(minify && !source_map);
                }
            }
        }
    }
    for build in [compile_project, compile_lifeboat] {
        let p = project(&[("main", empty)]);
        for source_map in [false, true] {
            let result = build(
                &p,
                &ApiProjectCompileOptions {
                    metadata_tag: Some("stormmin-metadata".into()),
                    minify: Some(false),
                    compile: ApiCompileOptions {
                        source_map: Some(source_map),
                        ..Default::default()
                    },
                },
            );
            assert!(result.ok, "{result:?}");
            let code = result.code.unwrap();
            let text = result.map.unwrap();
            let map = sourcemap::SourceMap::from_slice(text.as_bytes()).unwrap();
            snapshots(&map, &p);
            assert!(map.tokens().all(|t| t.get_source().is_none()));
            if source_map {
                assert!(validate(&code, &text)
                    .unwrap()
                    .mappings
                    .iter()
                    .all(|m| m.copied.is_none()));
            }
        }
    }
}

#[test]
fn addon_metadata_standard_map_excludes_changed_bytes_and_keeps_utf16_positions() {
    let main = "--[[stormmin-metadata\n雪😀]]function onCreate()g_savedata.value=3 end";
    let p = project(&[("main", main)]);
    let result = build_addon(
        &p,
        &AddonBuildOptions {
            metadata_tag: Some("stormmin-metadata".into()),
            ..Default::default()
        },
    );
    assert!(result.ok, "{result:?}");
    let code = result.code.unwrap();
    let map = sourcemap::SourceMap::from_slice(result.map.unwrap().as_bytes()).unwrap();
    snapshots(&map, &p);
    token_position(&map, &code, &p, "main", "g_savedata");
    let start = code.find("function onCreate").unwrap() - main.find("function onCreate").unwrap();
    let (line, col) = LineIndex::new(&code).utf16_position(start).unwrap();
    assert!(map.lookup_token(line, col).unwrap().get_source().is_none());
    let empty = project(&[("main", "--[[stormmin-metadata\nempty note\n]]")]);
    let result = build_addon(
        &empty,
        &AddonBuildOptions {
            metadata_tag: Some("stormmin-metadata".into()),
            ..Default::default()
        },
    );
    assert!(result.ok, "{result:?}");
    let map = sourcemap::SourceMap::from_slice(result.map.unwrap().as_bytes()).unwrap();
    snapshots(&map, &empty);
    assert!(map.tokens().all(|t| t.get_source().is_none()));
}

#[test]
fn metadata_detailed_maps_support_identity_target_and_conservative_minification() {
    for source in [
        "--[[stormmin-metadata\nnote\n]]\nfunction onTick()output.setNumber(1,3)end",
        "--[[stormmin-metadata\n雪😀\n]]\nlocal _ENV=_ENV;function onTick()output.setNumber(1,3)end",
    ] {
        let p=project(&[("main",source)]);
        for build in [compile_project,compile_lifeboat] {
            let result=build(&p,&ApiProjectCompileOptions {
                metadata_tag:Some("stormmin-metadata".into()),minify:Some(true),
                compile:ApiCompileOptions {source_map:Some(true),target_size:Some(8192),..Default::default()},
            });
            assert!(result.ok,"{result:?}");
            validate(result.code.as_ref().unwrap(),result.map.as_ref().unwrap()).unwrap();
            let map=sourcemap::SourceMap::from_slice(result.map.unwrap().as_bytes()).unwrap();snapshots(&map,&p);
        }
    }
}
