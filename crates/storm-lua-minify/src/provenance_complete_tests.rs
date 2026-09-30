//! End-to-end and negative coverage tests for the final origin-completion paths.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use crate::{pass::PassResult, passes};
use storm_lua_syntax::provenance::{GeneratedOrigins, Origin, OriginKind};
use storm_lua_syntax::{parse_source, parse_source_with_origins, Ast, Node, NodeId, Printer};

fn print(ast: &Ast, root: NodeId) -> (String, GeneratedOrigins) {
    let printed = Printer::new(ast, false).output_with_positions(root);
    let origins = GeneratedOrigins::from_print(ast, &printed).unwrap();
    origins.validate_for_code(&printed.code).unwrap();
    (printed.code, origins)
}
fn run(source: &str, pass: impl Fn(&mut Ast, NodeId) -> PassResult) -> (String, GeneratedOrigins) {
    let (mut ast, root) = parse_source_with_origins("original.lua", source).unwrap();
    let (mut plain, plain_root) = parse_source(source).unwrap();
    let result = pass(&mut ast, root);
    let plain_result = pass(&mut plain, plain_root);
    let (code, origins) = print(&ast, result.root);
    assert_eq!(code, Printer::new(&plain, false).output(plain_result.root));
    (code, origins)
}
fn at(origins: &GeneratedOrigins, pos: usize) -> Option<&Origin> {
    let mapping = origins
        .mappings
        .iter()
        .find(|m| m.start <= pos && pos < m.end)
        .unwrap();
    mapping.origin.map(|i| &origins.origins[i as usize])
}
fn original<'a>(source: &'a str, origin: &Origin) -> &'a str {
    let span = origin.primary.unwrap();
    &source[span.start..span.end]
}

#[test]
fn reciprocal_representation_retains_the_multiplier_literal() {
    let source = "return value*0.5";
    let (code, origins) = run(source, |a, r| {
        passes::literal_folding::constant_fold(a, r, true)
    });
    assert!(code.contains("value/2"), "{code}");
    assert_eq!(origins.unknown_bytes(), 0);
    assert_eq!(
        original(source, at(&origins, code.rfind('2').unwrap()).unwrap()),
        "0.5"
    );
}

#[test]
fn reciprocal_does_not_recover_a_missing_multiplier_origin() {
    let source = "return value*0.5";
    let (mut ast, root) = parse_source_with_origins("missing.lua", source).unwrap();
    let id = ast
        .nodes
        .iter()
        .position(|n| matches!(n,Node::Num(s) if s.as_ref()=="0.5"))
        .unwrap();
    let syntax = ast.nodes[id].clone();
    ast.nodes[id] = syntax;
    let r = passes::literal_folding::constant_fold(&mut ast, root, true);
    let (code, origins) = print(&ast, r.root);
    assert!(code.contains("value/2"));
    assert!(at(&origins, code.rfind('2').unwrap()).is_none());
}

#[test]
fn positive_term_operators_and_constants_have_explicit_input_ranges() {
    let source = "return value+(-0.14*other)";
    let (code, origins) = run(source, |a, r| {
        passes::literal_folding::constant_fold(a, r, false)
    });
    assert!(code.contains(".14"));
    assert_eq!(origins.unknown_bytes(), 0);
    let number = at(&origins, code.find(".14").unwrap()).unwrap();
    assert!(original(source, number).contains("0.14"));
}

#[test]
fn reduced_fraction_keeps_both_operand_literals() {
    let source = "return (16/12)*value";
    let (code, origins) = run(source, |a, r| {
        passes::literal_folding::constant_fold(a, r, true)
    });
    assert_eq!(origins.unknown_bytes(), 0, "{code}");
    assert!(code.len() < source.len());
    assert!(origins.origins.iter().any(|o| o
        .primary
        .is_some_and(|s| source[s.start..s.end].contains("16/12"))));
}

#[test]
fn bit_test_modulus_and_boundary_keep_mask_and_comparison_sources() {
    let source = "return (value&8)~=0";
    let (code, origins) = run(source, |a, r| {
        passes::literal_folding::constant_fold(a, r, true)
    });
    assert_eq!(code, "return value%16>7");
    assert_eq!(origins.unknown_bytes(), 0);
    assert_eq!(
        original(source, at(&origins, code.find("16").unwrap()).unwrap()),
        "8"
    );
    let boundary = at(&origins, code.rfind('7').unwrap()).unwrap();
    assert_eq!(original(source, boundary), "8");
    assert!(boundary
        .related
        .iter()
        .any(|s| &source[s.start..s.end] == "0"));
}

#[test]
fn rejected_literal_call_candidate_keeps_original_parentheses() {
    let source = "local function f(x)return x+external end result=f(0)";
    let (code, origins) = run(source, |a, r| {
        passes::inline_functions::fold_literal_function_calls(a, r, false)
    });
    assert!(code.contains("f(0)"), "{code}");
    assert_eq!(origins.unknown_bytes(), 0);
    assert_eq!(
        original(source, at(&origins, code.rfind("(0)").unwrap()).unwrap()),
        "f(0)"
    );
}

#[test]
fn generated_root_function_name_stays_synthetic_after_globalization() {
    let (mut ast, root) = parse_source_with_origins("empty.lua", "").unwrap();
    let literal = ast.num("7".into());
    let ret = ast.push(Node::Return(vec![literal]));
    let body = ast.block(vec![ret]);
    let f = ast.function(Vec::<String>::new(), false, body);
    let decl = ast.localfunc("generated", f);
    for node in [literal, ret, body, f, decl] {
        ast.nodes.mark_synthetic(node, "test-generated-helper");
    }
    ast.nodes
        .rewrite(root, Node::Block(vec![decl]), "helper-insertion");
    let r = passes::root_globals::globalize_root_locals(&mut ast, root);
    let (code, origins) = print(&ast, r.root);
    assert!(code.starts_with("function generated"));
    assert_eq!(origins.unknown_bytes(), 0);
    assert_eq!(
        at(&origins, code.find("generated").unwrap()).unwrap().kind,
        OriginKind::Synthetic
    );
}

#[test]
fn root_globalization_does_not_invent_a_missing_original_binding() {
    let source = "local function original()return 7 end";
    let (mut ast, root) = parse_source_with_origins("missing.lua", source).unwrap();
    let id = ast
        .nodes
        .iter()
        .position(|n| matches!(n, Node::Localfunc(..)))
        .unwrap();
    let n = ast.nodes[id].clone();
    ast.nodes[id] = n;
    let r = passes::root_globals::globalize_root_locals(&mut ast, root);
    let (code, origins) = print(&ast, r.root);
    assert!(at(&origins, code.find("original").unwrap()).is_none());
}

#[test]
fn available_expression_reference_keeps_use_and_reaching_definition() {
    let source = "function onTick()local value=left+right output.setNumber(1,left+right)end";
    let (code, origins) = run(
        source,
        passes::available_expressions::reuse_available_expressions,
    );
    assert!(code.contains("output.setNumber(1,value)"), "{code}");
    assert_eq!(origins.unknown_bytes(), 0);
    let reference = at(&origins, code.rfind("value").unwrap()).unwrap();
    assert_eq!(
        reference.primary.unwrap().start,
        source.rfind("left+right").unwrap()
    );
    assert!(reference
        .related
        .iter()
        .any(|s| s.start == source.find("left+right").unwrap()));
}

#[test]
fn screen_affine_columns_are_derived_but_loop_control_is_generated() {
    let source = format!(
        "function onDraw(){}end",
        (0..24)
            .map(|i| format!("screen.drawRectF({},3,2,2) ", i * 2 + 5))
            .collect::<String>()
    );
    let (code, origins) = run(&source, |a, r| {
        passes::screen_loops::synthesize_screen_loops(a, r, true)
    });
    assert!(code.contains("for "));
    assert_eq!(origins.unknown_bytes(), 0);
    assert_eq!(
        at(&origins, code.find("for ").unwrap()).unwrap().kind,
        OriginKind::Synthetic
    );
    assert!(origins.origins.iter().any(|o| o.transformation.as_deref()
        == Some("screen-affine-arguments")
        && o.related.len() > 10));
}

#[test]
fn missing_unit_progression_input_cannot_be_hidden_by_synthetic_loop_index() {
    let source = format!(
        "function onDraw(){}end",
        (0..24)
            .map(|i| format!("screen.drawRectF({i},3,2,2) "))
            .collect::<String>()
    );
    let (mut ast, root) = parse_source_with_origins("missing.lua", &source).unwrap();
    let id = ast
        .nodes
        .iter()
        .position(|n| matches!(n,Node::Num(s) if s.as_ref()=="13"))
        .unwrap();
    let n = ast.nodes[id].clone();
    ast.nodes[id] = n;
    let r = passes::screen_loops::synthesize_screen_loops(&mut ast, root, true);
    let (code, origins) = print(&ast, r.root);
    assert!(code.contains("for "));
    assert!(origins.unknown_bytes() > 0);
    assert_eq!(
        at(&origins, code.find("for ").unwrap()).unwrap().kind,
        OriginKind::Synthetic
    );
}

#[test]
fn screen_value_table_keeps_each_original_expression() {
    let source = format!(
        "function onDraw(){}end",
        [5, 9, 6, 13, 8, 16, 11, 18, 12, 21, 15, 25]
            .iter()
            .map(|i| format!("screen.drawLine(value+{i},0,30,20) "))
            .collect::<String>()
    );
    let (code, origins) = run(&source, |a, r| {
        passes::screen_loops::synthesize_screen_loops(a, r, false)
    });
    assert!(code.contains("for "), "{code}");
    assert_eq!(origins.unknown_bytes(), 0);
    assert!(origins
        .origins
        .iter()
        .any(|o| o.transformation.as_deref() == Some("screen-value-table-lookup")));
}

#[test]
fn approximation_replacement_keeps_its_original_literal() {
    let source = "return 3.141592653589793,2.718281828459045";
    let (code, origins) = run(source, passes::numeric_literals::shorten_numeric_literals);
    assert_eq!(origins.unknown_bytes(), 0);
    assert!(code.len() < source.len(), "{code}");
    for o in origins.origins.iter().filter(|o| {
        o.transformation.as_deref() == Some("numeric-literal-approximation")
            && o.precision == storm_lua_syntax::provenance::OriginPrecision::Expression
    }) {
        assert!(matches!(
            original(source, o),
            "3.141592653589793" | "2.718281828459045"
        ));
    }
}

#[test]
fn pooled_float_value_relates_distinct_original_occurrences() {
    let source = format!(
        "function onTick(){}end",
        (1..25)
            .map(|i| format!("output.setNumber({i},12345.125) "))
            .collect::<String>()
    );
    let (code, origins) = run(&source, |a, r| {
        passes::literal_pool::pool_numeric_literals(a, r, true)
    });
    assert!(
        code.starts_with("local ") && code.len() < source.len(),
        "{code}"
    );
    assert_eq!(origins.unknown_bytes(), 0);
    let initializer = origins
        .origins
        .iter()
        .find(|o| {
            o.primary
                .is_some_and(|span| &source[span.start..span.end] == "12345.125")
                && o.related.len() == 23
        })
        .unwrap();
    assert_eq!(initializer.related.len(), 23);
    assert_eq!(original(&source, initializer), "12345.125");
    assert!(initializer
        .related
        .iter()
        .all(|s| &source[s.start..s.end] == "12345.125"));
}

#[test]
fn missing_pooled_literal_contributor_stays_unknown() {
    let source = format!(
        "function onTick(){}end",
        (1..25)
            .map(|i| format!("output.setNumber({i},12345.125) "))
            .collect::<String>()
    );
    let (mut ast, root) = parse_source_with_origins("missing.lua", &source).unwrap();
    let id = ast
        .nodes
        .iter()
        .position(|n| matches!(n,Node::Num(s) if s.as_ref()=="12345.125"))
        .unwrap();
    let syntax = ast.nodes[id].clone();
    ast.nodes[id] = syntax;
    let r = passes::literal_pool::pool_numeric_literals(&mut ast, root, true);
    let (code, origins) = print(&ast, r.root);
    assert!(code.starts_with("local "));
    assert!(origins.unknown_bytes() > 0);
    assert!(at(&origins, code.find('=').unwrap() + 1).is_none());
}

#[test]
fn exact_fraction_encoding_has_complete_origins_without_value_change() {
    let source = "return 0.0009765625,0.000244140625,0.001953125";
    let (code, origins) = run(source, |a, r| {
        passes::literal_pool::pool_numeric_literals(a, r, true)
    });
    assert!(code.len() < source.len(), "{code}");
    assert_eq!(origins.unknown_bytes(), 0);
    let lua = mlua::Lua::new();
    let a: mlua::MultiValue = lua.load(source).eval().unwrap();
    let b: mlua::MultiValue = lua.load(&code).eval().unwrap();
    assert_eq!(format!("{a:?}"), format!("{b:?}"));
}

#[test]
fn rescaled_initializer_and_compensated_reads_keep_separate_origins() {
    let source="size=192 function onTick()x=input.getNumber(1)output.setNumber(1,(size/2+x*size/2)//1)output.setNumber(2,size)end";
    let (code, origins) = run(source, passes::binding_rescaling::rescale_bindings);
    assert!(code.contains("size=96"));
    assert!(code.contains("size*2"));
    assert_eq!(origins.unknown_bytes(), 0);
    let initializer = at(&origins, code.find("96").unwrap()).unwrap();
    assert_eq!(original(source, initializer), "192");
    assert!(initializer
        .related
        .iter()
        .any(|s| &source[s.start..s.end] == "2"));
}

#[test]
fn callback_slot_shared_name_keeps_both_original_function_identities() {
    let globals = (0..55).map(|i| format!("s{i}=0 ")).collect::<String>();
    let source=format!("{globals}function tickHelper(x)return x+1 end function drawHelper(x)return x+2 end function onTick()output.setNumber(1,tickHelper(input.getNumber(1)))end function onDraw()screen.drawText(0,0,drawHelper(input.getNumber(2)))end");
    let (code, origins) = run(
        &source,
        passes::callback_slots::slot_callback_exclusive_functions,
    );
    assert!(code.contains("__stormmin_phase_function_0"));
    assert_eq!(origins.unknown_bytes(), 0);
    for name in ["tickHelper", "drawHelper"] {
        let mut starts = origins
            .origins
            .iter()
            .filter(|o| o.name.as_deref() == Some(name))
            .map(|o| o.primary.unwrap().start)
            .collect::<Vec<_>>();
        starts.sort_unstable();
        starts.dedup();
        assert_eq!(
            starts,
            source
                .match_indices(name)
                .map(|(i, _)| i)
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn moved_else_default_is_not_attributed_to_the_true_branch() {
    let source="x=0 y=0 function onTick()if input.getBool(1)then x=input.getNumber(1)y=input.getNumber(2)else x,y=0,0 end output.setNumber(1,x)output.setNumber(2,y)end";
    let (code, origins) = run(source, passes::default_hoisting::hoist_else_defaults);
    assert!(code.contains("x,y=0,0 if "));
    assert_eq!(origins.unknown_bytes(), 0);
    let moved = at(&origins, code.find("x,y=0,0").unwrap() + 3).unwrap();
    assert_eq!(original(source, moved), "x,y=0,0");
}

#[test]
fn translated_wrapper_calls_retain_their_actual_original_constants() {
    let source="function dark()screen.setColor(45,45,55)end function bg()screen.setColor(60,60,70)end function onDraw()dark()bg()dark()bg()dark()bg()end";
    let (code, origins) = run(
        source,
        passes::wrapper_functions::merge_translated_constant_wrappers,
    );
    assert!(!code.contains("function bg()"));
    assert_eq!(origins.unknown_bytes(), 0);
    let actuals = origins
        .mappings
        .iter()
        .filter_map(|m| {
            let value = &code[m.start..m.end];
            if !matches!(value, "45" | "55" | "60" | "70") {
                return None;
            }
            let origin = m.origin.map(|i| &origins.origins[i as usize])?;
            assert_eq!(original(source, origin), value);
            Some(value)
        })
        .collect::<Vec<_>>();
    assert!(actuals.iter().any(|s| matches!(*s, "45" | "55")));
    assert!(actuals.iter().any(|s| matches!(*s, "60" | "70")));
}

#[test]
fn affine_wrapper_coefficients_retain_both_original_values() {
    let source="function warm()screen.setColor(180,180,200)end function dark()screen.setColor(35,30,30)end function onDraw()warm()dark()warm()dark()warm()dark()end";
    let (code, origins) = run(
        source,
        passes::wrapper_functions::merge_affine_constant_wrappers,
    );
    assert_eq!(origins.unknown_bytes(), 0);
    assert_ne!(code, source);
    assert!(origins.origins.iter().any(|o| o.transformation.as_deref()
        == Some("affine-wrapper-argument")
        && !o.related.is_empty()));
    assert!(origins.origins.iter().any(|o| o.transformation.as_deref()
        == Some("affine-wrapper-selector")
        && o.kind == OriginKind::Synthetic));
}

#[test]
fn full_search_stays_complete_for_rejected_literal_call_and_hoisted_defaults() {
    let source="local function f(x)return x+external end x=0 y=0 function onTick()if input.getBool(1)then x=input.getNumber(1)y=input.getNumber(2)else x,y=0,0 end output.setNumber(1,x)output.setNumber(2,y)output.setNumber(3,f(0))end";
    for mode in [crate::CompileMode::Safe, crate::CompileMode::Smallest] {
        let options = crate::CompileOptions {
            mode,
            origin_source: Some("source.lua".into()),
            ..Default::default()
        };
        let full = crate::compile_code(source, &options).unwrap();
        full.origins
            .as_ref()
            .unwrap()
            .validate_for_code(&full.code)
            .unwrap();
        assert_eq!(full.origins.as_ref().unwrap().unknown_bytes(), 0);
        let missed = crate::compile_code(
            source,
            &crate::CompileOptions {
                target_size: Some(0),
                search_mode: crate::SearchMode::Fast,
                search_beam_width: 1,
                ..options
            },
        )
        .unwrap();
        assert_eq!(full.code, missed.code);
        assert_eq!(full.origins, missed.origins);
    }
}
