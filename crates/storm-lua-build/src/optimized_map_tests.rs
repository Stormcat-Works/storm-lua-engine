#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use crate::optimized_source_map::{validate, MapExtension};
use crate::public_api::{compile, ApiCompileOptions, ApiNumericMode};
use serde_json::Value;
use storm_lua_syntax::explanation::{ReasonOperation, RelationRole};
use storm_lua_syntax::provenance::{OriginKind, OriginPrecision};
fn mapped(source: &str) -> (String, String, MapExtension) {
    let options = ApiCompileOptions {
        source_map: Some(true),
        source_name: Some("controller.lua".into()),
        zero_cost_newlines: Some(false),
        ..Default::default()
    };
    let result = compile(source, &options);
    assert!(result.ok, "{:?}", result.diagnostics);
    let code = result.code.unwrap();
    let map = result.map.unwrap();
    let data = validate(&code, &map).unwrap();
    let plain = compile(
        source,
        &ApiCompileOptions {
            source_map: Some(false),
            ..options
        },
    );
    assert_eq!(plain.code.as_deref(), Some(code.as_str()));
    assert!(plain.map.is_none());
    (code, map, data)
}
#[test]
fn standard_map_and_versioned_extension_describe_the_same_generated_buffer() {
    let source = "function onTick()output.setNumber(1,2*3)end";
    let (code, map, details) = mapped(source);
    let parsed: Value = serde_json::from_str(&map).unwrap();
    assert_eq!(parsed["version"], 3);
    assert_eq!(details.schema_version, 1);
    assert_eq!(details.producer.name, "storm-lua-engine");
    assert_eq!(details.producer.version, "0.3.0");
    assert_eq!(parsed["sourcesContent"][0], source);
    assert_eq!(parsed["sources"][0], "controller.lua");
    assert_eq!(details.generated.bytes, code.len());
    assert_eq!(details.generated.sha256.len(), 64);
    assert!(details
        .reasons
        .iter()
        .any(|r| r.code.as_ref() == "constant-folding"
            && r.before.as_deref() == Some("binary:*")
            && r.after.as_deref() == Some("number:6")));
    assert!(!sourcemap::SourceMap::from_slice(map.as_bytes())
        .unwrap()
        .tokens()
        .collect::<Vec<_>>()
        .is_empty());
}
#[test]
fn retained_operator_and_function_end_have_exact_token_positions() {
    let source = "function onTick()\n  output.setNumber(1, input.getNumber(1) + 2)\nend\n";
    let (code, map, data) = mapped(source);
    let point = code.find('+').unwrap();
    let mapping = data
        .mappings
        .iter()
        .find(|m| m.start <= point && point < m.end)
        .unwrap();
    let o = &data.origins[mapping.origin.unwrap() as usize];
    assert_eq!(o.precision, OriginPrecision::Token);
    let original = o.primary.unwrap();
    assert_eq!(&source[original.start..original.end], "+");
    assert_eq!(mapping.copied.unwrap().start, source.find('+').unwrap());
    let end = code.rfind("end").unwrap();
    let mapping = data
        .mappings
        .iter()
        .find(|m| m.start <= end && end < m.end)
        .unwrap();
    let original = data.origins[mapping.origin.unwrap() as usize]
        .primary
        .unwrap();
    assert_eq!(&source[original.start..original.end], "end");
    let sm = sourcemap::SourceMap::from_slice(map.as_bytes()).unwrap();
    let token = sm.lookup_token(0, point as u32).unwrap();
    assert_eq!(token.get_src_line(), 1);
    assert_eq!(
        token.get_src_col(),
        source.lines().nth(1).unwrap().find('+').unwrap() as u32
    );
}
#[test]
fn identity_target_returns_exact_copy_contract_not_just_a_coarse_group() {
    let source = "-- 😀\r\nfunction onTick()\r\n output.setNumber(1,7)\r\nend\r\n";
    let options = ApiCompileOptions {
        source_map: Some(true),
        target_size: Some(8192),
        ..Default::default()
    };
    let result = compile(source, &options);
    assert!(result.ok);
    assert_eq!(result.code.as_deref(), Some(source));
    let data = validate(source, &result.map.unwrap()).unwrap();
    assert_eq!(data.mappings.len(), 1);
    let copy = data.mappings[0].copied.unwrap();
    assert_eq!((copy.start, copy.end), (0, source.len()));
    assert_eq!(
        data.compilation["selection"]["criterion"],
        "valid-candidate-meeting-target"
    );
}
#[test]
fn inlined_leaf_keeps_its_definition_and_enclosing_call_context() {
    let source="local function twice(value)\n return value*2\nend\nfunction onTick()\n output.setNumber(1,twice(input.getNumber(1)))\nend";
    let result = compile(
        source,
        &ApiCompileOptions {
            source_map: Some(true),
            numeric_mode: Some(ApiNumericMode::Exact),
            zero_cost_newlines: Some(false),
            ..Default::default()
        },
    );
    assert!(result.ok);
    let code = result.code.unwrap();
    let data = validate(&code, &result.map.unwrap()).unwrap();
    let point = code.find("*2").unwrap() + 1;
    let mapping = data
        .mappings
        .iter()
        .find(|m| m.start <= point && point < m.end)
        .unwrap();
    let origin = &data.origins[mapping.origin.unwrap() as usize];
    let span = origin.primary.unwrap();
    assert_eq!(&source[span.start..span.end], "2");
    assert!(
        !mapping.inline_contexts.is_empty(),
        "leaf discarded inline context"
    );
    assert!(mapping.inline_contexts.iter().any(|i| {
        let context = &data.contexts[*i as usize];
        &source[context.call_site.start..context.call_site.end] == "twice(input.getNumber(1))"
    }));
    assert!(data
        .origins
        .iter()
        .flat_map(|o| o.related.iter())
        .any(|id| data.relations[*id as usize].role == RelationRole::ParameterUse));
}
#[test]
fn explicit_removals_are_recorded_without_claiming_every_missing_token_is_dead() {
    let source="function onTick()local unused=99 if false then output.setNumber(2,123)end output.setNumber(1,7)end";
    let (code, _, details) = mapped(source);
    assert!(!code.contains("unused"));
    assert!(!code.contains("123"));
    assert!(details.dispositions.iter().any(|d| {
        &source[d.original.start..d.original.end] == "local unused=99"
            && details.reasons[d.reason as usize].operation == ReasonOperation::Remove
    }));
    assert!(details
        .dispositions
        .iter()
        .all(|d| !details.reasons[d.reason as usize].code.is_empty()));
}
#[test]
fn same_length_wrong_code_and_edited_snapshots_or_metadata_are_rejected() {
    let (code, map, _) = mapped("function onTick()output.setNumber(1,7)end");
    assert!(validate(&code.replace('7', "8"), &map).is_err());
    for path in ["snapshot", "schema", "reason", "mappings", "producer"] {
        let mut changed: Value = serde_json::from_str(&map).unwrap();
        match path {
            "snapshot" => changed["sourcesContent"][0] = "function onTick()end".into(),
            "schema" => changed["x_storm"]["schemaVersion"] = 999.into(),
            "reason" => changed["x_storm"]["reasons"] = serde_json::json!([]),
            "mappings" => changed["mappings"] = "AAAA".into(),
            "producer" => changed["x_storm"]["producer"]["version"] = "another".into(),
            _ => unreachable!(),
        }
        // Editing an already empty reason array is not a mutation; use an invalid record.
        if path == "reason" {
            changed["x_storm"]["reasons"] = serde_json::json!([{"invalid":true}]);
        }
        assert!(
            validate(&code, &changed.to_string()).is_err(),
            "accepted changed {path}"
        );
    }
}
#[test]
fn renamed_bindings_keep_original_names_but_generated_helper_stays_unmapped() {
    let source="function onTick()local throttle=input.getNumber(1)output.setNumber(1,throttle)output.setNumber(2,throttle*2)end";
    let (_, _, data) = mapped(source);
    assert!(data
        .origins
        .iter()
        .any(|o| o.name.as_deref() == Some("throttle")));
    assert!(data
        .origins
        .iter()
        .any(|o| o.kind == OriginKind::Synthetic && o.primary.is_none()));
    assert!(data
        .origins
        .iter()
        .filter(|o| o.kind == OriginKind::Synthetic)
        .all(|o| o.primary.is_none()));
}

fn project(main: &str, lib: &str) -> storm_lua_analysis::project::LuaProject {
    storm_lua_analysis::project::LuaProject {
        entry: "main".into(),
        modules: std::collections::BTreeMap::from([
            ("main".into(), main.into()),
            ("lib".into(), lib.into()),
        ]),
        ambient: Default::default(),
    }
}
#[test]
fn regular_and_lifeboat_projects_compose_source_ranges_before_encoding() {
    use crate::public_api::{compile_lifeboat, compile_project, ApiProjectCompileOptions};
    for lifeboat in [false, true] {
        let p = if lifeboat {
            project(
                "require('lib')\nfunction onTick()output.setNumber(1,twice(input.getNumber(1)))end",
                "function twice(value)return value*2 end",
            )
        } else {
            project("local twice=require('lib')\nfunction onTick()output.setNumber(1,twice(input.getNumber(1)))end","return function(value)return value*2 end")
        };
        for minify in [false, true] {
            let options = ApiProjectCompileOptions {
                compile: ApiCompileOptions {
                    source_map: Some(true),
                    zero_cost_newlines: Some(false),
                    ..Default::default()
                },
                minify: Some(minify),
            };
            let result = if lifeboat {
                compile_lifeboat(&p, &options)
            } else {
                compile_project(&p, &options)
            };
            assert!(result.ok, "{:?}", result.diagnostics);
            let code = result.code.unwrap();
            let map = result.map.unwrap();
            let details = validate(&code, &map).unwrap();
            let plain_options = ApiProjectCompileOptions {
                compile: ApiCompileOptions {
                    source_map: Some(false),
                    ..options.compile
                },
                minify: Some(minify),
            };
            let plain = if lifeboat {
                compile_lifeboat(&p, &plain_options)
            } else {
                compile_project(&p, &plain_options)
            };
            assert_eq!(plain.code.as_deref(), Some(code.as_str()));
            let json: Value = serde_json::from_str(&map).unwrap();
            let sources = json["sources"].as_array().unwrap();
            let lib = sources.iter().position(|name| name == "lib.lua").unwrap();
            let original = &p.modules["lib"];
            assert_eq!(json["sourcesContent"][lib], original.as_str());
            let expected = storm_lua_syntax::source_position::LineIndex::new(original)
                .utf16_position(original.find('2').unwrap())
                .unwrap();
            let standard = sourcemap::SourceMap::from_slice(map.as_bytes()).unwrap();
            assert!(
                standard.tokens().any(|t| t.get_source() == Some("lib.lua")
                    && (t.get_src_line(), t.get_src_col()) == expected),
                "literal source anchor missing for minify={minify} lifeboat={lifeboat}"
            );
            for origin in &details.origins {
                if let Some(span) = origin.primary {
                    let text = json["sourcesContent"][span.source as usize]
                        .as_str()
                        .unwrap();
                    assert!(text.get(span.start..span.end).is_some());
                }
            }
        }
    }
}
#[test]
fn linked_identity_early_exit_splits_copied_ranges_and_generated_glue() {
    use crate::public_api::{compile_project, ApiProjectCompileOptions};
    let p = project(
        "local f=require('lib');function onTick()output.setNumber(1,f(3))end",
        "return function(v)return v+1 end",
    );
    let result = compile_project(
        &p,
        &ApiProjectCompileOptions {
            compile: ApiCompileOptions {
                source_map: Some(true),
                target_size: Some(8192),
                ..Default::default()
            },
            minify: Some(true),
        },
    );
    assert!(result.ok, "{:?}", result.diagnostics);
    let code = result.code.unwrap();
    let map = result.map.unwrap();
    let data = validate(&code, &map).unwrap();
    assert!(data.mappings.iter().any(|m| m.copied.is_some()));
    assert!(data.mappings.iter().any(|m| m
        .origin
        .is_some_and(|id| data.origins[id as usize].kind == OriginKind::Synthetic)));
    let sm = sourcemap::SourceMap::from_slice(map.as_bytes()).unwrap();
    for token in sm.tokens() {
        if let Some(name) = token.get_source() {
            assert!(["main.lua", "lib.lua"].contains(&name));
        }
    }
}
#[test]
fn unicode_crlf_offsets_and_same_line_link_boundaries_keep_original_columns() {
    use crate::public_api::{compile_project, ApiProjectCompileOptions};
    use storm_lua_syntax::source_position::LineIndex;
    let p=project("local banner='😀雪';local f=require('lib');function onTick()output.setNumber(1,f())end\r\n","return function()\r\n local title='あ😀';return 7\r\nend");
    for minify in [false, true] {
        let result = compile_project(
            &p,
            &ApiProjectCompileOptions {
                compile: ApiCompileOptions {
                    source_map: Some(true),
                    ..Default::default()
                },
                minify: Some(minify),
            },
        );
        assert!(result.ok, "{:?}", result.diagnostics);
        let code = result.code.unwrap();
        let map = result.map.unwrap();
        validate(&code, &map).unwrap();
        let raw: Value = serde_json::from_str(&map).unwrap();
        assert!(raw["sourcesContent"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == &p.modules["lib"]));
        if !minify {
            let point = code.find("return 7").unwrap();
            let (line, col) = LineIndex::new(&code).utf16_position(point).unwrap();
            let sm = sourcemap::SourceMap::from_slice(map.as_bytes()).unwrap();
            let token = sm.lookup_token(line, col).unwrap();
            assert_eq!(token.get_source(), Some("lib.lua"));
            let at = p.modules["lib"].find("return 7").unwrap();
            let expected = LineIndex::new(&p.modules["lib"])
                .utf16_position(at)
                .unwrap();
            assert_eq!((token.get_src_line(), token.get_src_col()), expected);
        }
    }
}
#[test]
fn empty_sources_lexical_paths_and_removed_lifeboat_sections_are_valid() {
    use crate::public_api::{compile_lifeboat, compile_project, ApiProjectCompileOptions};
    let (code, _, details) = mapped("");
    assert_eq!(code, "");
    assert!(details.mappings.is_empty());
    let source = "local _ENV=_ENV\nfunction onTick() output.setNumber(1,7) end";
    let (code, map, data) = mapped(source);
    assert_eq!(
        data.compilation["selection"]["criterion"],
        "conservative-token-only"
    );
    validate(&code, &map).unwrap();
    let p = project(
        "require('lib');function onTick()output.setNumber(1,7)end",
        "",
    );
    let options = ApiProjectCompileOptions {
        compile: ApiCompileOptions {
            source_map: Some(true),
            ..Default::default()
        },
        minify: Some(false),
    };
    for r in [
        compile_project(&p, &options),
        compile_lifeboat(&p, &options),
    ] {
        assert!(r.ok, "{:?}", r.diagnostics);
        let map = r.map.unwrap();
        validate(r.code.as_deref().unwrap(), &map).unwrap();
        let value: Value = serde_json::from_str(&map).unwrap();
        assert!(value["sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s == "lib.lua"));
    }
    let p=project("---@section __LB_SIMULATOR_ONLY__\nprint('😀')\n---@endsection\nfunction onTick()output.setNumber(1,7)end","");
    let result = compile_lifeboat(&p, &options);
    assert!(result.ok);
    let map = result.map.unwrap();
    let code = result.code.unwrap();
    let data = validate(&code, &map).unwrap();
    assert!(!code.contains("print"));
    for m in &data.mappings {
        if let Some(span) = m.copied {
            assert!(!p.modules["main"][span.start..span.end].contains("print"));
        }
    }
}
#[test]
fn same_filename_different_snapshots_keep_distinct_source_indices() {
    use storm_lua_syntax::provenance::GeneratedOrigins;
    use storm_lua_syntax::{parse_source_with_origins, Ast, Node, Printer};
    let (a, _) = parse_source_with_origins("same.lua", "return 1").unwrap();
    let (b, _) = parse_source_with_origins("same.lua", "return 2").unwrap();
    let mut target = Ast::new();
    target.nodes = a.nodes.empty_like();
    let first = target.num("1".into());
    let second = target.num("2".into());
    let one = a
        .nodes
        .iter()
        .position(|n| matches!(n, Node::Num(_)))
        .unwrap() as u32;
    let two = b
        .nodes
        .iter()
        .position(|n| matches!(n, Node::Num(_)))
        .unwrap() as u32;
    target.nodes.derive_from(first, &a.nodes, one, "test-copy");
    target.nodes.derive_from(second, &b.nodes, two, "test-copy");
    let ret = target.push(Node::Return(vec![first, second]));
    target.nodes.mark_synthetic(ret, "test-return");
    let root = target.push(Node::Block(vec![ret]));
    target.nodes.mark_synthetic(root, "test-block");
    let output = Printer::new(&target, false).output_with_positions(root);
    let origins = GeneratedOrigins::from_print(&target, &output).unwrap();
    let map =
        crate::optimized_source_map::encode(&output.code, &origins, serde_json::json!({})).unwrap();
    let details = validate(&output.code, &map).unwrap();
    assert_eq!(details.sources.len(), 2);
    assert_ne!(details.sources[0].sha256, details.sources[1].sha256);
    let raw: Value = serde_json::from_str(&map).unwrap();
    assert_eq!(raw["sources"], serde_json::json!(["same.lua", "same.lua"]));
}
#[test]
fn normalization_reason_is_retained_alongside_earlier_derivations() {
    let (code, _, data) = mapped("function onTick()output.setNumber(1,0x10)end");
    assert!(code.contains("16"));
    assert!(data
        .reasons
        .iter()
        .any(
            |r| ["numeric-literal-spelling", "constant-folding"].contains(&r.code.as_ref())
                && r.before.as_deref() == Some("number:0x10")
                && r.after.as_deref() == Some("number:16")
        ));
}

#[test]
fn common_argument_decision_has_roles_and_observed_call_count() {
    let source = "local function add(value,offset)return value+offset end a=add(1,200)b=add(3,200)";
    let options = ApiCompileOptions {
        source_map: Some(true),
        pass_toggles: storm_lua_minify::pass_ids::OPTIMIZATION_PASS_IDS
            .iter()
            .map(|id| (id.to_string(), *id == "constant-argument-specialization"))
            .collect(),
        ..Default::default()
    };
    let result = compile(source, &options);
    assert!(result.ok);
    let code = result.code.unwrap();
    let data = validate(&code, &result.map.unwrap()).unwrap();
    let reason = data
        .reasons
        .iter()
        .find(|r| {
            r.basis.as_deref()
                == Some("same-literal-at-every-analyzed-call-and-unmodified-parameter")
        })
        .unwrap();
    assert!(reason
        .facts
        .iter()
        .any(|f| f.key.as_ref() == "analyzedCallSites" && f.value.as_ref() == "2"));
    assert!(reason
        .facts
        .iter()
        .any(|f| f.key.as_ref() == "value" && f.value.as_ref() == "number:200"));
    assert!(data
        .relations
        .iter()
        .any(|r| r.role == RelationRole::Argument && &source[r.span.start..r.span.end] == "200"));
    assert!(data
        .relations
        .iter()
        .any(|r| r.role == RelationRole::ParameterUse
            && &source[r.span.start..r.span.end] == "offset"));
}
#[test]
fn retained_helpers_do_not_invent_inline_contexts() {
    let source="local function inner(v)return v*2 end local function outer(v)return inner(v)+1 end function onTick()output.setNumber(1,outer(input.getNumber(1)))end";
    let result = compile(
        source,
        &ApiCompileOptions {
            source_map: Some(true),
            numeric_mode: Some(ApiNumericMode::Exact),
            zero_cost_newlines: Some(false),
            ..Default::default()
        },
    );
    assert!(result.ok);
    let code = result.code.unwrap();
    let data = validate(&code, &result.map.unwrap()).unwrap();
    // The current optimizer retains these helpers. A source relationship must
    // not be presented as an expansion that never actually happened.
    assert!(code.match_indices("function").count() > 1);
    let at = code.find("*2").unwrap() + 1;
    let mapping = data
        .mappings
        .iter()
        .find(|m| m.start <= at && at < m.end)
        .unwrap();
    assert!(mapping.inline_contexts.is_empty());
    let span = data.origins[mapping.origin.unwrap() as usize]
        .primary
        .unwrap();
    assert_eq!(&source[span.start..span.end], "2");
}

#[test]
fn effective_numeric_defaults_are_not_misreported_as_one_shared_tolerance() {
    let (_, _, data) = mapped("function onTick()output.setNumber(1,2*3)end");
    assert_eq!(
        data.compilation["effectiveNumericTolerances"]["constantFolding"]["abs"],
        format!("{:016x}", 1e-12f64.to_bits())
    );
    assert_eq!(
        data.compilation["effectiveNumericTolerances"]["literalApproximationBeforeCaps"]["abs"],
        format!("{:016x}", 1e-6f64.to_bits())
    );
    assert_eq!(
        data.compilation["effectivePassToggles"]
            .as_object()
            .unwrap()
            .len(),
        67
    );
}
#[test]
fn invalid_source_name_is_a_label_error_not_an_unknown_optimizer_pass() {
    for name in ["", "bad\0name"] {
        let result = compile(
            "return 1",
            &ApiCompileOptions {
                source_map: Some(true),
                source_name: Some(name.into()),
                ..Default::default()
            },
        );
        assert!(!result.ok);
        assert_eq!(result.diagnostics[0].code, "invalid-source-name");
        assert!(result.map.is_none());
    }
}

fn resealed(value: &mut Value) -> String {
    use sha2::{Digest, Sha256};
    value["x_storm"]["integrity"] = "".into();
    let hash = format!("{:x}", Sha256::digest(serde_json::to_vec(value).unwrap()));
    value["x_storm"]["integrity"] = hash.into();
    value.to_string()
}
#[test]
fn recomputed_checksum_does_not_hide_structural_errors_or_disagreeing_maps() {
    let (code, map, _) = mapped("function onTick()output.setNumber(1,2*3)end");
    for case in [
        "standard",
        "sourceRoot",
        "unused-relation",
        "unused-reason",
        "duplicate-fact",
        "context",
        "mapping-field",
        "reason-field",
    ] {
        let mut value: Value = serde_json::from_str(&map).unwrap();
        match case {
            "standard"=>value["mappings"]="AAAA".into(),
            "sourceRoot"=>value["sourceRoot"]="https://incorrect.invalid/".into(),
            "unused-relation"=>value["x_storm"]["relations"].as_array_mut().unwrap().push(serde_json::json!({"role":"contribution","span":{"source":999,"start":0,"end":1}})),
            "unused-reason"=>value["x_storm"]["reasons"].as_array_mut().unwrap().push(serde_json::json!({"code":"","operation":"rewrite","before":null,"after":null,"basis":null,"facts":[]})),
            "duplicate-fact"=>value["x_storm"]["reasons"][0]["facts"]=serde_json::json!([{"key":"value","value":"1"},{"key":"value","value":"2"}]),
            "context"=>value["x_storm"]["contexts"].as_array_mut().unwrap().push(serde_json::json!({"definition":{"source":0,"start":0,"end":999999},"callSite":{"source":0,"start":0,"end":1}})),
            "mapping-field"=>value["x_storm"]["mappings"][0]["imaginary"] = true.into(),
            "reason-field"=>value["x_storm"]["reasons"][0]["imaginary"] = true.into(),
            _=>unreachable!(),
        }
        assert!(
            validate(&code, &resealed(&mut value)).is_err(),
            "accepted {case}"
        );
    }
}
#[test]
fn completely_eliminated_program_keeps_source_disposition_and_standard_empty_map() {
    let source = "local unused=99";
    let (code, map, data) = mapped(source);
    assert_eq!(code, "");
    assert!(data.mappings.is_empty());
    assert!(data
        .dispositions
        .iter()
        .any(|d| &source[d.original.start..d.original.end] == source));
    assert!(sourcemap::SourceMap::from_slice(map.as_bytes())
        .unwrap()
        .tokens()
        .all(|t| t.get_source().is_none()));
}
#[test]
fn reformatting_map_json_preserves_fingerprint_validation() {
    let (code, map, original) = mapped("function onTick()output.setNumber(1,7)end");
    let value: Value = serde_json::from_str(&map).unwrap();
    let pretty = serde_json::to_string_pretty(&value).unwrap();
    assert_eq!(
        validate(&code, &pretty).unwrap().integrity,
        original.integrity
    );
}

#[test]
fn removals_include_the_actual_elimination_checks_not_just_last_traversal() {
    for numeric_mode in [ApiNumericMode::Exact, ApiNumericMode::Tolerant] {
        let source = "local unused=99 if not true then output.setNumber(1,7)end";
        let result = compile(
            source,
            &ApiCompileOptions {
                source_map: Some(true),
                numeric_mode: Some(numeric_mode),
                ..Default::default()
            },
        );
        assert!(result.ok, "{:?}", result.diagnostics);
        let code = result.code.unwrap();
        assert_eq!(code, "");
        let data = validate(&code, &result.map.unwrap()).unwrap();
        let removed = data
            .dispositions
            .iter()
            .map(|d| &data.reasons[d.reason as usize])
            .collect::<Vec<_>>();
        assert!(removed.iter().any(|r| r.basis.as_deref()
            == Some("all-declared-bindings-unread-unwritten-and-initializers-movable")));
        assert!(removed.iter().any(|r| r.basis.as_deref()
            == Some("lua-truthiness-of-known-conditions")
            && r.facts
                .iter()
                .any(|f| f.key.as_ref() == "armTruthiness" && f.value.as_ref() == "false")));
    }
}
