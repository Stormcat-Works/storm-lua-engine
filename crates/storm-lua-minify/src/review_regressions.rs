//! Executable regressions from the October 2026 minify review.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use crate::{compile_code, CompileMode, CompileOptions, NumericMode};
use mlua::{Lua, MultiValue, Value};

fn evaluate(source: &str) -> Result<Vec<String>, ()> {
    Lua::new()
        .load(source)
        .eval::<MultiValue>()
        .map(|values| {
            values
                .into_iter()
                .map(|value| match value {
                    Value::Nil => "nil".into(),
                    Value::Boolean(value) => format!("bool:{value}"),
                    Value::Integer(value) => format!("int:{value}"),
                    Value::Number(value) => format!("float:{:016x}", value.to_bits()),
                    Value::String(value) => format!("bytes:{:?}", value.as_bytes()),
                    other => panic!("unexpected result {other:?}"),
                })
                .collect()
        })
        .map_err(|_| ())
}
fn equivalent(source: &str, options: &CompileOptions) {
    let result =
        compile_code(source, options).unwrap_or_else(|error| panic!("{source:?}: {error}"));
    assert_eq!(
        evaluate(source),
        evaluate(&result.code),
        "source={source:?}\ngenerated={:?}\noptions={options:?}",
        result.code
    );
}
fn modes() -> impl Iterator<Item = CompileOptions> {
    [CompileMode::Safe, CompileMode::Smallest]
        .into_iter()
        .flat_map(|mode| {
            [NumericMode::Exact, NumericMode::Tolerant]
                .into_iter()
                .map(move |numeric_mode| CompileOptions {
                    mode,
                    numeric_mode,
                    ..Default::default()
                })
        })
}

#[test]
fn review_byte_strings_and_newlines_are_values_not_unicode_codepoints() {
    let mut sources = vec![
        r#"return "\u{1f600}","\u{d800}","\z　x","\2559""#.to_string(),
        "return 'a\\\n\rb'".into(),
    ];
    for byte in 0..=255 {
        sources.push(format!(
            "return '\\{byte:03}','\\x{byte:02x}',#'\\{byte:03}'"
        ));
    }
    for newline in ["\n", "\r", "\r\n", "\n\r"] {
        sources.push(format!("return [==[{newline}a{newline}b]==]"));
        sources.push(format!("-- comment{newline}return 42"));
    }
    for options in modes() {
        for source in &sources {
            equivalent(source, &options);
        }
    }
}

#[test]
fn review_hex_boundary_literals_are_exact() {
    for options in
        modes().filter(|o| o.mode == CompileMode::Safe || o.numeric_mode == NumericMode::Exact)
    {
        for literal in [
            "0x1.",
            "0x10000000000000000",
            "0x.8p1024",
            "0x2p-1075",
            "0x1.0000000000000800001p0",
            "0x1p-1075",
            "0x1.0000000000001p-1075",
            "0x1.fffffffffffff8p1023",
        ] {
            equivalent(&format!("return {literal}"), &options);
        }
    }
}

#[test]
fn review_typed_literal_arithmetic_preserves_lua_values_and_errors() {
    for options in modes() {
        for expression in [
            "0x8000000000000000//-1",
            "0x8000000000000000%-1",
            "5//-2,5%-2",
            "1.0%1e20",
            "1.0%-1e20",
            "1<<64,4>>-1,1<<-64,1>>0x8000000000000000",
            "~4294967296.0",
            "~1.5",
            "true+1",
            "nil-1",
            "'10'<2",
            "math.floor(true)",
            "9007199254740993>9007199254740992",
            "9007199254740993==9007199254740992.0",
            "math.min('2','10')",
            "1.0 .. 'x'",
            "math.abs(9007199254740993)",
            "string.format('%018446744073709551615d',1)",
            "string.format('%01000000000d',1)",
        ] {
            equivalent(&format!("return {expression}"), &options);
        }
    }
}

#[test]
fn review_identity_does_not_remove_numeric_coercion() {
    for options in modes() {
        for source in ["local x='3' return x*1", "local x=false return x+0"] {
            equivalent(source, &options);
        }
    }
}

#[test]
fn review_safe_float_subtype_zero_and_nonfinite_precedence() {
    for numeric_mode in [NumericMode::Exact, NumericMode::Tolerant] {
        let options = CompileOptions {
            mode: CompileMode::Safe,
            numeric_mode,
            ..Default::default()
        };
        for source in [
            "return math.type(1/1)",
            "return 1/-0.0",
            "return 2/1e999",
            "return 2//1e999",
            "return 2%1e999",
            "return (-1.0)^2",
        ] {
            equivalent(source, &options);
        }
    }
}

#[test]
fn review_table_prefixes_remain_valid_lua() {
    for options in modes() {
        for source in [
            "local a=({{}})[1] return #a",
            "return ({x=42}).x",
            "return (function()return 42 end)()",
        ] {
            equivalent(source, &options);
        }
    }
}

#[test]
fn review_excessive_nesting_is_a_diagnostic() {
    for source in [
        format!("return {}1{}", "(".repeat(6400), ")".repeat(6400)),
        format!("return {}1", "not ".repeat(6400)),
        format!("return 1{}", "+1".repeat(6400)),
        format!("{}{}", "do ".repeat(6400), "end ".repeat(6400)),
    ] {
        assert!(compile_code(&source, &CompileOptions::default())
            .unwrap_err()
            .contains("nesting"));
    }
}

#[test]
fn review_table_captures_mutations_and_integer_keys() {
    for options in modes() {
        for source in [
            "local function f(t)local g=function()return t.x end return t.x+g()end return f({x=3})",
            "local t={} function t.f()return 3 end return t.f()",
            "local t={v=3} function t:f()return self.v end return t:f()",
            "local t={[9007199254740992]=10,[9007199254740993]=20} return t[9007199254740993]",
            "local t={} return t.missing.child",
            "local t={} return t.missing",
            "local t={2,[1]=1} return t[1]",
            "local t={[1]=1,2} return t[1]",
            "local t={x=1,x=2} return t.x",
            "local t={['\\255']=10,['ÿ']=20} return t['\\255'],t['ÿ']",
        ] {
            equivalent(source, &options);
        }
    }
}
#[test]
fn review_safe_induction_and_boolean_consumers_preserve_subtype() {
    for numeric_mode in [NumericMode::Exact, NumericMode::Tolerant] {
        let options = CompileOptions {
            mode: CompileMode::Safe,
            numeric_mode,
            ..Default::default()
        };
        for source in [
            "for i=1.0,1.0 do return math.type(math.floor(i)),math.type(i)end",
            "local x=3 local bit=x&1>0 return math.type(bit and 1.0 or 0.0)",
        ] {
            equivalent(source, &options);
        }
    }
}
#[test]
fn review_signed_factoring_observes_the_matched_statement_write() {
    for options in modes() {
        equivalent(
            "local x=1 local a,b,c x=x*12345 a=x*12345 b=x*12345 c=x*12345 return x,a,b,c",
            &options,
        );
    }
}

#[test]
fn review_exact_consumers_follow_intermediate_results() {
    for options in modes() {
        for source in [
            "local function f()return 1.23456789 end local v=f() return string.pack('d',v)",
            "local c=1.23456789 local function f()local v=c*2 return v end local v=f() return string.pack('d',v)",
            "local t={[1.23456789]=42} return t[1.23456789]",
        ] { equivalent(source, &options); }
    }
}
#[test]
fn review_destructive_results_preserve_indirect_observers_and_aliases() {
    for options in modes() {
        for source in [
            "v=100 local observed function observe()observed=v end function f(x)x=x+1 observe()return x end v=f(1)v=f(2)return v,observed",
            "function f(x)x=x+1 return x end local g=f v=f(1)v=f(2)return v,g(10)",
            "local x x=1 goto done x=2 ::done:: return x",
            "local a a=123456 function f()return a+789012 end local a a=222222 function g()return a+789012 end return f(),g()",
        ] { equivalent(source, &options); }
    }
}

#[test]
fn review_api_tables_escaping_through_parentheses_and_returns_are_mutable() {
    for options in modes() {
        for source in [
        "local function mutate(t)t.abs=function(x)return 7 end end mutate((math)) return math.abs(-3)",
        "local function get()return math end local m=get()m.abs=function(x)return 7 end return math.abs(-3)",
        "local t={} t.m=math t.m.abs=function(x)return 7 end return math.abs(-3)",
    ] { equivalent(source, &options); }
    }
}

#[test]
fn review_carrier_reassociation_does_not_remove_existing_integer_wrap() {
    for options in
        modes().filter(|o| o.mode == CompileMode::Safe || o.numeric_mode == NumericMode::Exact)
    {
        equivalent("tau=math.pi*2 function f()local x=0x7fffffffffffffff return x*2*math.pi,x*2*math.pi end return f()", &options);
        equivalent("k=2 function f()local x=0x7fffffffffffffff return x*k*1.23456789,x*k*1.23456789 end return f()", &options);
    }
}
