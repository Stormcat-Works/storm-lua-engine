//! Source attribution regressions for tables, field names and one-use function bodies.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use crate::{pass::PassResult, passes};
use storm_lua_syntax::provenance::{GeneratedOrigins, Origin, OriginKind, OriginPrecision};
use storm_lua_syntax::{
    parse_source, parse_source_with_origins, Ast, NameSite, Node, NodeId, Printer,
};

fn output(ast: &Ast, root: NodeId) -> (String, GeneratedOrigins) {
    let printed = Printer::new(ast, false).output_with_positions(root);
    let origins = GeneratedOrigins::from_print(ast, &printed).unwrap();
    origins.validate_for_code(&printed.code).unwrap();
    (printed.code, origins)
}
fn run(source: &str, pass: impl Fn(&mut Ast, NodeId) -> PassResult) -> (String, GeneratedOrigins) {
    let (mut ast, root) = parse_source_with_origins("fixture.lua", source).unwrap();
    let (mut plain, plain_root) = parse_source(source).unwrap();
    let r = pass(&mut ast, root);
    let expected = pass(&mut plain, plain_root);
    let (code, origins) = output(&ast, r.root);
    assert_eq!(code, Printer::new(&plain, false).output(expected.root));
    (code, origins)
}
fn at(origins: &GeneratedOrigins, offset: usize) -> Option<&Origin> {
    origins
        .mappings
        .iter()
        .find(|m| m.start <= offset && offset < m.end)
        .and_then(|m| m.origin)
        .map(|i| &origins.origins[i as usize])
}
fn text<'a>(source: &'a str, origin: &Origin) -> &'a str {
    let s = origin.primary.unwrap();
    &source[s.start..s.end]
}
fn node(ast: &Ast, pred: impl Fn(&Node) -> bool) -> NodeId {
    ast.nodes.iter().position(pred).unwrap() as NodeId
}
fn flatten(ast: &mut Ast, root: NodeId) -> PassResult {
    passes::tables::flatten_immutable_tables(ast, root, 10)
}
fn namespace(ast: &mut Ast, root: NodeId) -> PassResult {
    passes::tables::scalarize_closed_namespaces(ast, root, 10)
}
fn inline(ast: &mut Ast, root: NodeId) -> PassResult {
    passes::inline_functions::inline_one_use_statement_and_tail_functions(ast, root, 10)
}
fn signed(ast: &mut Ast, root: NodeId) -> PassResult {
    passes::signed_factoring::factor_signed_expressions(ast, root, true, 10)
}

#[test]
fn immutable_nested_leaf_uses_definition_and_separate_access_positions() {
    let source="local config={nested={value=12}} output.setNumber(1,config.nested.value)output.setNumber(2,config.nested.value)";
    let (code, o) = run(source, flatten);
    assert_eq!(o.unknown_bytes(), 0);
    assert_eq!(code, "output.setNumber(1,12)output.setNumber(2,12)");
    let uses = source
        .match_indices("config.nested.value")
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    let literals = code.match_indices("12").collect::<Vec<_>>();
    for ((offset, _), expected_use) in literals.into_iter().zip(uses) {
        let origin = at(&o, offset).unwrap();
        assert_eq!(text(source, origin), "12");
        assert!(origin.related.iter().any(|s| s.start == expected_use));
    }
}
#[test]
fn immutable_missing_key_nil_belongs_to_lookup_not_an_unrelated_literal() {
    let source = "local config={first=1}output.setNumber(1,config.missing)";
    let (code, o) = run(source, flatten);
    assert_eq!(o.unknown_bytes(), 0);
    let origin = at(&o, code.find("nil").unwrap()).unwrap();
    assert_eq!(text(source, origin), "config.missing");
    assert_eq!(origin.kind, OriginKind::Derived);
}
#[test]
fn immutable_leaf_with_lost_origin_stays_unknown_after_lookup_folding() {
    let source = "local config={value=12}output.setNumber(1,config.value)";
    let (mut ast, root) = parse_source_with_origins("lost.lua", source).unwrap();
    let literal = node(&ast, |n| matches!(n,Node::Num(v) if v.as_ref()=="12"));
    let value = ast.node(literal).clone();
    ast.nodes[literal as usize] = value;
    let r = flatten(&mut ast, root);
    let (code, o) = output(&ast, r.root);
    assert!(at(&o, code.find("12").unwrap()).is_none());
    assert_eq!(o.unknown_bytes(), 2);
}
#[test]
fn immutable_table_removal_shifts_surviving_local_names_to_the_correct_slot() {
    let source="local config,retained={value=12},input.getNumber(1)output.setNumber(1,config.value+retained)";
    let (code, o) = run(source, flatten);
    assert!(code.starts_with("local retained="));
    assert_eq!(o.unknown_bytes(), 0);
    let origin = at(&o, code.find("retained").unwrap()).unwrap();
    assert_eq!(origin.precision, OriginPrecision::Name);
    assert_eq!(
        origin.primary.unwrap().start,
        source.find("retained").unwrap()
    );
}
#[test]
fn immutable_fresh_reference_rejection_keeps_the_unchanged_table_origins() {
    let source = "local config={nested={}}output.setBool(1,config.nested==config.nested)";
    let (code, o) = run(source, flatten);
    assert!(code.contains("config.nested==config.nested"));
    assert_eq!(o.unknown_bytes(), 0);
}
#[test]
fn namespace_scalar_targets_belong_to_each_original_field_not_equal_values() {
    let source="local state={first_value=7,second_value=7}function onTick()state.first_value=state.first_value+1 output.setNumber(1,state.first_value+state.second_value)end";
    let (code, o) = run(source, namespace);
    assert!(!code.contains("state."));
    assert_eq!(o.unknown_bytes(), 0);
    for name in ["first_value", "second_value"] {
        let original = source.find(name).unwrap();
        assert!(o
            .origins
            .iter()
            .any(|x| x.name.as_deref() == Some(name) && x.primary.unwrap().start == original));
    }
    assert!(o.origins.iter().all(|x| x.kind != OriginKind::Synthetic));
}
#[test]
fn namespace_quoted_field_binding_keeps_literal_source_without_fake_identifier() {
    let source="local state={['long-field']=7}function onTick()state['long-field']=state['long-field']+1 output.setNumber(1,state['long-field'])end";
    let (code, o) = run(source, namespace);
    assert!(code.starts_with("__sf"));
    assert_eq!(o.unknown_bytes(), 0);
    let origin = at(&o, 0).unwrap();
    assert_eq!(text(source, origin), "'long-field'");
    assert!(origin.name.is_none());
}
#[test]
fn namespace_removal_preserves_other_local_declaration_names() {
    let source="local state,retained={value=7},2 function onTick()state.value=state.value+1 output.setNumber(1,state.value+retained)end";
    let (code, o) = run(source, namespace);
    assert!(code.starts_with("local retained=2"));
    assert_eq!(o.unknown_bytes(), 0);
    assert_eq!(
        at(&o, code.find("retained").unwrap())
            .unwrap()
            .primary
            .unwrap()
            .start,
        source.find("retained").unwrap()
    );
}
#[test]
fn one_use_tail_local_preserves_parameters_body_callsite_and_caller_binding() {
    let source="local function twice(value)local doubled=value*2 return doubled end function onTick()local result=twice(input.getNumber(1))output.setNumber(1,result)end";
    let (code, o) = run(source, inline);
    assert!(!code.contains("function twice"));
    assert_eq!(o.unknown_bytes(), 0);
    let call_start = source.find("twice(input").unwrap();
    let body_use = source.find("value*2").unwrap();
    assert!(o
        .origins
        .iter()
        .any(|x| x.name.as_deref() == Some("value") && x.primary.unwrap().start == body_use));
    assert!(o
        .origins
        .iter()
        .any(|x| x.related.iter().any(|r| r.start == call_start)));
    let binding = at(&o, code.find("local result").unwrap() + 6).unwrap();
    assert_eq!(
        binding.primary.unwrap().start,
        source.find("local result").unwrap() + 6
    );
}
#[test]
fn one_use_statement_call_preserves_actual_argument_and_callee_body_ranges() {
    let source="local function emit(value)output.setNumber(1,value)end function onTick()emit(input.getNumber(2))end";
    let (code, o) = run(source, inline);
    assert!(!code.contains("function emit"));
    assert_eq!(o.unknown_bytes(), 0);
    assert!(o.origins.iter().any(|x| x
        .primary
        .is_some_and(|s| &source[s.start..s.end] == "input.getNumber(2)")));
    assert!(o.origins.iter().any(|x| x.name.as_deref() == Some("value")
        && x.primary.unwrap().start == source.find("emit(value)").unwrap() + 5));
}
#[test]
fn one_use_tail_assignment_keeps_original_destination_and_return_contributor() {
    let source="local function twice(value)return value*2 end function onTick()answer=twice(input.getNumber(1))output.setNumber(1,answer)end";
    let (code, o) = run(source, inline);
    assert!(!code.contains("function twice"));
    assert_eq!(o.unknown_bytes(), 0);
    let origin = at(&o, code.find("answer=").unwrap() + 6).unwrap();
    assert!(text(source, origin).starts_with("answer=twice("));
    assert!(origin
        .related
        .iter()
        .any(|s| source[s.start..s.end].starts_with("return")));
}
#[test]
fn one_use_clone_cannot_recover_lost_callee_expression_from_the_call_site() {
    let source="local function twice(value)return value*2 end function onTick()answer=twice(input.getNumber(1))end";
    let (mut ast, root) = parse_source_with_origins("lost.lua", source).unwrap();
    let literal = node(&ast, |n| matches!(n,Node::Num(v) if v.as_ref()=="2"));
    let value = ast.node(literal).clone();
    ast.nodes[literal as usize] = value;
    let r = inline(&mut ast, root);
    let (code, o) = output(&ast, r.root);
    assert!(at(&o, code.find("*2").unwrap() + 1).is_none());
    assert_eq!(o.unknown_bytes(), 1);
}
#[test]
fn one_use_capture_safety_rejection_retains_original_source_positions() {
    let source="local outside=1 local function get()return outside end function onTick()local outside=2 answer=get()output.setNumber(1,outside)end";
    let (code, o) = run(source, inline);
    assert_eq!(o.unknown_bytes(), 0);
    // Whether relocation is legal is still decided by the existing resolver.
    assert!(code.contains("function get"));
}
#[test]
fn omitted_trailing_arguments_keep_call_parentheses_but_not_removed_values() {
    let source = "local function emit(a,b)output.setNumber(a,b)end function onTick()emit(1,nil)end";
    let (code, o) = run(
        source,
        passes::omit_arguments::omit_equivalent_trailing_arguments,
    );
    assert!(code.contains("emit(1)"));
    assert_eq!(o.unknown_bytes(), 0);
    let origin = at(&o, code.find("emit(1)").unwrap() + 4).unwrap();
    assert_eq!(text(source, origin), "emit(1,nil)");
    assert!(!o
        .origins
        .iter()
        .any(|x| x.primary.is_some_and(|r| &source[r.start..r.end] == "nil")));
}
#[test]
fn closed_field_names_preserve_definition_and_each_access_after_renaming() {
    let source="local t={long_field_name=1}t.long_field_name=t.long_field_name+1 output.setNumber(1,t.long_field_name)";
    let (code, o) = run(source, passes::closed_fields::rename_closed_fields);
    assert!(!code.contains("long_field_name"));
    assert_eq!(o.unknown_bytes(), 0);
    let mut offsets = o
        .origins
        .iter()
        .filter(|x| x.name.as_deref() == Some("long_field_name"))
        .map(|x| x.primary.unwrap().start)
        .collect::<Vec<_>>();
    offsets.sort_unstable();
    offsets.dedup();
    assert_eq!(
        offsets,
        source
            .match_indices("long_field_name")
            .map(|(i, _)| i)
            .collect::<Vec<_>>()
    );
}
#[test]
fn closed_quoted_field_becoming_identifier_preserves_quoted_source_range() {
    let source="local t={['long_quoted_name']=1}t['long_quoted_name']=t['long_quoted_name']+1 output.setNumber(1,t['long_quoted_name'])";
    let (code, o) = run(source, passes::closed_fields::rename_closed_fields);
    assert!(!code.contains("long_quoted_name"));
    assert_eq!(o.unknown_bytes(), 0);
    let origin = at(&o, code.find('{').unwrap() + 1).unwrap();
    assert_eq!(text(source, origin), "'long_quoted_name'");
    assert!(origin.name.is_none());
    assert_eq!(origin.precision, OriginPrecision::Expression);
}
#[test]
fn missing_quoted_key_is_not_filled_from_the_known_table_container() {
    let source="local t={['long_quoted_name']=1}t['long_quoted_name']=t['long_quoted_name']+1 output.setNumber(1,t['long_quoted_name'])";
    let (mut ast, root) = parse_source_with_origins("lost.lua", source).unwrap();
    let key = node(
        &ast,
        |n| matches!(n,Node::Str(v) if v.as_ref()=="'long_quoted_name'"),
    );
    let value = ast.node(key).clone();
    ast.nodes[key as usize] = value;
    let r = passes::closed_fields::rename_closed_fields(&mut ast, root);
    let (code, o) = output(&ast, r.root);
    assert!(!code.contains("long_quoted_name"));
    assert!(at(&o, code.find('{').unwrap() + 1).is_none());
    assert!(o.unknown_bytes() > 0);
    assert!(o
        .origins
        .iter()
        .any(|x| x.primary.is_some_and(|s| &source[s.start..s.end] == "1")));
}
#[test]
fn closed_method_definition_and_calls_retain_different_name_occurrences() {
    let source="local object={}function object:long_method_name(value)return value end output.setNumber(1,object:long_method_name(2))output.setNumber(2,object:long_method_name(3))";
    let (code, o) = run(source, passes::closed_fields::rename_closed_fields);
    assert!(!code.contains("long_method_name"));
    assert_eq!(o.unknown_bytes(), 0);
    let mut names = o
        .origins
        .iter()
        .filter(|x| x.name.as_deref() == Some("long_method_name"))
        .map(|x| x.primary.unwrap().start)
        .collect::<Vec<_>>();
    names.sort_unstable();
    names.dedup();
    assert_eq!(
        names,
        source
            .match_indices("long_method_name")
            .map(|(i, _)| i)
            .collect::<Vec<_>>()
    );
}
#[test]
fn field_rename_unknown_key_site_invalidates_only_the_container_fallback() {
    let source = "return {['missing-key']=1,known_field=2}";
    let (mut source_ast, _) = parse_source_with_origins("mixed.lua", source).unwrap();
    let table = node(&source_ast, |n| matches!(n, Node::Table(_)));
    let key = node(&source_ast, |n| matches!(n, Node::Str(_)));
    let value = source_ast.node(key).clone();
    source_ast.nodes[key as usize] = value;
    let mut target = source_ast.clone();
    target
        .nodes
        .copy_expression_to_name_from(table, NameSite::Field(0), &source_ast.nodes, key);
    assert!(target.nodes.origin(table).is_none());
    assert!(target
        .nodes
        .name_origin(table, NameSite::Field(0))
        .is_none());
    assert_eq!(
        target
            .nodes
            .name_origin(table, NameSite::Field(1))
            .unwrap()
            .name
            .as_deref(),
        Some("known_field")
    );
}
#[test]
fn signed_carrier_reads_keep_individual_original_expressions_and_base_definition() {
    let source="function onTick()local a=input.getNumber(1)local b=input.getNumber(2)output.setNumber(1,a*b*123)output.setNumber(2,-(a*b*123))output.setNumber(3,-(a*b*123))end";
    let (code, o) = run(source, signed);
    assert_eq!(o.unknown_bytes(), 0);
    assert!(code.contains("a=a*b*123"));
    let original = source
        .match_indices("a*b*123")
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    for (index, start) in original.iter().enumerate().skip(1) {
        let needle = format!("setNumber({},-(a))", index + 1);
        let offset = code.find(&needle).unwrap() + needle.len() - 3;
        let origin = at(&o, offset).unwrap();
        assert_eq!(origin.primary.unwrap().start, *start);
        assert!(origin.related.iter().any(|s| s.start == original[0]));
    }
    for m in &o.mappings {
        if m.origin
            .is_some_and(|i| o.origins[i as usize].kind == OriginKind::Synthetic)
        {
            assert!(!code[m.start..m.end].contains("123"));
        }
    }
}
#[test]
fn signed_base_normalization_and_fresh_carrier_have_source_backed_operands() {
    let source="a=input.getNumber(1)b=input.getNumber(2)output.setNumber(1,-a*b*123)output.setNumber(2,a*-b*123)output.setNumber(3,-a*b*123)";
    let (code, o) = run(source, signed);
    assert!(code.contains("local __stormmin_signed_"));
    assert_eq!(o.unknown_bytes(), 0);
    assert!(o
        .origins
        .iter()
        .any(|x| x.transformation.as_deref() == Some("signed-base-normalization")));
    assert!(o.origins.iter().any(|x| x.name.as_deref() == Some("a")));
}
#[test]
fn full_pipeline_tracking_keeps_text_and_missed_target_equal_after_table_and_function_passes() {
    let source="local config={gain=2}local function scale(value)local adjusted=value*config.gain return adjusted end function onTick()local result=scale(input.getNumber(1))output.setNumber(1,result)end";
    for mode in [crate::CompileMode::Safe, crate::CompileMode::Smallest] {
        for numeric_mode in [crate::NumericMode::Exact, crate::NumericMode::Tolerant] {
            let options = crate::CompileOptions {
                mode,
                numeric_mode,
                origin_source: Some("fixture.lua".into()),
                ..Default::default()
            };
            let tracked = crate::compile_code(source, &options).unwrap();
            let plain = crate::compile_code(
                source,
                &crate::CompileOptions {
                    origin_source: None,
                    ..options.clone()
                },
            )
            .unwrap();
            assert_eq!(tracked.code, plain.code);
            assert_eq!(tracked.origins.as_ref().unwrap().unknown_bytes(), 0);
            let miss = crate::compile_code(
                source,
                &crate::CompileOptions {
                    target_size: Some(0),
                    search_mode: crate::SearchMode::Fast,
                    search_beam_width: 1,
                    ..options
                },
            )
            .unwrap();
            assert_eq!(tracked.code, miss.code);
            assert_eq!(tracked.origins, miss.origins);
        }
    }
}

#[test]
fn namespace_unknown_named_field_does_not_become_known_from_its_value() {
    let source="local state={very_long_field=7}function onTick()state.very_long_field=state.very_long_field+1 output.setNumber(1,state.very_long_field)end";
    let (mut ast, root) = parse_source_with_origins("lost.lua", source).unwrap();
    let table = node(&ast, |n| matches!(n, Node::Table(_)));
    let value = ast.node(table).clone();
    ast.nodes[table as usize] = value;
    let r = namespace(&mut ast, root);
    let (code, o) = output(&ast, r.root);
    assert!(code.starts_with("__sf"));
    assert!(at(&o, 0).is_none());
    assert_eq!(text(source, at(&o, code.find('7').unwrap()).unwrap()), "7");
}

#[test]
fn signed_unknown_use_is_not_recovered_from_the_known_carrier_definition() {
    let source="function onTick()local a=input.getNumber(1)local b=input.getNumber(2)output.setNumber(1,a*b*123)output.setNumber(2,a*b*123)output.setNumber(3,a*b*123)end";
    let (mut ast, root) = parse_source_with_origins("lost.lua", source).unwrap();
    let expressions = ast
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| match n {
            Node::Bin(op, _, right)
                if op == "*" && matches!(ast.node(*right),Node::Num(v) if v.as_ref()=="123") =>
            {
                Some(i)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(expressions.len(), 3);
    let value = ast.nodes[expressions[1]].clone();
    ast.nodes[expressions[1]] = value;
    let r = signed(&mut ast, root);
    let (code, o) = output(&ast, r.root);
    assert!(code.contains("setNumber(2,a)"));
    assert!(at(&o, code.find("setNumber(2,a)").unwrap() + 12).is_none());
    assert!(at(&o, code.find("setNumber(3,a)").unwrap() + 12).is_some());
}

#[test]
fn quoted_field_origin_survives_json_roundtrip_and_does_not_claim_a_name() {
    let source="local t={['long_quoted_name']=1}t['long_quoted_name']=t['long_quoted_name']+1 output.setNumber(1,t['long_quoted_name'])";
    let (mut ast, root) = parse_source_with_origins("quoted.lua", source).unwrap();
    let result = passes::closed_fields::rename_closed_fields(&mut ast, root);
    let expected = output(&ast, result.root);
    let encoded = serde_json::to_vec(&ast).unwrap();
    let restored: Ast = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(output(&restored, result.root), expected);
    let value = at(&expected.1, expected.0.find('{').unwrap() + 1).unwrap();
    assert!(value.name.is_none());
    assert_eq!(text(source, value), "'long_quoted_name'");
}

#[test]
fn nonidentifier_quoted_field_remains_ineligible_without_an_output_change() {
    let source = "local t={['long-key-name']=1}output.setNumber(1,t['long-key-name'])";
    let (code, o) = run(source, passes::closed_fields::rename_closed_fields);
    assert!(code.contains("long-key-name"));
    assert_eq!(o.unknown_bytes(), 0);
}

#[test]
fn alpha_cloning_maps_shadowed_locals_to_distinct_declarations() {
    let source="local function emit(value)do local value=2 output.setNumber(1,value)end output.setNumber(2,value)end function onTick()emit(input.getNumber(1))end";
    let (code, o) = run(source, inline);
    assert!(!code.contains("function emit"));
    assert_eq!(o.unknown_bytes(), 0);
    let mut sites = o
        .origins
        .iter()
        .filter(|x| x.name.as_deref() == Some("value"))
        .map(|x| x.primary.unwrap().start)
        .collect::<Vec<_>>();
    sites.sort_unstable();
    sites.dedup();
    assert_eq!(
        sites,
        source
            .match_indices("value")
            .map(|(i, _)| i)
            .collect::<Vec<_>>()
    );
}
