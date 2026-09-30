//! Regression tests for complete (zero Unknown) origin coverage. A known origin
//! may be statement-level or explicitly synthetic; it is not exact character identity.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use crate::{pass::PassResult, passes};
use storm_lua_syntax::provenance::{GeneratedOrigins, OriginKind, OriginPrecision};
use storm_lua_syntax::{parse_source, parse_source_with_origins, Ast, Node, NodeId, Printer};

fn run(source: &str, pass: impl Fn(&mut Ast, NodeId) -> PassResult) -> (String, GeneratedOrigins) {
    let (mut tracked, root) = parse_source_with_origins("controller.lua", source).unwrap();
    let (mut plain, plain_root) = parse_source(source).unwrap();
    let result = pass(&mut tracked, root);
    let expected = pass(&mut plain, plain_root);
    let printed = Printer::new(&tracked, false).output_with_positions(result.root);
    assert_eq!(
        printed.code,
        Printer::new(&plain, false).output(expected.root)
    );
    let origins = GeneratedOrigins::from_print(&tracked, &printed).unwrap();
    origins.validate_for_code(&printed.code).unwrap();
    (printed.code, origins)
}
fn origin_at<'a>(
    code: &str,
    origins: &'a GeneratedOrigins,
    offset: usize,
) -> Option<&'a storm_lua_syntax::provenance::Origin> {
    assert!(offset < code.len());
    origins
        .mappings
        .iter()
        .find(|m| m.start <= offset && offset < m.end)
        .and_then(|m| m.origin)
        .map(|i| &origins.origins[i as usize])
}
fn source_text<'a>(source: &'a str, origin: &storm_lua_syntax::provenance::Origin) -> &'a str {
    let span = origin.primary.unwrap();
    &source[span.start..span.end]
}

#[test]
fn unchanged_assignments_and_separators_survive_unread_global_cleanup() {
    let source = "first=1 second=2 output.setNumber(1,first)output.setNumber(2,second)";
    let (code, origins) = run(source, passes::global_stores::remove_unread_global_stores);
    assert_eq!(origins.unknown_bytes(), 0);
    for prefix in ["first=", "second="] {
        let origin = origin_at(
            &code,
            &origins,
            code.find(prefix).unwrap() + prefix.len() - 1,
        )
        .unwrap();
        assert_eq!(origin.precision, OriginPrecision::Statement);
        assert!(source_text(source, origin).starts_with(prefix));
    }
    assert!(origins
        .origins
        .iter()
        .all(|o| o.kind != OriginKind::Synthetic));
}

#[test]
fn partial_unread_assignment_removal_keeps_surviving_statement_and_rhs() {
    let source = "unused,used=10,20 output.setNumber(1,used)";
    let (code, origins) = run(source, passes::global_stores::remove_unread_global_stores);
    assert_eq!(code, "used=20 output.setNumber(1,used)");
    assert_eq!(origins.unknown_bytes(), 0);
    assert_eq!(
        source_text(
            source,
            origin_at(&code, &origins, code.find('=').unwrap()).unwrap()
        ),
        "unused,used=10,20"
    );
    assert_eq!(
        source_text(
            source,
            origin_at(&code, &origins, code.find("20").unwrap()).unwrap()
        ),
        "20"
    );
    assert!(!origins
        .origins
        .iter()
        .any(|o| o.name.as_deref() == Some("unused")));
}

#[test]
fn final_store_cleanup_keeps_branch_keywords_and_surviving_assignment() {
    let source = "function f(cond)local x if cond then x=1 x=2 else do x=3 end end return x end";
    let (code, origins) = run(
        source,
        passes::final_stores::eliminate_overwritten_assignments,
    );
    assert!(!code.contains("x=1"));
    assert!(code.contains("x=2"));
    assert_eq!(origins.unknown_bytes(), 0);
    let branch = origin_at(&code, &origins, code.find("if ").unwrap()).unwrap();
    assert_eq!(
        source_text(source, branch),
        "if cond then x=1 x=2 else do x=3 end end"
    );
    let assignment = origin_at(&code, &origins, code.find("x=2").unwrap() + 1).unwrap();
    assert_eq!(source_text(source, assignment), "x=2");
    let wrapper = origin_at(&code, &origins, code.find("do ").unwrap()).unwrap();
    assert_eq!(source_text(source, wrapper), "do x=3 end");
}

#[test]
fn conditional_noop_traversals_do_not_erase_block_separator_origins() {
    let source = "function f(c)x=1 if c then x=false else x=true end return x end";
    for pass in [
        passes::conditionals::lower_conditional_calls,
        passes::conditionals::lower_conditional_assignments,
        passes::conditionals::lower_conditional_returns,
    ] {
        let (code, origins) = run(source, pass);
        assert_eq!(origins.unknown_bytes(), 0);
        for mapping in &origins.mappings {
            let origin = &origins.origins[mapping.origin.unwrap() as usize];
            if origin.precision == OriginPrecision::Group {
                assert!(
                    code[mapping.start..mapping.end]
                        .chars()
                        .all(|c| c.is_whitespace() || c == ';'),
                    "a block origin must not hide a child token"
                );
            }
        }
    }
}

#[test]
fn conditional_assignments_map_new_logical_operators_to_the_original_decision() {
    for source in [
        "if cond then result=1 else result=2 end",
        "if c then x=1 end",
        "if first then result=1 elseif second then result=2 else result=3 end",
    ] {
        let (code, origins) = run(source, passes::conditionals::lower_conditional_assignments);
        assert!(!code.starts_with("if "), "{code}");
        assert_eq!(origins.unknown_bytes(), 0);
        for (offset, _) in code
            .match_indices(" and ")
            .chain(code.match_indices(" or "))
        {
            let origin = origin_at(&code, &origins, offset + 1).unwrap();
            assert_eq!(origin.kind, OriginKind::Derived);
            assert_eq!(origin.precision, OriginPrecision::Statement);
            assert_eq!(source_text(source, origin), source);
        }
        let literal = origin_at(&code, &origins, code.find('1').unwrap()).unwrap();
        assert_eq!(source_text(source, literal), "1");
        assert_eq!(literal.precision, OriginPrecision::Expression);
    }
}

#[test]
fn conditional_calls_keep_operand_origins_separate_from_merged_control_flow() {
    for source in [
        "if cond then output.setNumber(1,2)else output.setNumber(1,3)end",
        "if cond then screen.drawLine(1,2,3,4)else screen.drawRectF(1,2,3,4)end",
    ] {
        let (code, origins) = run(source, passes::conditionals::lower_conditional_calls);
        assert!(!code.starts_with("if "), "{code}");
        assert_eq!(origins.unknown_bytes(), 0);
        let condition = origin_at(&code, &origins, code.find("cond").unwrap()).unwrap();
        assert_eq!(source_text(source, condition), "cond");
        let operator = origin_at(&code, &origins, code.find("and").unwrap()).unwrap();
        assert_eq!(source_text(source, operator), source);
        assert_eq!(
            operator.transformation.as_deref(),
            Some("conditional-call-lowering")
        );
    }
}

#[test]
fn guard_return_lowering_retains_fallback_as_a_distinct_contributing_statement() {
    let source = "if cond then return false end return 1";
    let (code, origins) = run(source, passes::conditionals::lower_conditional_returns);
    assert_eq!(origins.unknown_bytes(), 0);
    assert!(code.starts_with("return not cond"));
    let origin = origin_at(&code, &origins, 0).unwrap();
    assert_eq!(source_text(source, origin), "if cond then return false end");
    assert!(origin
        .related
        .iter()
        .any(|s| &source[s.start..s.end] == "return 1"));
    let condition = origin_at(&code, &origins, code.find("cond").unwrap()).unwrap();
    assert_eq!(source_text(source, condition), "cond");
}

#[test]
fn full_return_and_guard_chains_have_complete_derived_origins() {
    for source in [
        "if cond then return 1 else return 2 end",
        "if first then return 0 end;if second then return 1 end;return-1",
    ] {
        let (code, origins) = run(source, passes::conditionals::lower_conditional_returns);
        assert!(code.starts_with("return "));
        assert_eq!(origins.unknown_bytes(), 0);
        assert!(!origins
            .origins
            .iter()
            .any(|o| o.kind == OriginKind::Synthetic));
    }
}

#[test]
fn missing_condition_origin_stays_unknown_in_a_known_lowered_branch() {
    let source = "if cond then result=1 else result=2 end";
    let (mut ast, root) = parse_source_with_origins("unknown.lua", source).unwrap();
    let node = ast
        .nodes
        .iter()
        .position(|n| matches!(n,Node::Name(s) if ast.strings.get(*s)=="cond"))
        .unwrap();
    let original = ast.nodes[node].clone();
    ast.nodes[node] = original;
    let result = passes::conditionals::lower_conditional_assignments(&mut ast, root);
    let printed = Printer::new(&ast, false).output_with_positions(result.root);
    let origins = GeneratedOrigins::from_print(&ast, &printed).unwrap();
    assert_eq!(origins.unknown_bytes(), 4);
    assert!(origin_at(&printed.code, &origins, printed.code.find("cond").unwrap()).is_none());
    assert!(origin_at(&printed.code, &origins, printed.code.find("and").unwrap()).is_some());
}

#[test]
fn unread_cleanup_does_not_restore_a_previous_unannotated_assignment() {
    let source = "value=1 output.setNumber(1,value)";
    let (mut ast, root) = parse_source_with_origins("unknown.lua", source).unwrap();
    let id = ast
        .nodes
        .iter()
        .position(|n| matches!(n, Node::Assign(..)))
        .unwrap();
    let old = ast.nodes[id].clone();
    ast.nodes[id] = old;
    let result = passes::global_stores::remove_unread_global_stores(&mut ast, root);
    let printed = Printer::new(&ast, false).output_with_positions(result.root);
    let origins = GeneratedOrigins::from_print(&ast, &printed).unwrap();
    assert_eq!(origins.unknown_bytes(), 1);
    assert!(origin_at(&printed.code, &origins, printed.code.find('=').unwrap()).is_none());
}

#[test]
fn quotient_chain_noop_preserves_all_original_statement_boundaries() {
    let source =
        "first=1 second=2 function onTick()output.setNumber(1,first)output.setNumber(2,second)end";
    let (code, origins) = run(source, passes::decode_chains::fuse_quotient_chains);
    assert_eq!(code, source);
    assert_eq!(origins.unknown_bytes(), 0);
    for mapping in &origins.mappings {
        let o = &origins.origins[mapping.origin.unwrap() as usize];
        if o.precision == OriginPrecision::Group {
            assert!(code[mapping.start..mapping.end]
                .chars()
                .all(|c| c.is_whitespace() || c == ';'));
        }
    }
}

#[test]
fn quotient_product_keeps_both_original_divisor_occurrences() {
    let source = "value=value//10 value=value//10 output.setNumber(1,value)";
    let (code, origins) = run(source, passes::decode_chains::fuse_quotient_chains);
    assert_eq!(code, "value=value//100 output.setNumber(1,value)");
    assert_eq!(origins.unknown_bytes(), 0);
    let product = origin_at(&code, &origins, code.find("100").unwrap()).unwrap();
    let positions = source
        .match_indices("10")
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    assert_eq!(product.kind, OriginKind::Derived);
    assert_eq!(product.primary.unwrap().start, positions[0]);
    assert!(product
        .related
        .iter()
        .any(|s| s.start == positions[1] && &source[s.start..s.end] == "10"));
    let assignment = origin_at(&code, &origins, code.find('=').unwrap()).unwrap();
    assert_eq!(source_text(source, assignment), "value=value//10");
    assert!(assignment
        .related
        .iter()
        .any(|s| s.start == source.find(" value=").unwrap() + 1));
}

#[test]
fn initialization_then_division_keeps_the_initial_value_and_following_divisor() {
    let source = "value=left+right value=value//10 output.setNumber(1,value)";
    let (code, origins) = run(source, passes::decode_chains::fuse_quotient_chains);
    assert_eq!(code, "value=(left+right)//10 output.setNumber(1,value)");
    assert_eq!(origins.unknown_bytes(), 0);
    assert_eq!(
        source_text(
            source,
            origin_at(&code, &origins, code.find("10").unwrap()).unwrap()
        ),
        "10"
    );
    let expression = origin_at(&code, &origins, code.find("//").unwrap()).unwrap();
    assert_eq!(source_text(source, expression), "value=left+right");
    assert!(expression
        .related
        .iter()
        .any(|s| &source[s.start..s.end] == "value=value//10"));
    assert_eq!(
        source_text(
            source,
            origin_at(&code, &origins, code.find("left").unwrap()).unwrap()
        ),
        "left"
    );
}

#[test]
fn quotient_chain_records_all_contributing_divisors_over_multiple_rounds() {
    let source = "value=value//2 value=value//3 value=value//5 output.setNumber(1,value)";
    let (code, origins) = run(source, passes::decode_chains::fuse_quotient_chains);
    assert_eq!(code, "value=value//30 output.setNumber(1,value)");
    assert_eq!(origins.unknown_bytes(), 0);
    let product = origin_at(&code, &origins, code.find("30").unwrap()).unwrap();
    assert_eq!(source_text(source, product), "2");
    for factor in ["3", "5"] {
        assert!(product
            .related
            .iter()
            .any(|s| &source[s.start..s.end] == factor));
    }
}

#[test]
fn screen_loop_scan_without_drawing_keeps_all_source_boundaries() {
    let source = "a=input.getNumber b=output.setNumber function onTick()x=a(1)y=a(2)b(1,x+y)end";
    for periodic in [false, true] {
        let (code, origins) = run(source, |a, r| {
            passes::screen_loops::synthesize_screen_loops(a, r, periodic)
        });
        assert_eq!(code, source);
        assert_eq!(origins.unknown_bytes(), 0);
        for m in &origins.mappings {
            let o = &origins.origins[m.origin.unwrap() as usize];
            if o.precision == OriginPrecision::Group {
                assert!(code[m.start..m.end]
                    .chars()
                    .all(|c| c.is_whitespace() || c == ';'));
            }
        }
        assert!(!origins
            .origins
            .iter()
            .any(|o| o.kind == OriginKind::Synthetic));
    }
}

#[test]
fn insufficient_screen_run_retains_each_call_origin() {
    let source = "function onDraw()screen.drawLine(1,2,3,4)screen.drawLine(5,6,7,8)end";
    let (code, origins) = run(source, |a, r| {
        passes::screen_loops::synthesize_screen_loops(a, r, true)
    });
    assert_eq!(code, source);
    assert_eq!(origins.unknown_bytes(), 0);
    let names = origins
        .origins
        .iter()
        .filter(|o| o.name.as_deref() == Some("drawLine"))
        .map(|o| o.primary.unwrap().start)
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        source
            .match_indices("drawLine")
            .map(|(i, _)| i)
            .collect::<Vec<_>>()
    );
}

#[test]
fn preserving_screen_block_does_not_attribute_unannotated_generated_loop_nodes() {
    let source = format!(
        "function onDraw(){}screen.drawText(1,1,'unchanged')end",
        (0..24)
            .map(|i| format!("screen.drawLine({i},0,{i},20) "))
            .collect::<String>()
    );
    let (code, origins) = run(&source, |a, r| {
        passes::screen_loops::synthesize_screen_loops(a, r, true)
    });
    assert!(
        code.contains("for "),
        "fixture must exercise actual synthesis: {code}"
    );
    assert!(
        origins.unknown_bytes() > 0,
        "unannotated synthesized nodes must stay Unknown"
    );
    assert!(origin_at(&code, &origins, code.find("for ").unwrap()).is_none());
    let text = origin_at(&code, &origins, code.find("drawText").unwrap()).unwrap();
    assert_eq!(source_text(&source, text), "drawText");
}

#[test]
fn original_unknown_block_remains_unknown_after_screen_noop() {
    let source = "first=1 second=2";
    let (mut ast, root) = parse_source_with_origins("unknown.lua", source).unwrap();
    let old = ast.nodes[root as usize].clone();
    ast.nodes[root as usize] = old;
    let result = passes::screen_loops::synthesize_screen_loops(&mut ast, root, false);
    let printed = Printer::new(&ast, false).output_with_positions(result.root);
    let origins = GeneratedOrigins::from_print(&ast, &printed).unwrap();
    assert_eq!(origins.unknown_bytes(), 1);
    assert!(origin_at(&printed.code, &origins, printed.code.find(' ').unwrap()).is_none());
    assert!(origin_at(&printed.code, &origins, printed.code.find('=').unwrap()).is_some());
}

#[test]
fn immutable_value_reuse_preserves_each_use_expression_and_its_definition() {
    let source="local angle=math.pi function onTick()output.setNumber(1,math.pi)output.setNumber(2,math.pi*2)end";
    let (code, origins) = run(source, passes::immutable_values::reuse_immutable_values);
    assert_eq!(code.matches("math.pi").count(), 1, "{code}");
    assert_eq!(origins.unknown_bytes(), 0);
    let replaced = origins
        .origins
        .iter()
        .filter(|o| o.transformation.as_deref() == Some("immutable-value-definition"))
        .collect::<Vec<_>>();
    assert_eq!(replaced.len(), 2);
    let definitions = source
        .match_indices("math.pi")
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    for (origin, expected) in replaced.iter().zip(definitions.iter().skip(1)) {
        assert_eq!(origin.primary.unwrap().start, *expected);
        assert_eq!(source_text(source, origin), "math.pi");
        assert_eq!(origin.precision, OriginPrecision::Expression);
        assert!(origin
            .related
            .iter()
            .any(|span| span.start == definitions[0]));
    }
}

#[test]
fn immutable_reuse_does_not_invent_the_location_of_an_unknown_use() {
    let source = "local angle=math.pi function onTick()output.setNumber(1,math.pi)end";
    let (mut ast, root) = parse_source_with_origins("unknown.lua", source).unwrap();
    let id = ast
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| matches!(n, Node::Index(..)).then_some(i))
        .next_back()
        .unwrap();
    let old = ast.nodes[id].clone();
    ast.nodes[id] = old;
    let result = passes::immutable_values::reuse_immutable_values(&mut ast, root);
    let printed = Printer::new(&ast, false).output_with_positions(result.root);
    assert_eq!(printed.code.matches("math.pi").count(), 1);
    let origins = GeneratedOrigins::from_print(&ast, &printed).unwrap();
    let offset = printed.code.rfind("angle").unwrap();
    assert!(origin_at(&printed.code, &origins, offset).is_none());
    assert_eq!(origins.unknown_bytes(), "angle".len());
}

#[test]
fn boolean_coercion_tracks_both_negations_to_the_original_boolean_expression() {
    let source = "result=condition and true or false";
    for exact in [false, true] {
        let (code, origins) = run(source, |ast, root| {
            if exact {
                passes::literal_folding::fold_exact_safe_control_flow(ast, root)
            } else {
                passes::literal_folding::constant_fold(ast, root, true)
            }
        });
        assert_eq!(code, "result=not not condition");
        assert_eq!(origins.unknown_bytes(), 0);
        for (offset, _) in code.match_indices("not") {
            let origin = origin_at(&code, &origins, offset).unwrap();
            assert_eq!(source_text(source, origin), "condition and true or false");
            assert_eq!(origin.precision, OriginPrecision::Expression);
        }
        assert_eq!(
            source_text(
                source,
                origin_at(&code, &origins, code.find("condition").unwrap()).unwrap()
            ),
            "condition"
        );
    }
}

#[test]
fn quotient_product_with_an_unattributed_factor_remains_unknown() {
    let source = "value=value//2 value=value//3";
    let (mut ast, root) = parse_source_with_origins("unknown.lua", source).unwrap();
    let id = ast
        .nodes
        .iter()
        .position(|n| matches!(n,Node::Num(s) if s.as_ref()=="3"))
        .unwrap();
    let old = ast.nodes[id].clone();
    ast.nodes[id] = old;
    let result = passes::decode_chains::fuse_quotient_chains(&mut ast, root);
    let printed = Printer::new(&ast, false).output_with_positions(result.root);
    assert_eq!(printed.code, "value=value//6");
    let origins = GeneratedOrigins::from_print(&ast, &printed).unwrap();
    assert_eq!(origins.unknown_bytes(), 1);
    assert!(origin_at(&printed.code, &origins, printed.code.find('6').unwrap()).is_none());
}

#[test]
fn complete_zero_coverage_survives_real_search_and_target_fallback() {
    use crate::{compile_code, CompileMode, CompileOptions, NumericMode, SearchMode};
    for source in [
        "a=input.getNumber b=output.setNumber function onTick()x=a(1)y=a(2)b(1,x+y)end",
        "function onTick()local value=input.getNumber(1)if value>0 then output.setNumber(1,1)else output.setNumber(1,2)end end",
        "function onTick()local value=input.getNumber(1)local flag=value>0 and true or false output.setBool(1,flag)end",
    ] {
        for mode in [CompileMode::Safe, CompileMode::Smallest] {
            for numeric_mode in [NumericMode::Exact, NumericMode::Tolerant] {
                for zero_cost_newlines in [false,true] {
                    let options=CompileOptions {mode,numeric_mode,zero_cost_newlines,..Default::default()};
                    let plain=compile_code(source,&options).unwrap();
                    let tracked=CompileOptions{origin_source:Some("complete.lua".into()),..options};
                    let result=compile_code(source,&tracked).unwrap();
                    assert_eq!(result.code,plain.code);
                    assert_eq!(result.stats.candidate_sizes,plain.stats.candidate_sizes);
                    let origins=result.origins.as_ref().unwrap();
                    origins.validate_for_code(&result.code).unwrap();
                    assert_eq!(origins.unknown_bytes(),0,"{source}: {}",result.code);
                    let fallback=compile_code(source,&CompileOptions{target_size:Some(0),search_mode:SearchMode::Fast,search_beam_width:1,..tracked}).unwrap();
                    assert_eq!(fallback.code,result.code);
                    assert_eq!(fallback.origins,result.origins);
                }
            }
        }
    }
}

#[test]
fn split_sign_traversal_preserves_noop_and_deleted_sequence_boundaries() {
    for source in [
        "a=input.getNumber b=output.setNumber function onTick()x=a(1)y=a(2)b(1,x+y)end",
        "function onTick()local x=input.getNumber(1)local p=math.max(x,0)x=math.min(x,0)x=p+x output.setNumber(1,x)end",
    ] {
        let (code,origins)=run(source,|a,r|passes::split_sign::eliminate_split_sign_recomposition(a,r,true));
        assert_eq!(origins.unknown_bytes(),0);
        assert!(!code.contains("math.max"));
        assert!(origins.origins.iter().all(|o|o.kind!=OriginKind::Synthetic));
    }
}

#[test]
fn generated_split_sign_square_expression_does_not_inherit_known_block_origin() {
    let source = "x=input.getNumber(1)y=math.max(x,0)^2*3-math.min(x,0)^2*3";
    let (code, origins) = run(source, |a, r| {
        passes::split_sign::eliminate_split_sign_recomposition(a, r, true)
    });
    assert_eq!(code, "x=input.getNumber(1)y=x*math.abs(x)*3");
    assert!(origins.unknown_bytes() > 0);
    assert!(origin_at(&code, &origins, code.find("abs").unwrap()).is_none());
    assert_eq!(
        source_text(
            source,
            origin_at(&code, &origins, code.find("input").unwrap()).unwrap()
        ),
        "input"
    );
}
