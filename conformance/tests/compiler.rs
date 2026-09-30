//! Public compiler APIs exercised with the real vehicle runtime and rasterizer.
//! All Lua below is a self-contained synthetic program; no private corpus is used.
use std::{collections::BTreeMap, error::Error};
use storm_lua_analysis::{analyze, AnalyzeOptions, LuaProject};
use storm_lua_build::public_api::{finish_compile_result, ApiNumericMode};
use storm_lua_build::{build, minify, ApiCompileOptions, ApiProjectCompileOptions};
use storm_lua_microcontroller::{Microcontroller, MicrocontrollerConfig};
use storm_lua_minify::{compile_code, CompileOptions};
use storm_lua_spec::{
    io::CompositeSignal,
    property::{PropertyBag, PropertyValue},
};
use storm_lua_vm::runner::RunOutcome;
use storm_screen_raster::raster::ScreenRaster;

fn project() -> LuaProject {
    LuaProject {
        entry: "main".into(),
        modules: BTreeMap::from([
            (
                "main".into(),
                r#"
local util = require("lib.util")
local ticks, draws, value = 0, 0, 0
local title = property.getText("Title")
function onTick()
 ticks=ticks+1
 value=util.scale(input.getNumber(1))+draws
 output.setNumber(1,value)
 output.setNumber(2,ticks)
 output.setNumber(3,draws)
 output.setBool(1,input.getBool(1) and property.getBool("Enabled"))
 if ticks==1 then output.setNumber(4,42) end
end
function onDraw()
 draws=draws+1
 screen.setColor(0,0,0)
 screen.drawClear()
 screen.setColor(40,80,120,128)
 screen.drawRectF(draws%8,2,screen.getWidth()-4,screen.getHeight()-6)
 screen.setColor(255,255,255)
 screen.drawText(1,1,title)
end
"#
                .into(),
            ),
            (
                "lib.util".into(),
                r#"
local gain=property.getNumber("Gain")
local M={}
function M.scale(x)return x*gain end
return M
"#
                .into(),
            ),
        ]),
        ambient: BTreeMap::new(),
    }
}

fn properties() -> PropertyBag {
    let mut bag = PropertyBag::default();
    bag.insert(b"Gain".to_vec(), PropertyValue::Number(2.0));
    bag.insert(b"Enabled".to_vec(), PropertyValue::Bool(true));
    bag.insert(b"Title".to_vec(), PropertyValue::Text(b"SDK".to_vec()));
    bag
}

fn assert_signals(a: CompositeSignal, b: CompositeSignal) {
    assert_eq!(a.booleans, b.booleans);
    for (a, b) in a.numbers.into_iter().zip(b.numbers) {
        assert_eq!(a.to_bits(), b.to_bits());
    }
}

#[test]
fn project_build_and_minify_preserve_real_vehicle_io_and_two_draws() -> Result<(), Box<dyn Error>> {
    let project = project();
    let diagnostics = analyze(&project, &AnalyzeOptions::default());
    assert!(diagnostics.ok);
    assert!(
        diagnostics.diagnostics.is_empty(),
        "{:?}",
        diagnostics.diagnostics
    );
    let options = ApiCompileOptions {
        numeric_mode: Some(ApiNumericMode::Exact),
        zero_cost_newlines: Some(false),
        ..Default::default()
    };
    let linked = build(
        &project,
        &ApiProjectCompileOptions {
            compile: options.clone(),
            minify: Some(false),
        },
    );
    let optimized = build(
        &project,
        &ApiProjectCompileOptions {
            compile: options,
            minify: Some(true),
        },
    );
    assert!(linked.ok, "{:?}", linked.diagnostics);
    assert!(optimized.ok, "{:?}", optimized.diagnostics);
    let linked_source = linked.code.ok_or("missing linked source")?;
    let generated = optimized.code.ok_or("missing minified source")?;
    assert!(generated.len() < linked_source.len());
    let map: serde_json::Value =
        serde_json::from_str(&linked.map.ok_or("missing non-minified source map")?)?;
    let sources = map["sources"].as_array().ok_or("source map sources")?;
    assert!(sources.iter().any(|s| s == "lib/util.lua"));
    assert!(optimized.map.is_none());
    let config = MicrocontrollerConfig {
        properties: properties(),
        ..Default::default()
    };
    let mut a = Microcontroller::new(config.clone())?;
    let mut b = Microcontroller::new(config)?;
    assert_eq!(
        a.load(linked_source.as_bytes(), "=linked")?,
        RunOutcome::Completed
    );
    assert_eq!(
        b.load(generated.as_bytes(), "=minified")?,
        RunOutcome::Completed
    );
    let mut frame_count = 0;
    for tick in 0..32 {
        let mut input = CompositeSignal::default();
        input.numbers[0] = [0.0_f32, -0.0, -2.5, 0.25, 16777216.0][tick % 5];
        input.booleans[0] = tick % 3 != 0;
        assert_eq!(a.tick(&input)?, RunOutcome::Completed);
        assert_eq!(b.tick(&input)?, RunOutcome::Completed);
        assert_signals(a.output(), b.output());
        assert_eq!(a.output().numbers[1], (tick + 1) as f32);
        assert_eq!(a.output().numbers[2], (tick * 2) as f32);
        assert_eq!(a.output().numbers[3], 42.0);
        for (width, height) in [(32, 32), (96, 64)] {
            assert_eq!(a.draw(width, height)?, RunOutcome::Completed);
            assert_eq!(b.draw(width, height)?, RunOutcome::Completed);
            assert!(!a.commands().is_empty());
            assert_eq!(&*a.commands(), &*b.commands());
            let mut raster_a = ScreenRaster::new(width, height)?;
            let mut raster_b = ScreenRaster::new(width, height)?;
            a.replay(&mut raster_a)?;
            b.replay(&mut raster_b)?;
            assert_eq!(raster_a.pixels(), raster_b.pixels());
            assert_signals(a.output(), b.output());
            frame_count += 1;
        }
    }
    assert_eq!(frame_count, 64);
    Ok(())
}

#[test]
fn compilation_does_not_execute_top_level_lua() {
    let source = "error('this must run only when a host loads the artifact')";
    let result = minify(
        source,
        &ApiCompileOptions {
            environment: storm_lua_spec::environment::EnvironmentProfile::Extended,
            ..Default::default()
        },
    );
    assert!(result.ok, "{:?}", result.diagnostics);
    let project = LuaProject {
        entry: "main".into(),
        modules: BTreeMap::from([("main".into(), source.into())]),
        ambient: BTreeMap::new(),
    };
    let result = build(
        &project,
        &ApiProjectCompileOptions {
            minify: Some(false),
            compile: ApiCompileOptions {
                environment: storm_lua_spec::environment::EnvironmentProfile::Extended,
                ..Default::default()
            },
        },
    );
    assert!(result.ok);
    assert!(result.code.is_some());
}

#[test]
fn malformed_public_finalization_input_is_a_diagnostic() -> Result<(), Box<dyn Error>> {
    let options = CompileOptions::default();
    let candidate = compile_code("function onTick()output.setNumber(1,3)end", &options)?;
    let result = finish_compile_result("local value = (", &options, Ok(candidate));
    assert!(!result.ok);
    assert!(result.code.is_none());
    assert_eq!(result.diagnostics[0].code, "syntax-error");
    Ok(())
}

#[test]
fn vehicle_target_is_explicit_and_addon_is_not_silently_compiled() -> Result<(), Box<dyn Error>> {
    let options: ApiCompileOptions = serde_json::from_str(r#"{"target":"vehicle"}"#)?;
    assert!(minify("function onTick()end", &options).ok);
    assert!(serde_json::from_str::<ApiCompileOptions>(r#"{"target":"addon"}"#).is_err());
    assert!(serde_json::from_str::<AnalyzeOptions>(r#"{"target":"addon"}"#).is_err());
    assert!(serde_json::from_str::<ApiProjectCompileOptions>(
        r#"{"target":"addon","minify":false}"#
    )
    .is_err());
    Ok(())
}

#[test]
fn retired_passes_are_rejected_by_the_public_engine_compiler() {
    for id in [
        "general-expression-factoring",
        "repeated-expression-factoring",
        "scalar-vector-loop-synthesis",
        "redundant-nil-fallback-elimination",
    ] {
        for enabled in [false, true] {
            let options = ApiCompileOptions {
                pass_toggles: BTreeMap::from([(id.into(), enabled)]),
                target_size: Some(8192),
                ..Default::default()
            };
            let result = minify("function onTick()end", &options);
            assert!(!result.ok);
            assert_eq!(result.diagnostics[0].code, "unknown-optimization-pass");
            assert!(result.diagnostics[0].message.contains(id));
        }
    }
}

#[test]
fn captured_property_snapshot_is_not_resampled_on_later_callbacks() -> Result<(), Box<dyn Error>> {
    let project = LuaProject {
        entry: "main".into(),
        modules: BTreeMap::from([
            (
                "main".into(),
                "local m=require('m') function onTick()output.setNumber(1,m.f())end".into(),
            ),
            (
                "m".into(),
                "local x=property.getNumber('Gain') return {f=function()return x end}".into(),
            ),
        ]),
        ambient: BTreeMap::new(),
    };
    let result = build(
        &project,
        &ApiProjectCompileOptions {
            compile: ApiCompileOptions {
                numeric_mode: Some(ApiNumericMode::Exact),
                ..Default::default()
            },
            minify: Some(true),
        },
    );
    let code = result.code.ok_or("missing artifact")?;
    let mut vm = Microcontroller::new(MicrocontrollerConfig {
        properties: properties(),
        ..Default::default()
    })?;
    vm.load(code.as_bytes(), "=property-snapshot")?;
    vm.tick(&CompositeSignal::default())?;
    assert_eq!(vm.output().numbers[0], 2.0);
    let mut changed = properties();
    changed.insert(b"Gain".to_vec(), PropertyValue::Number(9.0));
    vm.set_properties(changed)?;
    vm.tick(&CompositeSignal::default())?;
    assert_eq!(
        vm.output().numbers[0],
        2.0,
        "captured top-level property was resampled: {code}"
    );
    Ok(())
}

#[test]
fn captured_tick_input_is_not_reread_by_a_persistent_closure() -> Result<(), Box<dyn Error>> {
    let source="local read=nil function onTick()if read==nil then local x=input.getNumber(1) read=function()return x end end output.setNumber(1,read())end";
    let result = minify(
        source,
        &ApiCompileOptions {
            numeric_mode: Some(ApiNumericMode::Exact),
            ..Default::default()
        },
    );
    let code = result.code.ok_or("missing artifact")?;
    let mut vm = Microcontroller::new(MicrocontrollerConfig::default())?;
    vm.load(code.as_bytes(), "=input-snapshot")?;
    let mut input = CompositeSignal::default();
    input.numbers[0] = 4.0;
    vm.tick(&input)?;
    assert_eq!(vm.output().numbers[0], 4.0);
    input.numbers[0] = 12.0;
    vm.tick(&input)?;
    assert_eq!(
        vm.output().numbers[0],
        4.0,
        "captured input was resampled: {code}"
    );
    Ok(())
}

#[test]
fn target_checkpoints_and_full_search_preserve_real_io_and_draw_commands(
) -> Result<(), Box<dyn Error>> {
    let linked = build(
        &project(),
        &ApiProjectCompileOptions {
            minify: Some(false),
            ..Default::default()
        },
    );
    assert!(linked.ok);
    let source = linked.code.ok_or("missing source")?;
    let settings = ApiCompileOptions {
        numeric_mode: Some(ApiNumericMode::Exact),
        ..Default::default()
    };
    let full = minify(&source, &settings);
    assert!(full.ok);
    for target in [0, 400, 8192] {
        let result = minify(
            &source,
            &ApiCompileOptions {
                target_size: Some(target),
                ..settings.clone()
            },
        );
        assert!(result.ok, "{:?}", result.diagnostics);
        if target == 0 {
            assert_eq!(result.code, full.code);
        }
        let config = MicrocontrollerConfig {
            properties: properties(),
            ..Default::default()
        };
        let mut original = Microcontroller::new(config.clone())?;
        let mut optimized = Microcontroller::new(config)?;
        original.load(source.as_bytes(), "=original")?;
        optimized.load(
            result.code.ok_or("missing target source")?.as_bytes(),
            "=target",
        )?;
        for tick in 0..16 {
            let mut input = CompositeSignal::default();
            input.numbers[0] = [-2.5, 0.0, 3.0, 0.25][tick % 4];
            input.booleans[0] = tick % 2 == 0;
            assert_eq!(original.tick(&input)?, RunOutcome::Completed);
            assert_eq!(optimized.tick(&input)?, RunOutcome::Completed);
            assert_signals(original.output(), optimized.output());
            for (width, height) in [(32, 32), (96, 64)] {
                assert_eq!(original.draw(width, height)?, RunOutcome::Completed);
                assert_eq!(optimized.draw(width, height)?, RunOutcome::Completed);
                assert_eq!(&*original.commands(), &*optimized.commands());
                let mut a = ScreenRaster::new(width, height)?;
                let mut b = ScreenRaster::new(width, height)?;
                original.replay(&mut a)?;
                optimized.replay(&mut b)?;
                assert_eq!(a.pixels(), b.pixels());
            }
        }
    }
    Ok(())
}
