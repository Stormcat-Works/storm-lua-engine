//! Origin propagation regressions use the real passes and candidate scheduler.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use crate::config::{CompileOptions, PropertyConfig, PropertyMode};
use crate::pass_ids::OPTIMIZATION_PASS_IDS;
use crate::search::{
    compile_code, evaluate_candidate, prepare_search, select_best, CandidateBatch, CandidateJob,
    SearchContext,
};
use storm_lua_syntax::provenance::{GeneratedOrigins, OriginKind};
use storm_lua_syntax::{parse_source_with_origins, NameSite, Node, Printer};

fn only(passes: &[&'static str]) -> CompileOptions {
    let mut options = CompileOptions {
        origin_source: Some("controller.lua".into()),
        ..Default::default()
    };
    options.pass_toggles = OPTIMIZATION_PASS_IDS
        .iter()
        .map(|&id| (id, false))
        .collect();
    for id in passes {
        options.pass_toggles.insert(id, true);
    }
    options
}
fn slice<'a>(
    origins: &'a GeneratedOrigins,
    origin: &storm_lua_syntax::provenance::Origin,
) -> &'a str {
    let span = origin.primary.unwrap();
    &origins.sources[span.source as usize].text[span.start..span.end]
}

#[test]
fn unchanged_real_search_produces_complete_origins_without_changing_output() {
    let source = "local longValue=1 output.setNumber(1,longValue)";
    for zero in [false, true] {
        let options = CompileOptions {
            zero_cost_newlines: zero,
            ..only(&[])
        };
        let mapped = compile_code(source, &options).unwrap();
        let plain = compile_code(
            source,
            &CompileOptions {
                origin_source: None,
                ..options
            },
        )
        .unwrap();
        assert_eq!(mapped.code, plain.code);
        assert_eq!(mapped.stats, plain.stats);
        assert!(plain.origins.is_none());
        let origins = mapped.origins.unwrap();
        assert_eq!(origins.unknown_bytes(), 0);
        assert_eq!(origins.sources[0].text.as_ref(), source);
        assert!(origins
            .origins
            .iter()
            .any(|origin| origin.name.as_deref() == Some("longValue")));
    }
}

#[test]
fn arithmetic_fold_maps_the_result_to_the_original_expression() {
    let source = "output.setNumber(1,2 * 3)";
    let result = compile_code(source, &only(&["constant-folding"])).unwrap();
    let origins = result.origins.unwrap();
    assert!(result.code.contains(",6)"), "{}", result.code);
    assert_eq!(origins.unknown_bytes(), 0);
    assert!(origins.mappings.iter().any(|mapping| {
        mapping.origin.is_some_and(|id| {
            let origin = &origins.origins[id as usize];
            &result.code[mapping.start..mapping.end] == "6"
                && slice(&origins, origin) == "2 * 3"
                && origin.kind == OriginKind::Derived
        })
    }));
}

#[test]
fn scope_rename_preserves_each_original_name_occurrence() {
    let source = "local longValue=input.getNumber(1) output.setNumber(1,longValue)";
    let result = compile_code(source, &only(&["scope-renaming"])).unwrap();
    assert!(!result.code.contains("longValue"));
    let origins = result.origins.unwrap();
    assert_eq!(origins.unknown_bytes(), 0);
    let names = origins
        .origins
        .iter()
        .filter(|origin| origin.name.as_deref() == Some("longValue"))
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 2);
    assert_ne!(names[0].primary, names[1].primary);
    for name in names {
        assert_eq!(slice(&origins, name), "longValue");
    }
}

#[test]
fn property_specialization_attributes_new_literals_to_the_call_site() {
    let source = "output.setNumber(1,property.getNumber('Gain'))";
    let mut options = only(&[]);
    options.property = Some(PropertyConfig {
        mode: PropertyMode::Hardcode,
        numbers: Some(std::collections::BTreeMap::from([("Gain".into(), 2.5)])),
        bools: None,
        texts: None,
    });
    let result = compile_code(source, &options).unwrap();
    assert_eq!(result.property_reads_hardcoded, 1);
    let origins = result.origins.unwrap();
    assert_eq!(origins.unknown_bytes(), 0);
    assert!(origins
        .origins
        .iter()
        .any(|origin| origin.kind == OriginKind::Derived
            && slice(&origins, origin) == "property.getNumber('Gain')"));
}

#[test]
fn removing_parentheses_retains_the_selected_child_origin() {
    let source = "output.setNumber(1,(((value))))";
    let result = compile_code(source, &only(&["redundant-parentheses-elimination"])).unwrap();
    let origins = result.origins.unwrap();
    assert_eq!(origins.unknown_bytes(), 0);
    assert!(!result.code.contains("(("));
    let origin = origins
        .origins
        .iter()
        .find(|origin| origin.name.as_deref() == Some("value"))
        .unwrap();
    assert_eq!(slice(&origins, origin), "value");
}

#[test]
fn merging_locals_keeps_each_name_and_both_contributing_declarations() {
    let source = "local first=input.getNumber(1) local second=input.getNumber(2) output.setNumber(1,first+second)";
    let (mut ast, root) = parse_source_with_origins("controller.lua", source).unwrap();
    let result = crate::passes::adjacent_locals::pack_adjacent_locals(&mut ast, root, false);
    let printed = Printer::new(&ast, true).output_with_positions(result.root);
    assert!(
        printed.code.contains("local first,second="),
        "{}",
        printed.code
    );
    let origins = GeneratedOrigins::from_print(&ast, &printed).unwrap();
    assert_eq!(origins.unknown_bytes(), 0);
    let local = ast
        .nodes
        .iter()
        .position(|node| matches!(node, Node::Local(names,_) if names.len()==2))
        .unwrap() as u32;
    assert_eq!(
        ast.nodes
            .name_origin(local, NameSite::Binding(0))
            .unwrap()
            .name
            .as_deref(),
        Some("first")
    );
    assert_eq!(
        ast.nodes
            .name_origin(local, NameSite::Binding(1))
            .unwrap()
            .name
            .as_deref(),
        Some("second")
    );
    let origin = ast.nodes.origin(local).unwrap();
    assert_eq!(origin.related.len(), 1);
    let second = origin.related[0];
    assert_eq!(
        &source[second.start..second.end],
        "local second=input.getNumber(2)"
    );
}

#[test]
fn worker_jobs_and_batches_roundtrip_without_metadata_affecting_selection() {
    let source = "local longValue=input.getNumber(1) output.setNumber(1,longValue*2)";
    let options = only(&[
        "scope-renaming",
        "constant-folding",
        "redundant-parentheses-elimination",
    ]);
    let expected = compile_code(source, &options).unwrap();
    let (context, jobs) = prepare_search(source, &options).unwrap();
    let context: SearchContext =
        serde_json::from_str(&serde_json::to_string(&context).unwrap()).unwrap();
    let mut batches = Vec::new();
    for job in jobs.into_iter().rev() {
        let job: CandidateJob =
            serde_json::from_str(&serde_json::to_string(&job).unwrap()).unwrap();
        let batch = evaluate_candidate(job).unwrap();
        batches.push(
            serde_json::from_str::<CandidateBatch>(&serde_json::to_string(&batch).unwrap())
                .unwrap(),
        );
    }
    let actual = select_best(context, batches).unwrap();
    assert_eq!(actual.code, expected.code);
    assert_eq!(actual.stats, expected.stats);
    assert_eq!(actual.origins, expected.origins);
}

#[test]
fn full_pipeline_target_miss_and_untracked_search_keep_identical_candidates() {
    let source = "local value=input.getNumber(1) function onTick() if value>1 then output.setNumber(1,value*2)else output.setNumber(1,0)end end";
    let options = CompileOptions {
        origin_source: Some("full.lua".into()),
        ..Default::default()
    };
    let tracked = compile_code(source, &options).unwrap();
    let normal = compile_code(
        source,
        &CompileOptions {
            origin_source: None,
            ..options.clone()
        },
    )
    .unwrap();
    let target = compile_code(
        source,
        &CompileOptions {
            target_size: Some(0),
            search_mode: crate::config::SearchMode::Fast,
            search_beam_width: 1,
            ..options
        },
    )
    .unwrap();
    assert_eq!(tracked.code, normal.code);
    assert_eq!(target.code, normal.code);
    assert_eq!(tracked.stats.candidate_sizes, normal.stats.candidate_sizes);
    assert_eq!(target.origins, tracked.origins);
    assert!(!target.stats.target_met);
    assert!(tracked.origins.is_some());
}

#[test]
fn source_early_exit_and_extended_lexical_route_return_matching_origins() {
    let source = "function onTick() output.setNumber(1,1) end";
    let result = compile_code(
        source,
        &CompileOptions {
            target_size: Some(source.len()),
            ..only(&[])
        },
    )
    .unwrap();
    assert_eq!(result.code, source);
    assert!(result.stats.stopped_early);
    assert_eq!(result.origins.unwrap().unknown_bytes(), 0);
    let options = CompileOptions {
        environment: storm_lua_spec::environment::EnvironmentProfile::Extended,
        ..only(&[])
    };
    let source = "local value=1\r\n_ENV.print(value)";
    let result = compile_code(source, &options).unwrap();
    assert_eq!(result.stats.winner_structural, "lexical");
    let origins = result.origins.unwrap();
    for mapping in &origins.mappings {
        if let Some(id) = mapping.origin {
            assert_eq!(
                &result.code[mapping.start..mapping.end],
                slice(&origins, &origins.origins[id as usize])
            );
        }
    }
}

#[test]
fn binary_worker_protocol_retains_tracked_metadata_and_validates_ranges() {
    let source = "local number=input.getNumber(1) output.setNumber(1,number+2*3)";
    let options = only(&["scope-renaming", "constant-folding"]);
    let expected = compile_code(source, &options).unwrap();
    let (context, jobs) = prepare_search(source, &options).unwrap();
    let context: SearchContext =
        bincode::deserialize(&bincode::serialize(&context).unwrap()).unwrap();
    let mut batches = Vec::new();
    for job in jobs.into_iter().rev() {
        let job: CandidateJob = bincode::deserialize(&bincode::serialize(&job).unwrap()).unwrap();
        let batch = evaluate_candidate(job).unwrap();
        batches.push(
            bincode::deserialize::<CandidateBatch>(&bincode::serialize(&batch).unwrap()).unwrap(),
        );
    }
    let actual = select_best(context, batches).unwrap();
    assert_eq!(actual.code, expected.code);
    assert_eq!(actual.stats, expected.stats);
    assert_eq!(actual.origins, expected.origins);
    let mut invalid = actual.origins.unwrap();
    invalid.mappings[0].end += 1;
    assert!(invalid.validate_for_code(&actual.code).is_err());
}

#[test]
fn target_continuation_carries_completed_origin_batches_without_recomputing() {
    let source = "local total=0 function onTick() local x=input.getNumber(1) local y=input.getNumber(2) total=total+x+y output.setNumber(1,total+x+x+y) end function onDraw() local w=screen.getWidth() screen.drawLine(0,0,w,w) screen.drawText(1,1,total) end";
    let options = CompileOptions {
        origin_source: Some("worker.lua".into()),
        target_size: Some(0),
        ..Default::default()
    };
    let expected = compile_code(source, &options).unwrap();
    let mut attempt = crate::search::try_satisficing(source, &options, None).unwrap();
    if let Some((context, jobs)) = crate::search::take_satisficing_fallback(&mut attempt) {
        let context: SearchContext =
            bincode::deserialize(&bincode::serialize(&context).unwrap()).unwrap();
        let mut batches = Vec::new();
        for job in jobs.into_iter().rev() {
            let job: CandidateJob =
                bincode::deserialize(&bincode::serialize(&job).unwrap()).unwrap();
            let batch = evaluate_candidate(job).unwrap();
            batches.push(
                bincode::deserialize::<CandidateBatch>(&bincode::serialize(&batch).unwrap())
                    .unwrap(),
            );
        }
        let actual = select_best(context, batches).unwrap();
        assert_eq!(actual.code, expected.code);
        assert_eq!(actual.origins, expected.origins);
        assert_eq!(actual.stats.candidate_sizes, expected.stats.candidate_sizes);
    } else {
        panic!("fixture must exercise a real remaining-job continuation");
    }
}

#[test]
fn copy_through_paths_do_not_erase_an_untouched_program() {
    // This fixture is a copy-through regression, not a claim that every
    // transformation in every registered pass has complete attribution.
    let source = "local signal=input.getNumber(1) output.setNumber(1,signal+2 * 3)";
    for &pass in OPTIMIZATION_PASS_IDS {
        let result = compile_code(source, &only(&[pass])).unwrap();
        let origins = result.origins.unwrap();
        assert_eq!(
            origins.unknown_bytes(),
            0,
            "origin lost through {pass}: {}",
            result.code
        );
    }
}

#[test]
fn a_full_search_preserves_moved_definition_and_related_use_sites() {
    let source = "local signal=input.getNumber(1) output.setNumber(1,signal+2 * 3)";
    let result = compile_code(
        source,
        &CompileOptions {
            origin_source: Some("full.lua".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let origins = result.origins.unwrap();
    assert_eq!(result.code, "output.setNumber(1,input.getNumber(1)+6)");
    assert_eq!(origins.unknown_bytes(), 0);
    let constant = origins
        .mappings
        .iter()
        .find(|mapping| &result.code[mapping.start..mapping.end] == "6")
        .unwrap();
    assert_eq!(
        slice(
            &origins,
            &origins.origins[constant.origin.unwrap() as usize]
        ),
        "2 * 3"
    );
    assert!(origins.origins.iter().any(|origin| {
        origin
            .primary
            .is_some_and(|_| slice(&origins, origin) == "input.getNumber(1)")
            && origin
                .related
                .iter()
                .any(|range| &source[range.start..range.end] == "signal")
    }));
}

#[test]
fn removal_of_the_first_local_binding_moves_the_survivor_name_slot() {
    let source = "local unused,kept=1,input.getNumber(1) output.setNumber(1,kept)";
    let result = compile_code(source, &only(&["dead-local-elimination"])).unwrap();
    let origins = result.origins.unwrap();
    assert!(!result.code.contains("unused"));
    assert_eq!(origins.unknown_bytes(), 0);
    let names = origins
        .origins
        .iter()
        .filter(|origin| origin.name.as_deref() == Some("kept"))
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 2);
    assert!(names.iter().all(|origin| slice(&origins, origin) == "kept"));
    assert!(origins
        .origins
        .iter()
        .all(|origin| origin.name.as_deref() != Some("unused")));
}

#[test]
fn root_local_globalization_copies_function_binding_to_new_name_node() {
    let source =
        "local function helper(argument) return argument+1 end output.setNumber(1,helper(2))";
    let result = compile_code(source, &only(&["root-local-globalization"])).unwrap();
    assert!(result.code.starts_with("function helper"));
    let origins = result.origins.unwrap();
    assert_eq!(origins.unknown_bytes(), 0);
    let names = origins
        .origins
        .iter()
        .filter(|origin| origin.name.as_deref() == Some("helper"))
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 2);
    assert_ne!(names[0].primary, names[1].primary);
}

#[test]
fn synthesized_api_alias_definitions_are_not_claimed_as_original_lines() {
    let source = format!(
        "function onDraw(){}end",
        (0..24)
            .map(|n| format!("screen.drawLine({n},0,{n},10) "))
            .collect::<String>()
    );
    let result = compile_code(&source, &only(&["api-alias-optimization"])).unwrap();
    assert!(
        !result.api_aliases.is_empty(),
        "fixture must create an alias definition"
    );
    let origins = result.origins.unwrap();
    assert_eq!(origins.unknown_bytes(), 0);
    assert!(origins
        .origins
        .iter()
        .any(|origin| origin.kind == OriginKind::Synthetic));
    for origin in origins
        .origins
        .iter()
        .filter(|origin| origin.kind == OriginKind::Synthetic)
    {
        assert!(origin.primary.is_none() && origin.name.is_none());
    }
    assert!(origins.origins.iter().any(|origin| origin
        .primary
        .is_some_and(|_| slice(&origins, origin) == "screen.drawLine")));
}

#[test]
fn induction_folding_retains_the_induction_name_instead_of_call_parent() {
    let source = "for index=1,3 do output.setNumber(1,math.floor(index)) end";
    let result = compile_code(source, &only(&["integer-loop-call-folding"])).unwrap();
    assert!(!result.code.contains("floor"));
    let origins = result.origins.unwrap();
    assert_eq!(origins.unknown_bytes(), 0);
    let names = origins
        .origins
        .iter()
        .filter(|origin| origin.name.as_deref() == Some("index"))
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 2);
    assert!(names
        .iter()
        .all(|origin| slice(&origins, origin) == "index"));
}

#[test]
fn every_registered_pass_has_an_explicit_audit_status() {
    let inventory: serde_json::Value = serde_json::from_str(include_str!(
        "../../../docs/design/source-provenance-pass-audit.json"
    ))
    .unwrap();
    let entries = inventory["passes"].as_array().unwrap();
    let ids = entries
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(ids.as_slice(), OPTIMIZATION_PASS_IDS);
    for entry in entries {
        assert!(matches!(
            entry["status"].as_str(),
            Some("partial" | "pending" | "implemented")
        ));
        assert!(!entry["notes"].as_str().unwrap().is_empty());
    }
}

#[test]
fn removing_one_declaration_does_not_invalidate_unrelated_statement_origins() {
    let source = "local ignored=1 local kept=2 output.setNumber(1,kept)";
    let (mut ast, root) = parse_source_with_origins("remove.lua", source).unwrap();
    let Node::Block(statements) = ast.node(root) else {
        panic!("root block");
    };
    let removed = statements[0];
    let kept = statements[1];
    let before = ast.nodes.origin(kept).cloned();
    ast.nodes
        .retain_block_statements(|statement| statement != removed, "test-declaration-removal");
    assert_eq!(ast.nodes.origin(kept), before.as_ref());
    let printed = Printer::new(&ast, false).output_with_positions(root);
    assert!(!printed.code.contains("ignored"));
    let generated = GeneratedOrigins::from_print(&ast, &printed).unwrap();
    assert_eq!(generated.unknown_bytes(), 0);
}

#[test]
fn expression_inlining_retains_definition_arguments_and_call_site_separately() {
    let source = "local function twice(value) return value*2 end output.setNumber(1,twice(input.getNumber(1)))";
    for pass in [
        "expression-helper-inlining",
        "one-use-expression-helper-reversal",
    ] {
        let result = compile_code(source, &only(&[pass])).unwrap();
        assert!(
            !result.code.contains("function twice"),
            "fixture must inline under {pass}: {}",
            result.code
        );
        let origins = result.origins.unwrap();
        assert_eq!(origins.unknown_bytes(), 0, "{pass}: {}", result.code);
        assert!(
            origins.origins.iter().any(|origin| origin
                .primary
                .is_some_and(|_| slice(&origins, origin) == "value*2")),
            "inlined body origin missing: {pass}"
        );
        assert!(
            origins.origins.iter().any(|origin| origin
                .primary
                .is_some_and(|_| slice(&origins, origin) == "input.getNumber(1)")),
            "actual argument origin missing: {pass}"
        );
        assert!(
            origins.origins.iter().any(|origin| origin
                .related
                .iter()
                .any(|span| &source[span.start..span.end] == "twice(input.getNumber(1))")),
            "call-site origin missing: {pass}"
        );
    }
}
