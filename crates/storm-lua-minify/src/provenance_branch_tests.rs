//! Branch-specific contracts beyond the one admitted fixture for each registry ID.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use crate::provenance_pass_matrix::apply;
use storm_lua_syntax::provenance::GeneratedOrigins;
use storm_lua_syntax::{parse_source, parse_source_with_origins, Ast, NodeId, Printer};

fn print(a: &Ast, r: NodeId) -> (String, GeneratedOrigins) {
    let p = Printer::new(a, false).output_with_positions(r);
    let o = GeneratedOrigins::from_print(a, &p).unwrap();
    o.validate_for_code(&p.code).unwrap();
    (p.code, o)
}
fn checked(id: &str, source: &str) -> (String, GeneratedOrigins) {
    let (mut a, r) = parse_source_with_origins("branch.lua", source).unwrap();
    let (mut plain, p) = parse_source(source).unwrap();
    let r = apply(id, &mut a, r);
    let p = apply(id, &mut plain, p);
    assert!(a == plain, "{id}: metadata changed syntax");
    let (code, origins) = print(&a, r);
    assert_eq!(code, Printer::new(&plain, false).output(p));
    let gaps = origins
        .mappings
        .iter()
        .filter(|m| m.origin.is_none())
        .map(|m| &code[m.start..m.end])
        .collect::<Vec<_>>();
    assert_eq!(origins.unknown_bytes(), 0, "{id}: {gaps:?} in {code}");
    (code, origins)
}
fn erased_node(a: &mut Ast, source: &str, needle: &str, occurrence: usize) -> NodeId {
    let start = source.match_indices(needle).nth(occurrence).unwrap().0;
    let id = a
        .nodes
        .iter()
        .enumerate()
        .find_map(|(n, _)| {
            let o = a.nodes.origin(n as NodeId)?;
            let s = o.primary?;
            (s.start == start && s.end == start + needle.len()).then_some(n as NodeId)
        })
        .unwrap();
    let value = a.node(id).clone();
    a.nodes[id as usize] = value;
    id
}
fn missing(id: &str, source: &str, needle: &str, occurrence: usize) -> (String, GeneratedOrigins) {
    let (mut a, r) = parse_source_with_origins("missing-operand.lua", source).unwrap();
    erased_node(&mut a, source, needle, occurrence);
    let r = apply(id, &mut a, r);
    print(&a, r)
}

#[test]
fn folded_value_never_uses_a_known_parent_to_hide_a_missing_operand() {
    for source in [
        "result=2*3",
        "result=2+3",
        "result=2/3",
        "result=2^3",
        "result=math.max(2,3)",
        "result=-2",
        "result=(2)",
    ] {
        let (code, o) = missing("constant-folding", source, "2", 0);
        assert!(o.unknown_bytes() > 0, "{source} => {code}");
    }
}
#[test]
fn missing_common_argument_is_not_recovered_from_an_equal_argument_at_another_call() {
    let source = "local function f(x,y)return x+y end a=f(1,200)b=f(3,200)";
    let (code, o) = missing("constant-argument-specialization", source, "200", 1);
    assert!(o.unknown_bytes() > 0, "{code}");
}
#[test]
fn missing_reused_expression_operand_is_not_recovered_from_its_parent() {
    let source =
        "function onTick()local a=input.getNumber(1)x=a*200 y=a*200 output.setNumber(1,x+y)end";
    let (code, o) = missing("available-expression-reuse", source, "200", 1);
    assert!(o.unknown_bytes() > 0, "{code}");
}
#[test]
fn folded_builtin_literal_call_retains_missing_actual_data() {
    let source = "result=math.abs(200)";
    let (code, o) = missing("literal-call-folding", source, "200", 0);
    assert!(o.unknown_bytes() > 0, "{code}");
}
#[test]
fn literal_operator_alternatives_keep_all_emitted_inner_nodes() {
    for source in [
        "result=x*.5",
        "result=x*(1/2)",
        "result=4*(1/3)",
        "result=x+(-2*y)",
        "result=x+(-y/3)",
        "result=(x&8)~=0",
        "result=(x&8)==0",
        "result=x+(-x)*y",
        "result=(x and 3)or(x and 4)",
        "result=(not x and 3)or x",
        "result=(not x)or(x and 4)",
        "result=not(not true)",
        "result=string.format('%03d',7)",
        "result=math.max(2,3)",
        "result=math.cos(0)",
        "result=('雪😀')..'a'",
        "if nil then a=1 elseif true then a=2 else a=3 end",
    ] {
        checked("constant-folding", source);
    }
}
#[test]
fn constant_argument_filtering_preserves_live_parameters_and_implicit_nil() {
    for source in [
        "local function f(a,b,c)return a+b+(c or 0)end x=f(1,input.getNumber(1))y=f(1,input.getNumber(2))",
        "local function f(a,b,c)return a+b+c end x=f(1,input.getNumber(1),3)y=f(1,input.getNumber(2),3)",
        "local function f(a,b)return a,b end x,y=f('雪😀',true)z=f('雪😀',true)",
        "local function f(a,b)return function(a)return a+b end end x=f(1,2)y=f(3,2)",
    ] {checked("constant-argument-specialization",source);}
}
#[test]
fn table_parameters_keep_missing_keys_preceding_and_following_parameters() {
    for source in [
        "local function f(a,t,c)return a+t.x+t.y+c end x=f(1,{x=2,y=3},4)y=f(5,{x=6,y=7},8)",
        "local function f(t)return t.x,t.y,t.missing end a,b,c=f({x=1,y=2})d,e=f({x=3,y=4})",
        "local function f(t)return t[1]+t['odd-key'] end a=f({1,['odd-key']=2})b=f({3,['odd-key']=4})",
        "local function f(t)return t.x,t.x end a,b=f({x=1})c,d=f({x=2})",
    ] {checked("table-parameter-scalarization",source);}
}
#[test]
fn missing_scalarized_table_argument_data_remains_unknown() {
    let source = "local function f(t)return t.x+t.y end a=f({x=12345,y=2})b=f({x=3,y=4})";
    let (code, o) = missing("table-parameter-scalarization", source, "12345", 0);
    assert!(o.unknown_bytes() > 0, "{code}");
}
#[test]
fn namespace_alias_chains_quoted_keys_and_dead_store_variants_are_mapped() {
    for source in [
        "local module={}module['long-key']=function(x)return x+1 end local alias=module if alias==nil then alias=true end function onTick()output.setNumber(1,alias['long-key'](2))end",
        "local module,retained module={}module.run=function(x)return x+1 end local alias=module if alias==nil then alias=true end function onTick()retained=alias.run(2)output.setNumber(1,retained)end",
        "local module={}module.value=7 module.unused=9 module.run=function()return module.value end if module==nil then module=true end function onTick()output.setNumber(1,module.run())end",
        "local module={}module.run=function(x)return x+1 end local alias=module if alias==nil then alias=true end alias.run=function(x)return x+2 end function onTick()output.setNumber(1,alias.run(2))end",
    ] {checked("closed-namespace-devirtualization",source);}
}
#[test]
fn no_dynamic_table_or_callback_operation_invents_static_source_mappings() {
    for (id,source) in [
        ("closed-namespace-devirtualization","local t={}t.run=function()return 1 end output.setNumber(1,t[input.getNumber(1)]())"),
        ("immutable-table-flattening","local t={nested={}}output.setBool(1,t.nested==t.nested)"),
        ("table-parameter-scalarization","local function f(t)return t[input.getNumber(1)]end a=f({1,2,3})"),
        ("sparse-boolean-decode-scalarization","local _ENV=_ENV output.setNumber(1,input.getNumber(1))"),
        ("callback-exclusive-function-slotting","function helper()return 1 end saved=helper function onTick()output.setNumber(1,helper())end function onDraw()screen.drawText(0,0,helper())end"),
        ("uniform-table-fill-scalarization","local a={}function onTick()for i=1,8 do a[i]=input.getNumber(i)end output.setNumber(1,a[2])end"),
    ] {checked(id,source);}
}
#[test]
fn uniform_table_range_guards_keep_bound_and_read_origins() {
    for source in [
        "local a={}function onTick()for i=1,8 do a[i]=7 end output.setNumber(1,a[0])output.setNumber(2,a[2])output.setNumber(3,a[9])end",
        "local a={}function onTick()for i=1,8 do a[i]=7 end for j=0,9 do output.setNumber(1,a[j])end end",
        "local a={}function onTick()for i=1,8 do a[i]=input.getNumber(1)end a[2]=9 output.setNumber(1,a[2])end",
    ] {checked("uniform-table-fill-scalarization",source);}
}
#[test]
fn interval_shifting_conjunction_and_operand_orders_keep_origins() {
    for source in [
        "function f(v,lower,width)return v>=lower and v<=lower+width end",
        "function f(v,lower,width)return lower<=v and lower+width>=v end",
        "function f(v,lower,width,flag)return v>=lower and v<=lower+width and flag end",
        "function f(v,lower,width)return v>=lower and v<=lower+width+1 end",
    ] {
        checked("interval-origin-shifting", source);
    }
}
#[test]
fn signed_coefficients_and_multiple_product_orders_keep_origins() {
    for source in [
        "k=input.getNumber(1)function onTick()a=input.getNumber(2)*-k*12345 b=input.getNumber(3)*-k*12345 output.setNumber(1,a+b)end",
        "k=input.getNumber(1)function onTick()a=12345*k*input.getNumber(2)b=12345*k*input.getNumber(3)output.setNumber(1,a+b)end",
        "k=input.getNumber(1)function onTick()a=input.getNumber(2)/k/12345 b=input.getNumber(3)/k/12345 output.setNumber(1,a+b)end",
    ] {checked("coefficient-carrier-synthesis",source);}
}
#[test]
fn arithmetic_helper_pipeline_retains_locations_after_multiple_passes() {
    let source = crate::provenance_pass_matrix::fixture("destructive-radix-helper-synthesis");
    let (mut a, mut r) = parse_source_with_origins("radix-pipeline.lua", &source).unwrap();
    for id in [
        "destructive-radix-helper-synthesis",
        "destructive-radix-terminal-reuse",
        "root-local-globalization",
        "function-local-globalization",
        "scope-renaming",
    ] {
        r = apply(id, &mut a, r);
        let (code, o) = print(&a, r);
        assert_eq!(o.unknown_bytes(), 0, "{id} {code}");
    }
}
#[test]
fn every_registered_fixture_is_also_checked_through_the_real_compiler() {
    use crate::{compile_code, CompileMode, CompileOptions, NumericMode};
    for &id in crate::pass_ids::OPTIMIZATION_PASS_IDS {
        let source = crate::provenance_pass_matrix::fixture(id);
        for (mode, numeric_mode, zero_cost_newlines) in [
            (CompileMode::Safe, NumericMode::Exact, false),
            (CompileMode::Safe, NumericMode::Exact, true),
            (CompileMode::Safe, NumericMode::Tolerant, false),
            (CompileMode::Safe, NumericMode::Tolerant, true),
            (CompileMode::Smallest, NumericMode::Exact, false),
            (CompileMode::Smallest, NumericMode::Exact, true),
            (CompileMode::Smallest, NumericMode::Tolerant, false),
            (CompileMode::Smallest, NumericMode::Tolerant, true),
        ] {
            let config = CompileOptions {
                mode,
                numeric_mode,
                zero_cost_newlines,
                origin_source: Some("pipeline-fixture.lua".into()),
                ..Default::default()
            };
            let result = compile_code(&source, &config).unwrap();
            let plain = compile_code(
                &source,
                &CompileOptions {
                    origin_source: None,
                    ..config
                },
            )
            .unwrap();
            assert_eq!(result.code, plain.code, "{id}");
            assert_eq!(
                result.stats.candidate_sizes, plain.stats.candidate_sizes,
                "{id}"
            );
            let origins = result.origins.unwrap();
            origins.validate_for_code(&result.code).unwrap();
            let gaps = origins
                .mappings
                .iter()
                .filter(|m| m.origin.is_none())
                .map(|m| &result.code[m.start..m.end])
                .collect::<Vec<_>>();
            assert_eq!(
                origins.unknown_bytes(),
                0,
                "{id} real compiler gaps {gaps:?}"
            );
        }
    }
}

#[test]
fn noneligible_local_declarations_keep_original_names_during_scratch_scan() {
    for source in [
        "function onTick()local x=input.getNumber(1)local first,second=x*2,x*2 output.setNumber(1,first+second)end",
        "function onTick()local x=input.getNumber(1)local untouched,first,second=1,x*2,x*2 output.setNumber(1,untouched+first+second)end",
    ] {checked("equal-scratch-value-coalescing",source);}
}
#[test]
fn filtered_parameter_names_remain_bound_to_original_declaration_offsets() {
    let source =
        "local function f(a,t,c)return a+t.x+t.y+c end x=f(1,{x=2,y=3},4)y=f(5,{x=6,y=7},8)";
    let (code, origins) = checked("table-parameter-scalarization", source);
    assert!(!code.contains("t.x"));
    let expected = source.find("t,c)").unwrap() + 2;
    assert!(origins
        .origins
        .iter()
        .any(|o| o.name.as_deref() == Some("c") && o.primary.unwrap().start == expected));
}

#[test]
fn shared_immutable_replacements_preserve_a_missing_use_operand() {
    for (id,source,needle,occurrence) in [
        ("immutable-global-expression-reuse","local shared=math.pi*200 function onTick()output.setNumber(1,math.pi*200)end","200",1),
        ("immutable-carrier-synthesis","a=123456 b=789012 function f0()return a+b end function f1()return a+b end function f2()return a+b end function f3()return a+b end","b",2),
    ] {
        let (code,o)=missing(id,source,needle,occurrence);
        assert!(o.unknown_bytes()>0,"{id}: a missing operand was hidden by the enclosing expression: {code}");
    }
}

#[test]
fn unicode_crlf_prefix_does_not_change_any_registered_transformation() {
    for &id in crate::pass_ids::OPTIMIZATION_PASS_IDS {
        let source = crate::provenance_pass_matrix::fixture(id);
        let decorated = format!("-- 雪😀 source map boundary\r\n{source}\r\n-- EOF 雪😀");
        let (ordinary, _) = checked(id, &source);
        let (relocated, origins) = checked(id, &decorated);
        assert_eq!(
            ordinary, relocated,
            "{id}: comments/line ending changed code"
        );
        for origin in &origins.origins {
            if let Some(span) = origin.primary {
                assert!(
                    span.start >= decorated.find(&source).unwrap(),
                    "{id}: origin incorrectly includes prefix comment"
                );
            }
        }
    }
}
