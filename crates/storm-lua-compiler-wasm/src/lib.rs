//! Compiler-only wasm-bindgen boundary; no Lua VM is loaded or initialized.
use js_sys::{Array, Object, Reflect, Uint8Array};
use serde::Serialize;
use storm_lua_analysis::analyze as analyze_core;
use storm_lua_analysis::analyze::AnalyzeOptions;
use storm_lua_analysis::project::LuaProject;
use storm_lua_analysis::property_scan::scan_properties as scan_properties_core;
use storm_lua_build::public_api::compile as compile_core;
use storm_lua_build::public_api::compile_project as compile_project_core;
use storm_lua_build::public_api::finish_compile_result;
use storm_lua_build::public_api::pass_metadata as pass_metadata_core;
use storm_lua_build::public_api::ApiCompileOptions;
use storm_lua_build::public_api::ApiCompileResult;
use storm_lua_build::public_api::ApiProjectCompileOptions;
use storm_lua_minify::search::{
    decode_and_evaluate_candidate_job, decode_search_context, encode_candidate_job,
    encode_search_context, finalize_satisficing_fallback, prepare_search, select_best,
    take_satisficing_fallback, try_satisficing as try_satisficing_core, CandidateBatch,
    CandidateJob, SatisficingStatus, SearchContext,
};
use wasm_bindgen::prelude::*;

/// wasm境界の全出力で共通に使うシリアライザ。`json_compatible()` は
/// `HashMap`/`BTreeMap` をJS `Map` ではなくプレーンオブジェクトへ、
/// `None` を `undefined` ではなく `null` へ変換する（`.d.ts` の宣言
/// — `Record<string, T>` / `T | null` — と一致させるため。serde-json
/// 相当の変換なので `JSON.stringify` でも欠落しない）。
fn to_js<T: Serialize + ?Sized>(value: &T) -> Result<JsValue, serde_wasm_bindgen::Error> {
    value.serialize(&serde_wasm_bindgen::Serializer::json_compatible())
}

fn decode_options(options: JsValue) -> Result<ApiCompileOptions, JsValue> {
    if options.is_null() || options.is_undefined() {
        Ok(ApiCompileOptions::default())
    } else {
        serde_wasm_bindgen::from_value(options)
            .map_err(|error| JsValue::from_str(&format!("invalid compile options: {error}")))
    }
}

fn decode_project(project: JsValue) -> Result<LuaProject, JsValue> {
    serde_wasm_bindgen::from_value(project)
        .map_err(|error| JsValue::from_str(&format!("invalid project: {error}")))
}

fn decode_project_compile_options(options: JsValue) -> Result<ApiProjectCompileOptions, JsValue> {
    if options.is_null() || options.is_undefined() {
        Ok(ApiProjectCompileOptions::default())
    } else {
        serde_wasm_bindgen::from_value(options)
            .map_err(|error| JsValue::from_str(&format!("invalid compileProject options: {error}")))
    }
}

fn decode_analyze_options(options: JsValue) -> Result<AnalyzeOptions, JsValue> {
    if options.is_null() || options.is_undefined() {
        Ok(AnalyzeOptions::default())
    } else {
        serde_wasm_bindgen::from_value(options)
            .map_err(|error| JsValue::from_str(&format!("invalid analyze options: {error}")))
    }
}

/// `compileProject()`(設計 §6/§8)。project/options は JSON 互換(camelCase)。
#[wasm_bindgen(js_name = compileProject)]
pub fn compile_project(project: JsValue, options: JsValue) -> Result<JsValue, JsValue> {
    let project = decode_project(project)?;
    let options = decode_project_compile_options(options)?;
    to_js(&compile_project_core(&project, &options)).map_err(|error| {
        JsValue::from_str(&format!("failed to encode compileProject result: {error}"))
    })
}

/// `analyze()`(設計 §5/§8)。project/options は JSON 互換(camelCase)。
#[wasm_bindgen]
pub fn analyze(project: JsValue, options: JsValue) -> Result<JsValue, JsValue> {
    let project = decode_project(project)?;
    let options = decode_analyze_options(options)?;
    to_js(&analyze_core(&project, &options))
        .map_err(|error| JsValue::from_str(&format!("failed to encode analyze result: {error}")))
}

fn bytes(value: Vec<u8>) -> Uint8Array {
    Uint8Array::from(value.as_slice())
}

/// Coarse-grained public WASM boundary: one source/options input, one result output.
/// Candidate verification callbacks intentionally stay native-only.
#[wasm_bindgen]
pub fn compile(source: &str, options: JsValue) -> Result<JsValue, JsValue> {
    let options = decode_options(options)?;
    to_js(&compile_core(source, &options))
        .map_err(|error| JsValue::from_str(&format!("failed to encode compile result: {error}")))
}

/// Build the expensive common core once and serialize each independent candidate job.
/// The returned bytes are structured-cloneable Uint8Arrays for dedicated Web Workers.
#[wasm_bindgen(js_name = prepareJobs)]
pub fn prepare_jobs(source: &str, options: JsValue) -> Result<JsValue, JsValue> {
    let options = decode_options(options)?;
    let core_options = options
        .to_core()
        .map_err(|error| JsValue::from_str(&error))?;
    let (context, jobs) =
        prepare_search(source, &core_options).map_err(|error| JsValue::from_str(&error))?;
    encode_prepared_jobs(&context, jobs)
}

fn encode_prepared_jobs(
    context: &SearchContext,
    jobs: Vec<CandidateJob>,
) -> Result<JsValue, JsValue> {
    let object = Object::new();
    let context = encode_search_context(context).map_err(|error| JsValue::from_str(&error))?;
    Reflect::set(&object, &JsValue::from_str("context"), &bytes(context))?;
    let encoded_jobs = Array::new();
    for job in jobs {
        let encoded = encode_candidate_job(&job).map_err(|error| JsValue::from_str(&error))?;
        encoded_jobs.push(&bytes(encoded));
    }
    Reflect::set(&object, &JsValue::from_str("jobs"), &encoded_jobs)?;
    Ok(object.into())
}

/// Run target checkpoints and a canonical candidate prefix in the coordinator.
/// Remaining jobs can run in independent Workers; the encoded context retains
/// completed canonical batches so no candidate evaluation is repeated.
#[wasm_bindgen(js_name = trySatisficing)]
pub fn try_satisficing(source: &str, options: JsValue) -> Result<JsValue, JsValue> {
    let options = decode_options(options)?;
    let core_options = options
        .to_core()
        .map_err(|error| JsValue::from_str(&error))?;
    if core_options.target_size.is_none() {
        return Err(JsValue::from_str("trySatisficing requires targetSize"));
    }
    let mut attempt = try_satisficing_core(source, &core_options, None)
        .map_err(|error| JsValue::from_str(&error))?;
    let object = Object::new();
    let done = matches!(
        attempt.status,
        SatisficingStatus::Satisfied | SatisficingStatus::Stopped
    );
    Reflect::set(
        &object,
        &JsValue::from_str("done"),
        &JsValue::from_bool(done),
    )?;
    Reflect::set(
        &object,
        &JsValue::from_str("checkpoints"),
        &JsValue::from_f64(attempt.result.stats.checkpoints as f64),
    )?;
    if done {
        let result = finish_compile_result(source, &core_options, Ok(attempt.result));
        Reflect::set(
            &object,
            &JsValue::from_str("result"),
            &to_js(&result).map_err(|error| {
                JsValue::from_str(&format!("failed to encode satisficing result: {error}"))
            })?,
        )?;
    } else {
        // Resume the same canonical search: export remaining jobs and the
        // context containing completed batches. Never call prepareJobs again.
        let (context, jobs) = take_satisficing_fallback(&mut attempt)
            .ok_or_else(|| JsValue::from_str("satisficing fallback preparation missing"))?;
        Reflect::set(
            &object,
            &JsValue::from_str("prepared"),
            &encode_prepared_jobs(&context, jobs)?,
        )?;
    }
    Ok(object.into())
}

/// Evaluate one serialized candidate job without selecting the overall result.
#[wasm_bindgen(js_name = evaluateJob)]
pub fn evaluate_job(job: &[u8]) -> Result<JsValue, JsValue> {
    let batch =
        decode_and_evaluate_candidate_job(job).map_err(|error| JsValue::from_str(&error))?;
    to_js(&batch)
        .map_err(|error| JsValue::from_str(&format!("failed to encode candidate result: {error}")))
}

/// Merge worker results using the same Rust size/order tie-break and package public metadata.
#[wasm_bindgen(js_name = finishSearch)]
pub fn finish_search(
    source: &str,
    options: JsValue,
    context: &[u8],
    batches: JsValue,
) -> Result<JsValue, JsValue> {
    let options = decode_options(options)?;
    let core_options = options
        .to_core()
        .map_err(|error| JsValue::from_str(&error))?;
    let context = decode_search_context(context).map_err(|error| JsValue::from_str(&error))?;
    let batches: Vec<CandidateBatch> = serde_wasm_bindgen::from_value(batches)
        .map_err(|error| JsValue::from_str(&format!("invalid candidate batches: {error}")))?;
    let result: ApiCompileResult =
        finish_compile_result(source, &core_options, select_best(context, batches));
    to_js(&result)
        .map_err(|error| JsValue::from_str(&format!("failed to encode compile result: {error}")))
}

/// Finish a full parallel fallback while preserving OBJ-2 target metadata.
#[wasm_bindgen(js_name = finishTargetSearch)]
pub fn finish_target_search(
    source: &str,
    options: JsValue,
    context: &[u8],
    batches: JsValue,
    prior_checkpoints: usize,
) -> Result<JsValue, JsValue> {
    let options = decode_options(options)?;
    let core_options = options
        .to_core()
        .map_err(|error| JsValue::from_str(&error))?;
    let target_size = core_options
        .target_size
        .ok_or_else(|| JsValue::from_str("finishTargetSearch requires targetSize"))?;
    let context = decode_search_context(context).map_err(|error| JsValue::from_str(&error))?;
    let batches: Vec<CandidateBatch> = serde_wasm_bindgen::from_value(batches)
        .map_err(|error| JsValue::from_str(&format!("invalid candidate batches: {error}")))?;
    let selected = select_best(context, batches)
        .map(|result| finalize_satisficing_fallback(result, target_size, prior_checkpoints));
    let result: ApiCompileResult = finish_compile_result(source, &core_options, selected);
    to_js(&result)
        .map_err(|error| JsValue::from_str(&format!("failed to encode compile result: {error}")))
}

/// AST ベースの property key 検出。web UI の regex fallback を置き換える。
/// parse に失敗した場合は `ok: false` を返し、呼び出し側の regex fallback に委ねる。
#[wasm_bindgen(js_name = scanProperties)]
pub fn scan_properties(source: &str) -> Result<JsValue, JsValue> {
    to_js(&scan_properties_core(source))
        .map_err(|error| JsValue::from_str(&format!("failed to encode property scan: {error}")))
}

/// パス id ⇔ `PassRecord.name` 対応表。Web UI が passes[] のバッジ表示に使う。
#[wasm_bindgen(js_name = passMetadata)]
pub fn pass_metadata() -> Result<JsValue, JsValue> {
    to_js(&pass_metadata_core())
        .map_err(|error| JsValue::from_str(&format!("failed to encode pass metadata: {error}")))
}

/// Crate version of the compiler adapter.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// All supported optimization IDs, independent of optional display-name records.
#[wasm_bindgen(js_name = passIds)]
pub fn pass_ids() -> Result<JsValue, JsValue> {
    to_js(storm_lua_minify::pass_ids::OPTIMIZATION_PASS_IDS)
        .map_err(|error| JsValue::from_str(&format!("failed to encode pass identifiers: {error}")))
}

/// Source inspection for bounded IDE declaration editing, without running Lua.
#[wasm_bindgen(js_name = inspectSource)]
pub fn inspect_source(source: &str) -> Result<JsValue, JsValue> {
    to_js(&storm_lua_syntax::source_tools::inspect_source(source))
        .map_err(|error| JsValue::from_str(&format!("failed to encode source inspection: {error}")))
}

/// LB include-once build, independent of default static module semantics.
#[wasm_bindgen(js_name = buildLifeboat)]
pub fn build_lifeboat(project: JsValue, options: JsValue) -> Result<JsValue, JsValue> {
    let project = decode_project(project)?;
    let options = decode_project_compile_options(options)?;
    to_js(&storm_lua_build::public_api::compile_lifeboat(
        &project, &options,
    ))
    .map_err(|e| JsValue::from_str(&e.to_string()))
}
/// Remove real development directives while retaining byte and line positions.
#[wasm_bindgen(js_name = stripDevelopment)]
pub fn strip_development(source: &str) -> Result<String, JsValue> {
    storm_lua_build::lifeboat::strip_development(source).map_err(|e| JsValue::from_str(&e))
}

/// Validate code/map/snapshot identity and return the typed Storm optimization extension.
/// This neither executes Lua nor trusts an unverified map merely because it parses.
#[wasm_bindgen(js_name = validateSourceMap)]
pub fn validate_source_map(code: &str, map: &str) -> Result<JsValue, JsValue> {
    let details = storm_lua_build::optimized_source_map::validate(code, map)
        .map_err(|e| JsValue::from_str(&e))?;
    to_js(&details)
        .map_err(|e| JsValue::from_str(&format!("failed to encode source map details: {e}")))
}
