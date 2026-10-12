//! Compile a logical project and explicitly run its artifact in a vehicle VM.
use std::{collections::BTreeMap, error::Error};
use storm_lua_analysis::{analyze, AnalyzeOptions, LuaProject};
use storm_lua_build::public_api::ApiNumericMode;
use storm_lua_build::{build, ApiCompileOptions, ApiProjectCompileOptions};
use storm_lua_microcontroller::{Microcontroller, MicrocontrollerConfig};
use storm_lua_spec::{
    io::CompositeSignal,
    property::{PropertyBag, PropertyValue},
};

fn main() -> Result<(), Box<dyn Error>> {
    let project = LuaProject {
        entry: "main".into(),
        modules: BTreeMap::from([
            ("main".into(), "local calc=require('calc')\nfunction onTick()output.setNumber(1,calc.apply(input.getNumber(1)))end".into()),
            ("calc".into(), "local gain=property.getNumber('Gain')\nreturn {apply=function(x)return x*gain end}".into()),
        ]),
        ambient: BTreeMap::new(),
    };
    let analysis = analyze(&project, &AnalyzeOptions::default());
    println!("diagnostics: {}", analysis.diagnostics.len());
    let result = build(
        &project,
        &ApiProjectCompileOptions {
            metadata_tag: None,
            compile: ApiCompileOptions {
                numeric_mode: Some(ApiNumericMode::Exact),
                zero_cost_newlines: Some(false),
                ..Default::default()
            },
            minify: Some(true),
        },
    );
    if !result.ok {
        return Err(format!("build failed: {:?}", result.diagnostics).into());
    }
    let code = result.code.ok_or("compiler returned no artifact")?;
    println!("generated: {code}");
    let mut properties = PropertyBag::default();
    properties.insert(b"Gain".to_vec(), PropertyValue::Number(2.0));
    let mut vm = Microcontroller::new(MicrocontrollerConfig {
        properties,
        ..Default::default()
    })?;
    vm.load(code.as_bytes(), "=compiled-project")?;
    let mut input = CompositeSignal::default();
    input.numbers[0] = 3.0;
    vm.tick(&input)?;
    println!("input: 3, Gain: 2, output: {}", vm.output().numbers[0]);
    if vm.output().numbers[0] != 6.0 {
        return Err("unexpected compiled program output".into());
    }
    Ok(())
}
