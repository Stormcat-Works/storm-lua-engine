//! Registered-pass contract: a real transform, unchanged code with metadata,
//! complete tracked output, erased-source negative, and lossless JSON transfer.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use crate::passes;
use std::collections::BTreeSet;
use storm_lua_syntax::provenance::{GeneratedOrigins, OriginKind};
use storm_lua_syntax::{parse_source, parse_source_with_origins, Ast, NodeId, Printer};

pub(crate) fn apply(id: &str, a: &mut Ast, r: NodeId) -> NodeId {
    match id {
        "constant-folding" => passes::literal_folding::constant_fold(a, r, true).root,
        "numeric-literal-approximation" => {
            passes::numeric_literals::shorten_numeric_literals(a, r).root
        }
        "binding-unit-rescaling" => passes::binding_rescaling::rescale_bindings(a, r).root,
        "interval-origin-shifting" => passes::interval_shifting::shift_interval_origins(a, r).root,
        "integer-loop-call-folding" => {
            passes::induction_values::fold_integer_induction_calls(a, r).root
        }
        "color-unpack-specialization" => {
            passes::color_unpack::specialize_color_unpack_helpers(a, r).root
        }
        "constant-argument-specialization" => {
            passes::inline_functions::specialize_constant_arguments(a, r, false).root
        }
        "literal-call-folding" => {
            passes::inline_functions::fold_literal_function_calls(a, r, true).root
        }
        "split-sign-recomposition-elimination" => {
            passes::split_sign::eliminate_split_sign_recomposition(a, r, true).root
        }
        "equivalent-trailing-argument-omission" => {
            passes::omit_arguments::omit_equivalent_trailing_arguments(a, r).root
        }
        "table-parameter-scalarization" => {
            passes::tables::scalarize_table_literal_parameters(a, r, 16).root
        }
        "sparse-boolean-decode-scalarization" => {
            passes::sparse_boolean::sparse_boolean_decode_scalarization(a, r, true).root
        }
        "numeric-boolean-bit-specialization" => {
            passes::boolean_numerics::specialize_numeric_booleans(a, r).root
        }
        "quotient-remainder-fusion" => {
            passes::quotient_remainder::fuse_quotient_remainder(a, r).root
        }
        "destructive-radix-helper-synthesis" => {
            passes::radix_helpers::synthesize_destructive_radix_helpers(a, r, true, 4).root
        }
        "destructive-radix-terminal-reuse" => {
            passes::radix_helpers::reuse_terminal_radix_quotients(a, r, true, 8).root
        }
        "quotient-chain-fusion" => passes::decode_chains::fuse_quotient_chains(a, r).root,
        "captured-single-use-hoisting" => {
            passes::captured_use::hoist_captured_single_uses(a, r).root
        }
        "immutable-global-expression-reuse" => {
            passes::immutable_values::reuse_immutable_values(a, r).root
        }
        "multiplicative-carrier-reassociation" => {
            passes::multiplicative_carriers::reuse_multiplicative_carriers(a, r).root
        }
        "coefficient-carrier-synthesis" => {
            passes::coefficient_carriers::synthesize_coefficient_carriers(a, r, true, 8).root
        }
        "destructive-result-globalization" => {
            passes::destructive_results::globalize_destructive_results(a, r).root
        }
        "immutable-carrier-synthesis" => {
            passes::immutable_synthesis::synthesize_immutable_carriers(a, r).root
        }
        "callback-exclusive-function-slotting" => {
            passes::callback_slots::slot_callback_exclusive_functions(a, r).root
        }
        "screen-button-outlining" => {
            passes::screen_buttons::outline_screen_buttons(a, r, true).root
        }
        "else-default-hoisting" => passes::default_hoisting::hoist_else_defaults(a, r).root,
        "conditional-call-lowering" => passes::conditionals::lower_conditional_calls(a, r).root,
        "conditional-assignment-lowering" => {
            passes::conditionals::lower_conditional_assignments(a, r).root
        }
        "write-only-table-field-cleanup" => {
            passes::tables::remove_write_only_nil_table_fields(a, r).root
        }
        "immutable-table-flattening" => passes::tables::flatten_immutable_tables(a, r, 8).root,
        "closed-namespace-scalarization" => {
            passes::tables::scalarize_closed_namespaces(a, r, 8).root
        }
        "uniform-table-fill-scalarization" => {
            passes::uniform_tables::scalarize_uniform_fill_tables(a, r).root
        }
        "one-use-function-fusion" => {
            passes::inline_functions::inline_one_use_statement_and_tail_functions(a, r, 32).root
        }
        "expression-helper-inlining" => {
            passes::inline_functions::inline_expression_functions(a, r, true, true).root
        }
        "tiny-literal-inlining" => passes::locals::inline_tiny_literal_bindings(a, r).root,
        "single-use-local-sinking" => passes::locals::inline_single_use_locals(a, r).root,
        "dead-local-elimination" => passes::locals::eliminate_dead_locals(a, r).root,
        "terminal-scope-flattening" => passes::locals::flatten_terminal_do_blocks(a, r).root,
        "ordered-screen-call-factoring" => {
            passes::screen_call_factoring::factor_repeated_screen_calls(a, r).root
        }
        "constant-wrapper-merging" => {
            passes::wrapper_functions::merge_translated_constant_wrappers(a, r).root
        }
        "affine-wrapper-merging" => {
            passes::wrapper_functions::merge_affine_constant_wrappers(a, r).root
        }
        "root-local-globalization" => passes::root_globals::globalize_root_locals(a, r).root,
        "function-local-globalization" => {
            passes::function_globalization::globalize_function_locals(a, r, 0).root
        }
        "hybrid-function-local-globalization" => {
            passes::function_globalization::globalize_function_locals(a, r, 3).root
        }
        "global-store-cleanup" => passes::global_stores::cleanup_global_stores(a, r).root,
        "single-use-global-forwarding" => {
            passes::single_use_forwarding::forward_single_use_globals(a, r).root
        }
        "equal-scratch-value-coalescing" => {
            passes::scratch_coalescing::coalesce_equal_scratch_values(a, r).root
        }
        "unwritten-global-nil-propagation" => {
            passes::global_stores::propagate_unwritten_globals_as_nil(a, r).root
        }
        "temporary-global-packing" => passes::temporary_globals::pack_temporary_globals(a, r).root,
        "output-sequence-loop-synthesis" => {
            passes::output_loops::synthesize_output_loops(a, r).root
        }
        "common-offset-absorption" => {
            passes::common_offsets::absorb_common_offsets(a, r, true).root
        }
        "ordered-screen-loop-synthesis" => {
            passes::screen_loops::synthesize_screen_loops(a, r, false).root
        }
        "periodic-screen-loop-synthesis" => {
            passes::screen_loops::synthesize_screen_loops(a, r, true).root
        }
        "available-expression-reuse" => {
            passes::available_expressions::reuse_available_expressions(a, r).root
        }
        "signed-expression-factoring" => {
            passes::signed_factoring::factor_signed_expressions(a, r, true, 16).root
        }
        "one-use-expression-helper-reversal" => {
            passes::inline_functions::inline_one_use_expression_helpers(a, r, true).root
        }
        "final-dead-store-elimination" => {
            passes::final_stores::eliminate_overwritten_assignments(a, r).root
        }
        "redundant-parentheses-elimination" => {
            passes::parentheses::remove_redundant_parentheses(a, r).root
        }
        "scope-renaming" => {
            let result = crate::scope_rename::scope_rename(a, r);
            *a = result.ast;
            result.root
        }
        "api-alias-optimization" => passes::api_aliases::optimize_api_aliases(a, r).root,
        "closed-table-field-renaming" => passes::closed_fields::rename_closed_fields(a, r).root,
        "closed-namespace-devirtualization" => {
            passes::namespace_functions::devirtualize_closed_namespaces(a, r, true).root
        }
        "exact-numeric-literal-pooling" => {
            passes::literal_pool::pool_numeric_literals(a, r, true).root
        }
        "ordered-draw-record-packing" => passes::draw_records::pack_draw_records(a, r, true).root,
        "repeated-draw-sequence-outlining" => {
            passes::draw_sequences::outline_draw_sequences(a, r, true).root
        }
        "adjacent-local-declaration-packing" => {
            passes::adjacent_locals::pack_adjacent_locals(a, r, false).root
        }
        "conditional-return-lowering" => passes::conditionals::lower_conditional_returns(a, r).root,
        _ => panic!("unregistered fixture dispatch {id}"),
    }
}

pub(crate) fn fixture(id: &str) -> String {
    match id {
        "constant-folding" => r#"result=2*3 if false then result=99 end output.setNumber(1,result)"#.to_owned(),
        "numeric-literal-approximation" => r#"x=1.23456789012345 output.setNumber(1,x)"#.to_owned(),
        "binding-unit-rescaling" => r#"size=192 function onTick()x=input.getNumber(1)output.setNumber(1,(size/2+x*size/2)//1)output.setNumber(2,size)end"#.to_owned(),
        "interval-origin-shifting" => r#"function inside(lower,width)return input.getNumber(1)>=lower and input.getNumber(1)<=lower+width end"#.to_owned(),
        "integer-loop-call-folding" => r#"for i=1,10 do output.setNumber(1,math.floor(i))end"#.to_owned(),
        "color-unpack-specialization" => r#"function color(c)screen.setColor(table.unpack(c))end function onDraw()color({1,2,3})color({4,5,6})end"#.to_owned(),
        "constant-argument-specialization" => r#"local function f(x,y)return x+y end a=f(1,2)b=f(3,2)"#.to_owned(),
        "literal-call-folding" => r#"local function f(x)return x+1 end a=f(3)"#.to_owned(),
        "split-sign-recomposition-elimination" => r#"x=input.getNumber(1)y=math.max(x,0)^2*3-math.min(x,0)^2*3"#.to_owned(),
        "equivalent-trailing-argument-omission" => r#"x=0 local function f(a,b)if a then x=b end end function onTick()f(true,1)f(false,0)output.setNumber(1,x)end"#.to_owned(),
        "table-parameter-scalarization" => r#"local function compute(t)return t.x+t.y+t.z end a=compute({x=1,y=2,z=3})b=compute({x=4,y=5,z=6})"#.to_owned(),
        "sparse-boolean-decode-scalarization" => r#"local current,old,pulse={},{},{} local function decode(data)local out={}for i=1,8 do out[i]=(data&(1<<(i-1)))~=0 end return out end function onTick()old=current current=decode(input.getNumber(1))for i=1,8 do pulse[i]=not old[i]and current[i]end output.setBool(1,current[2])output.setBool(2,pulse[3])end"#.to_owned(),
        "numeric-boolean-bit-specialization" => r#"function onTick()local bits=input.getNumber(1)local flag=bits&256>0 output.setNumber(1,flag and 1 or 0)end"#.to_owned(),
        "quotient-remainder-fusion" => r#"r=x%10 x=(x-r)/10 output.setNumber(1,x+r)"#.to_owned(),
        "destructive-radix-helper-synthesis" => r#"function onTick()p=input.getNumber(1)r=p%100001 p=p//100001 r=p%1251 p=p//1251 r=p%60001 p=p//60001 r=p%15001 p=p//15001 r=p%100001 p=p//100001 output.setNumber(1,p+r)end"#.to_owned(),
        "destructive-radix-terminal-reuse" => r#"function extract(base)remainder=number%base number=number//base end function onTick()number=input.getNumber(1)remainder=number%100001 a=number//100001 b=number//100001+1 number=0 output.setNumber(1,a+b+remainder)end"#.to_owned(),
        "quotient-chain-fusion" => r#"x=input.getNumber(1)x=x//10 x=x//10 output.setNumber(1,x)"#.to_owned(),
        "captured-single-use-hoisting" => r#"a=123 x=1 y=a"#.to_owned(),
        "immutable-global-expression-reuse" => r#"local angle=math.pi function onTick()output.setNumber(1,math.pi)output.setNumber(2,math.pi*2)end"#.to_owned(),
        "multiplicative-carrier-reassociation" => r#"tau=math.pi*2 function onTick()x=input.getNumber(1)output.setNumber(1,x*math.pi*2)end"#.to_owned(),
        "coefficient-carrier-synthesis" => r#"k=input.getNumber(1)function onTick()a=input.getNumber(2)*k*12345 b=input.getNumber(3)*k*12345 output.setNumber(1,a+b)end"#.to_owned(),
        "destructive-result-globalization" => r#"function f(v)v=v//10 return v end function onTick()value=f(input.getNumber(1))value=f(value)output.setNumber(1,value)end"#.to_owned(),
        "immutable-carrier-synthesis" => r#"a=123456 b=789012 function f0()return a+b end function f1()return a+b end function f2()return a+b end function f3()return a+b end"#.to_owned(),
        "callback-exclusive-function-slotting" => format!("{} function tickHelper(x)return x+1 end function drawHelper(x)return x+2 end function onTick()output.setNumber(1,tickHelper(input.getNumber(1)))end function onDraw()screen.drawText(0,0,drawHelper(input.getNumber(2)))end",(0..55).map(|i|format!("s{i}=0 ")).collect::<String>()),
        "screen-button-outlining" => format!("value=0 active=false pulse=false function hit(x,w,y,h)return input.getBool(1)end function onColor()screen.setColor(180,180,200)end function offColor()screen.setColor(35,30,30)end function onDraw(){}end",(0..4).map(|i|format!("if hit({},5,{},5)and active then if pulse then value=value+{} end onColor()else offColor()end screen.drawText({}+1,{},'B') ",i*6+1,i*6+2,i+1,i*6+1,i*6+2)).collect::<String>()),
        "else-default-hoisting" => r#"x=0 y=0 function onTick()if input.getBool(1)then x=input.getNumber(1)y=input.getNumber(2)else x,y=0,0 end output.setNumber(1,x)output.setNumber(2,y)end"#.to_owned(),
        "conditional-call-lowering" => r#"if flag then output.setNumber(1,1)else output.setNumber(1,2)end"#.to_owned(),
        "conditional-assignment-lowering" => r#"if flag then value=1 else value=2 end output.setNumber(1,value)"#.to_owned(),
        "write-only-table-field-cleanup" => r#"local state={}state.unused=nil output.setNumber(1,2)"#.to_owned(),
        "immutable-table-flattening" => r#"local t={x={y=12345},z=7}function onTick()output.setNumber(1,t.x.y+t.z)end"#.to_owned(),
        "closed-namespace-scalarization" => r#"local state={amount=1}function onTick()state.amount=state.amount+1 output.setNumber(1,state.amount)end"#.to_owned(),
        "uniform-table-fill-scalarization" => r#"local a={}function onTick()for i=1,8 do a[i]=input.getNumber(18)end output.setNumber(1,a[2])output.setBool(1,a[9]==nil)end"#.to_owned(),
        "one-use-function-fusion" => r#"local function update(v)local x=v+1 output.setNumber(1,x)end function onTick()update(input.getNumber(1))end"#.to_owned(),
        "expression-helper-inlining" => r#"local function add(v)return v+1 end function onTick()output.setNumber(1,add(input.getNumber(1)))output.setNumber(2,add(input.getNumber(2)))end"#.to_owned(),
        "tiny-literal-inlining" => r#"local tiny=1 output.setNumber(1,tiny)"#.to_owned(),
        "single-use-local-sinking" => r#"local sum=left+right result=sum*2 output.setNumber(1,result)"#.to_owned(),
        "dead-local-elimination" => r#"local unused=3 output.setNumber(1,2)"#.to_owned(),
        "terminal-scope-flattening" => r#"function onTick()do output.setNumber(1,2)end end"#.to_owned(),
        "ordered-screen-call-factoring" => r#"function onDraw()screen.setColor(123,234,56,78)screen.drawText(0,0,"a")screen.setColor(123,234,56,78)screen.drawText(0,6,"b")screen.setColor(123,234,56,78)screen.drawText(0,12,"c")screen.setColor(123,234,56,78)end"#.to_owned(),
        "constant-wrapper-merging" => r#"function dark()screen.setColor(45,45,55)end function bg()screen.setColor(60,60,70)end function onDraw()dark()bg()dark()bg()dark()bg()end"#.to_owned(),
        "affine-wrapper-merging" => r#"function warm()screen.setColor(180,180,200)end function dark()screen.setColor(35,30,30)end function onDraw()warm()dark()warm()dark()warm()dark()end"#.to_owned(),
        "root-local-globalization" => r#"local x=1 local function helper(v)return x+v end function onTick()output.setNumber(1,helper(2))end"#.to_owned(),
        "function-local-globalization" => r#"function onTick()local value=input.getNumber(1)output.setNumber(1,value)end"#.to_owned(),
        "hybrid-function-local-globalization" => r#"function onTick()local value=input.getNumber(1)value=value+1 output.setNumber(1,value)output.setNumber(2,value)end"#.to_owned(),
        "global-store-cleanup" => r#"function onTick()unused=4 x=1 x=2 output.setNumber(1,x)end"#.to_owned(),
        "single-use-global-forwarding" => r#"function onTick()temporary=input.getNumber(1)output.setNumber(1,temporary)end"#.to_owned(),
        "equal-scratch-value-coalescing" => r#"function onTick()local x=input.getNumber(1)__fs0,__fs1=x*2,x*2 output.setNumber(1,__fs0)output.setNumber(2,__fs1)end"#.to_owned(),
        "unwritten-global-nil-propagation" => r#"function onTick()output.setBool(1,pcall==nil)end"#.to_owned(),
        "temporary-global-packing" => r#"function onTick()first=input.getNumber(1)output.setNumber(1,first)second=input.getNumber(2)output.setNumber(2,second)end"#.to_owned(),
        "output-sequence-loop-synthesis" => format!("function onTick(){}end",(1..=12).map(|i|format!("output.setNumber({i},{i}) ")).collect::<String>()),
        "common-offset-absorption" => r#"function onTick()a=input.getNumber(1)b=input.getNumber(2)c=input.getNumber(3)t={-a+b+.022+c,a+b+.022+c,-a-b-.022+c,a-b-.022+c}b=0 output.setNumber(1,t[1])end"#.to_owned(),
        "ordered-screen-loop-synthesis" => format!("function onDraw(){}end",(0..32).map(|i|format!("screen.drawLine({i},0,{i},20) ")).collect::<String>()),
        "periodic-screen-loop-synthesis" => format!("function onDraw(){}end",(0..32).map(|i|format!("screen.drawLine({},0,{},20) ",i%4,i%4)).collect::<String>()),
        "available-expression-reuse" => r#"function onTick()local a=input.getNumber(1)x=a*2 y=a*2 output.setNumber(1,x+y)end"#.to_owned(),
        "signed-expression-factoring" => r#"function onTick()local x=input.getNumber(1)a=(x+123456)*2 b=-(x+123456)*3 c=(x+123456)*4 output.setNumber(1,a+b+c)end"#.to_owned(),
        "one-use-expression-helper-reversal" => r#"local function helper(v)return v+1 end function onTick()output.setNumber(1,helper(input.getNumber(1)))end"#.to_owned(),
        "final-dead-store-elimination" => r#"function onTick()x=1 x=2 output.setNumber(1,x)end"#.to_owned(),
        "redundant-parentheses-elimination" => r#"result=((value+1)) output.setNumber(1,result)"#.to_owned(),
        "scope-renaming" => r#"local descriptive=1 function onTick()output.setNumber(1,descriptive)end"#.to_owned(),
        "api-alias-optimization" => r#"function onTick()output.setNumber(1,1)output.setNumber(2,2)output.setNumber(3,3)output.setNumber(4,4)end"#.to_owned(),
        "closed-table-field-renaming" => r#"local state={longDescriptiveField=1}function onTick()state.longDescriptiveField=state.longDescriptiveField+1 output.setNumber(1,state.longDescriptiveField)end"#.to_owned(),
        "closed-namespace-devirtualization" => r#"local module={}module.run=function(x)return x+1 end local alias=module if alias==nil then alias=true end function onTick()output.setNumber(1,alias.run(2))output.setNumber(2,module.run(3))end"#.to_owned(),
        "exact-numeric-literal-pooling" => r#"function onTick()output.setNumber(1,12345678)output.setNumber(2,12345678)output.setNumber(3,12345678)end"#.to_owned(),
        "ordered-draw-record-packing" => format!("function onDraw(){}end",(0..96).map(|i|format!("screen.drawRectF({},{},{},{}) ",(i*7)%127,(i*11)%61,1+i%3,2+i%2)).collect::<String>()),
        "repeated-draw-sequence-outlining" => format!("function onDraw(){}end",(0..6).map(|i|format!("screen.drawLine(1,2,3,4)screen.drawText(5,6,'repeated long label')screen.drawRectF(7,8,9,10)screen.setColor({i},1,2) ")).collect::<String>()),
        "adjacent-local-declaration-packing" => r#"local first=1 local second=2 output.setNumber(1,first+second)"#.to_owned(),
        "conditional-return-lowering" => r#"function f()if flag then return 1 else return 2 end end"#.to_owned(),
        _=>panic!("missing fixture {id}")
    }
}

fn run(id: &str) {
    let source = fixture(id);
    let (mut a, root) = parse_source_with_origins("registered-pass.lua", &source).unwrap();
    let (mut plain, plain_root) = parse_source(&source).unwrap();
    let before = Printer::new(&plain, false).output(plain_root);
    let root = apply(id, &mut a, root);
    let plain_root = apply(id, &mut plain, plain_root);
    assert!(
        a.nodes.tracks_origins(),
        "{id}: transformation discarded the entire origin arena"
    );
    assert!(a == plain, "{id}: origin collection changed syntax");
    let compact = Printer::new(&plain, false).output(plain_root);
    assert_ne!(
        compact, before,
        "{id}: fixture must exercise a real transformation, not a no-op"
    );
    for zero in [false, true] {
        let printed = Printer::new(&a, zero).output_with_positions(root);
        assert_eq!(
            printed.code,
            Printer::new(&plain, zero).output(plain_root),
            "{id}"
        );
        let origins = GeneratedOrigins::from_print(&a, &printed).unwrap();
        origins.validate_for_code(&printed.code).unwrap();
        let gaps = origins
            .mappings
            .iter()
            .filter(|m| m.origin.is_none())
            .map(|m| &printed.code[m.start..m.end])
            .collect::<Vec<_>>();
        assert_eq!(
            origins.unknown_bytes(),
            0,
            "{id}: gaps {gaps:?} in {}",
            printed.code
        );
        assert!(origins
            .origins
            .iter()
            .filter(|o| o.kind == OriginKind::Synthetic)
            .all(|o| o.primary.is_none() && o.name.is_none()));
        let decoded: Ast = serde_json::from_slice(&serde_json::to_vec(&a).unwrap()).unwrap();
        let again = Printer::new(&decoded, zero).output_with_positions(root);
        assert_eq!(again.code, printed.code);
        assert_eq!(
            GeneratedOrigins::from_print(&decoded, &again).unwrap(),
            origins
        );
    }
    // Missing original information must not be inferred from text, NodeId, or a
    // surrounding statement. This does not forbid genuinely synthetic plumbing.
    let (mut missing, missing_root) = parse_source_with_origins("missing.lua", &source).unwrap();
    for n in 0..missing.nodes.len() {
        let value = missing.nodes[n].clone();
        missing.nodes[n] = value;
    }
    let missing_root = apply(id, &mut missing, missing_root);
    let printed = Printer::new(&missing, false).output_with_positions(missing_root);
    assert_eq!(
        printed.code, compact,
        "{id}: unavailable origins changed optimization choices"
    );
    let origins = GeneratedOrigins::from_print(&missing, &printed).unwrap();
    origins.validate_for_code(&printed.code).unwrap();
    if !printed.code.is_empty() {
        assert!(
            origins.unknown_bytes() > 0,
            "{id}: erased original data was manufactured as known"
        );
    }
}

macro_rules! pass_cases {
    ($( $test:ident => $id:literal ),* $(,)?) => {
        $(#[test] fn $test(){run($id);})*
        #[test] fn inventory_matches_every_registered_pass() {
            let tested=BTreeSet::from([$($id),*]);
            let registered=crate::pass_ids::OPTIMIZATION_PASS_IDS.iter().copied().collect::<BTreeSet<_>>();
            assert_eq!(tested,registered);
            assert_eq!(tested.len(),67);
        }
    }
}
pass_cases! {
    constant_folding => "constant-folding",
    numeric_literal_approximation => "numeric-literal-approximation",
    binding_unit_rescaling => "binding-unit-rescaling",
    interval_origin_shifting => "interval-origin-shifting",
    integer_loop_call_folding => "integer-loop-call-folding",
    color_unpack_specialization => "color-unpack-specialization",
    constant_argument_specialization => "constant-argument-specialization",
    literal_call_folding => "literal-call-folding",
    split_sign_recomposition_elimination => "split-sign-recomposition-elimination",
    equivalent_trailing_argument_omission => "equivalent-trailing-argument-omission",
    table_parameter_scalarization => "table-parameter-scalarization",
    sparse_boolean_decode_scalarization => "sparse-boolean-decode-scalarization",
    numeric_boolean_bit_specialization => "numeric-boolean-bit-specialization",
    quotient_remainder_fusion => "quotient-remainder-fusion",
    destructive_radix_helper_synthesis => "destructive-radix-helper-synthesis",
    destructive_radix_terminal_reuse => "destructive-radix-terminal-reuse",
    quotient_chain_fusion => "quotient-chain-fusion",
    captured_single_use_hoisting => "captured-single-use-hoisting",
    immutable_global_expression_reuse => "immutable-global-expression-reuse",
    multiplicative_carrier_reassociation => "multiplicative-carrier-reassociation",
    coefficient_carrier_synthesis => "coefficient-carrier-synthesis",
    destructive_result_globalization => "destructive-result-globalization",
    immutable_carrier_synthesis => "immutable-carrier-synthesis",
    callback_exclusive_function_slotting => "callback-exclusive-function-slotting",
    screen_button_outlining => "screen-button-outlining",
    else_default_hoisting => "else-default-hoisting",
    conditional_call_lowering => "conditional-call-lowering",
    conditional_assignment_lowering => "conditional-assignment-lowering",
    write_only_table_field_cleanup => "write-only-table-field-cleanup",
    immutable_table_flattening => "immutable-table-flattening",
    closed_namespace_scalarization => "closed-namespace-scalarization",
    uniform_table_fill_scalarization => "uniform-table-fill-scalarization",
    one_use_function_fusion => "one-use-function-fusion",
    expression_helper_inlining => "expression-helper-inlining",
    tiny_literal_inlining => "tiny-literal-inlining",
    single_use_local_sinking => "single-use-local-sinking",
    dead_local_elimination => "dead-local-elimination",
    terminal_scope_flattening => "terminal-scope-flattening",
    ordered_screen_call_factoring => "ordered-screen-call-factoring",
    constant_wrapper_merging => "constant-wrapper-merging",
    affine_wrapper_merging => "affine-wrapper-merging",
    root_local_globalization => "root-local-globalization",
    function_local_globalization => "function-local-globalization",
    hybrid_function_local_globalization => "hybrid-function-local-globalization",
    global_store_cleanup => "global-store-cleanup",
    single_use_global_forwarding => "single-use-global-forwarding",
    equal_scratch_value_coalescing => "equal-scratch-value-coalescing",
    unwritten_global_nil_propagation => "unwritten-global-nil-propagation",
    temporary_global_packing => "temporary-global-packing",
    output_sequence_loop_synthesis => "output-sequence-loop-synthesis",
    common_offset_absorption => "common-offset-absorption",
    ordered_screen_loop_synthesis => "ordered-screen-loop-synthesis",
    periodic_screen_loop_synthesis => "periodic-screen-loop-synthesis",
    available_expression_reuse => "available-expression-reuse",
    signed_expression_factoring => "signed-expression-factoring",
    one_use_expression_helper_reversal => "one-use-expression-helper-reversal",
    final_dead_store_elimination => "final-dead-store-elimination",
    redundant_parentheses_elimination => "redundant-parentheses-elimination",
    scope_renaming => "scope-renaming",
    api_alias_optimization => "api-alias-optimization",
    closed_table_field_renaming => "closed-table-field-renaming",
    closed_namespace_devirtualization => "closed-namespace-devirtualization",
    exact_numeric_literal_pooling => "exact-numeric-literal-pooling",
    ordered_draw_record_packing => "ordered-draw-record-packing",
    repeated_draw_sequence_outlining => "repeated-draw-sequence-outlining",
    adjacent_local_declaration_packing => "adjacent-local-declaration-packing",
    conditional_return_lowering => "conditional-return-lowering",
}
