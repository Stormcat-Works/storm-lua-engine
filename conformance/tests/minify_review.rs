//! Public compile boundary regressions, including detailed source attribution.
use std::error::Error;
use storm_lua_vm::backend::Lua;

#[test]
fn reviewed_api_regressions_preserve_observations_and_source_maps() -> Result<(), Box<dyn Error>> {
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("fixtures/minify-review-api.json"))?;
    for case in cases {
        let source = case["source"].as_str().ok_or("source")?;
        let setup = case["setup"].as_str().unwrap_or("");
        let after = case["after"].as_str().unwrap_or("");
        let original: String = Lua::new()
            .load(format!("{setup}\n{source}\n{after}"))
            .eval()?;
        for source_map in [false, true] {
            let mut options = case["options"].clone();
            options["sourceMap"] = source_map.into();
            let options = serde_json::from_value(options)?;
            let result = storm_lua_build::public_api::compile(source, &options);
            assert!(result.ok, "{}: {:?}", case["name"], result.error);
            let code = result.code.ok_or("missing compiled code")?;
            let output: String = Lua::new()
                .load(format!("{setup}\n{code}\n{after}"))
                .eval()?;
            assert_eq!(original, output, "{}: {code}", case["name"]);
            if source_map {
                let map = result.map.ok_or("missing requested map")?;
                storm_lua_build::optimized_source_map::validate(&code, &map)?;
            }
        }
    }
    Ok(())
}
