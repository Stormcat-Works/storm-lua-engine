//! アドオンプロファイルの分離、ホスト呼び出し、ライフサイクル、および永続データ。
#[path = "support/assertions.rs"]
mod assertion_support;
use std::{cell::RefCell, collections::BTreeMap, error::Error, rc::Rc};
use storm_lua_addon::{Addon, AddonConfig};
use storm_lua_microcontroller::Microcontroller;
use storm_lua_vm::{
    logging::LogSource,
    runner::{ErrorKind, RunOutcome, StepMode},
    value::{HostFunction, LuaValue},
};
fn field(value: &LuaValue, name: &str) -> Option<LuaValue> {
    if let LuaValue::Table(entries) = value {
        entries
            .iter()
            .find(|(k, _)| *k == LuaValue::text(name))
            .map(|(_, v)| v.clone())
    } else {
        None
    }
}
#[test]
fn profiles_do_not_mix_namespaces_or_callback_contracts() -> Result<(), Box<dyn Error>> {
    let mut addon = Addon::new(Default::default())?;
    assert!(addon.tick(1).is_err());
    addon.load(
        &assertion_support::with_assertions(
            br#"assert(input==nil and output==nil and screen==nil and async==nil)
assert(property.getNumber==nil and type(property.slider)=='function')
assert(debug.getregistry==nil and type(debug.log)=='function' and print==nil)
assert(io==nil and os==nil and require==nil)
g_savedata={ticks=0}
function onCreate(new) assert(new) end
function onTick(dt) g_savedata.ticks=g_savedata.ticks+dt end"#,
        ),
        "=addon",
    )?;
    assert!(addon.tick(1).is_err());
    addon.start()?;
    assert!(addon.start().is_err());
    addon.tick(400)?;
    assert_eq!(
        field(&addon.savedata()?, "ticks"),
        Some(LuaValue::Integer(400))
    );
    assert!(addon.dispatch("onTick", &[]).is_err());
    assert!(addon.dispatch("onDraw", &[]).is_err());
    addon.destroy()?;
    assert!(addon.tick(1).is_err());
    assert!(addon.destroy().is_err());
    let mut vehicle = Microcontroller::new(Default::default())?;
    vehicle.load(&assertion_support::with_assertions(b"assert(server==nil and matrix==nil and g_savedata==nil and property.checkbox==nil); assert(type(async.httpGet)=='function')"), "=vehicle")?;
    Ok(())
}
#[test]
fn top_level_precedes_restore_and_on_create() -> Result<(), Box<dyn Error>> {
    let snapshot = LuaValue::record([("count", LuaValue::Integer(41))]);
    let mut addon = Addon::new(AddonConfig {
        is_world_create: false,
        savedata: Some(snapshot),
        ..Default::default()
    })?;
    addon.load(&assertion_support::with_assertions(br#"assert(g_savedata.count==nil)
g_savedata={count=999, default=property.checkbox('Enabled',true)}
assert(g_savedata.default==nil)
function onCreate(new) assert(not new); assert(g_savedata.count==41);g_savedata.count=g_savedata.count+1 end"#), "=restore")?;
    assert_eq!(
        field(&addon.savedata()?, "count"),
        Some(LuaValue::Integer(41))
    );
    addon.start()?;
    assert_eq!(
        field(&addon.savedata()?, "count"),
        Some(LuaValue::Integer(42))
    );
    Ok(())
}
#[test]
fn synchronous_queries_preserve_tables_i64_bytes_and_trailing_nil() -> Result<(), Box<dyn Error>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let capture = Rc::clone(&seen);
    let handler: HostFunction = Rc::new(move |args| {
        *capture.borrow_mut() = args.to_vec();
        Ok(vec![
            LuaValue::record([
                ("id", LuaValue::Integer(i64::MAX)),
                ("name", LuaValue::Bytes(vec![0, 255])),
            ]),
            LuaValue::Bool(true),
            LuaValue::Nil,
        ])
    });
    let mut addon = Addon::new(AddonConfig {
        server: BTreeMap::from([("query".into(), handler)]),
        ..Default::default()
    })?;
    addon.load(&assertion_support::with_assertions(br#"function onCreate()
local r=table.pack(server.query(9223372036854775807, string.char(0,255), nil))
assert(r.n==3 and r[2] and r[3]==nil and r[1].id==9223372036854775807 and r[1].name==string.char(0,255))
end"#), "=query")?;
    addon.start()?;
    assert_eq!(
        *seen.borrow(),
        vec![
            LuaValue::Integer(i64::MAX),
            LuaValue::Bytes(vec![0, 255]),
            LuaValue::Nil
        ]
    );
    Ok(())
}
#[test]
fn missing_services_and_bad_host_data_are_not_success_stubs() -> Result<(), Box<dyn Error>> {
    let mut addon = Addon::new(Default::default())?;
    addon.load(
        &assertion_support::with_assertions(b"function onCreate() server.getPlayers() end"),
        "=missing",
    )?;
    assert_eq!(
        addon.start().err().ok_or("missing error")?.kind,
        ErrorKind::Lua
    );
    assert!(Addon::new(AddonConfig {
        is_world_create: false,
        savedata: Some(LuaValue::Table(vec![
            (LuaValue::Integer(1), LuaValue::Bool(true)),
            (LuaValue::Number(1.0), LuaValue::Bool(false))
        ])),
        ..Default::default()
    })
    .is_err());
    let mut addon = Addon::new(Default::default())?;
    addon.load(
        &assertion_support::with_assertions(b"g_savedata.self=g_savedata"),
        "=cycle",
    )?;
    assert!(addon.savedata().is_err());
    Ok(())
}
#[test]
fn menu_properties_logs_and_reload_checkpoint() -> Result<(), Box<dyn Error>> {
    let mut config = AddonConfig::default();
    config.properties.insert(
        b"Rate".to_vec(),
        storm_lua_spec::property::PropertyValue::Number(2.5),
    );
    config.dev_logs = true;
    config.environment = storm_lua_spec::environment::EnvironmentProfile::Extended;
    let mut addon = Addon::new(config)?;
    addon.load(&assertion_support::with_assertions(br#"g_savedata={rate=property.slider('Rate',0,10,0.5,1),enabled=property.checkbox('Enabled',true)}
print('loaded',string.char(255))
function onCreate(new) debug.log('started'); assert(g_savedata.rate==2.5 and g_savedata.enabled) end"#), "=menu")?;
    addon.start()?;
    assert_eq!(addon.property_definitions().len(), 2);
    let logs = addon.drain_log_records();
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0].source, LogSource::Print);
    assert_eq!(logs[0].bytes, b"loaded\t\xff");
    assert_eq!(logs[1].source, LogSource::Debug);
    let saved = addon.savedata()?;
    addon.reload(saved)?;
    addon.start()?;
    assert_eq!(
        field(&addon.savedata()?, "rate"),
        Some(LuaValue::Number(2.5))
    );
    Ok(())
}
#[test]
fn matrix_composition_inverse_and_vector_operations() -> Result<(), Box<dyn Error>> {
    let mut addon = Addon::new(AddonConfig {
        environment: storm_lua_spec::environment::EnvironmentProfile::Extended,
        ..Default::default()
    })?;
    addon.load(&assertion_support::with_assertions(br#"local a=matrix.translation(10,20,30)
local b=matrix.translation(1,2,3)
local x,y,z=matrix.position(matrix.multiply(a,b));assert(x==11 and y==22 and z==33)
local x,y,z,w=matrix.multiplyXYZW(a,2,3,4,1);assert(x==12 and y==23 and z==34 and w==1)
local p=matrix.multiply(a,matrix.rotationY(0.5));local i=matrix.multiply(p,matrix.invert(p));local expected=matrix.identity()
for n=1,16 do assert(math.abs(i[n]-expected[n])<1e-12) end
local tt=matrix.transpose(matrix.transpose(a));for n=1,16 do assert(tt[n]==a[n]) end
assert(matrix.distance(matrix.translation(0,0,0),matrix.translation(3,4,0))==5)
local singular={};for n=1,16 do singular[n]=0 end;assert(not pcall(matrix.invert,singular))"#), "=matrices")?;
    Ok(())
}
#[test]
fn paused_initialization_restores_savedata_only_after_top_level_completes(
) -> Result<(), Box<dyn Error>> {
    let mut addon = Addon::new(AddonConfig {
        is_world_create: false,
        savedata: Some(LuaValue::record([("n", LuaValue::Integer(7))])),
        ..Default::default()
    })?;
    addon.set_breakpoints(vec![("=paused".into(), 2)])?;
    assert_eq!(
        addon.load(
            &assertion_support::with_assertions(
                b"g_savedata.n=1
g_savedata.n=2
function onCreate() assert(g_savedata.n==7) end"
            ),
            "=paused"
        )?,
        RunOutcome::Suspended
    );
    assert!(addon.start().is_err());
    assert!(addon.savedata().is_err());
    addon.set_breakpoints(vec![])?;
    assert_eq!(addon.resume(StepMode::Continue)?, RunOutcome::Completed);
    assert_eq!(field(&addon.savedata()?, "n"), Some(LuaValue::Integer(7)));
    addon.start()?;
    Ok(())
}
#[test]
fn matrix_multiply_order_matches_game_binary() -> Result<(), Box<dyn Error>> {
    let mut addon = Addon::new(AddonConfig {
        environment: storm_lua_spec::environment::EnvironmentProfile::Extended,
        ..Default::default()
    })?;
    addon.load(&assertion_support::with_assertions(r##"-- 実ゲーム matrix_multiply の静的解析から導いた合成式（out[r][i] = Σ b[r][k]・a[k][i]）
local function game_mul(a, b)
  local out = {}
  for r = 1, 4 do
    for i = 1, 4 do
      local s = 0
      for k = 1, 4 do s = s + b[(r-1)*4+k] * a[(k-1)*4+i] end
      out[(r-1)*4+i] = s
    end
  end
  return out
end
local function approx(a, b) for n=1,16 do assert(math.abs(a[n]-b[n])<1e-9) end end
local a = matrix.multiply(matrix.translation(3,-2,5), matrix.rotationZ(0.7))
local b = matrix.multiply(matrix.rotationX(-0.3), matrix.translation(-1,4,2))
approx(matrix.multiply(a, b), game_mul(a, b))
approx(matrix.multiply(b, a), game_mul(b, a))
-- 平行移動と回転の合成順序は位置で観測できる
local rz = matrix.rotationZ(math.pi/2)
local tr = matrix.translation(1,2,3)
local x,y,z = matrix.position(matrix.multiply(rz, tr))
assert(math.abs(x+2)<1e-9 and math.abs(y-1)<1e-9 and math.abs(z-3)<1e-9)
local x,y,z = matrix.position(matrix.multiply(tr, rz))
assert(math.abs(x-1)<1e-9 and math.abs(y-2)<1e-9 and math.abs(z-3)<1e-9)"##.as_bytes()), "=matrix-order")?;
    Ok(())
}
#[test]
fn matrix_read_validation_matches_game() -> Result<(), Box<dyn Error>> {
    let mut addon = Addon::new(AddonConfig {
        environment: storm_lua_spec::environment::EnvironmentProfile::Extended,
        ..Default::default()
    })?;
    addon.load(
        &assertion_support::with_assertions(
            r##"local function expect_err(substr, f, ...)
  local ok, err = pcall(f, ...)
  assert(not ok, "expected error containing: "..substr)
  assert(string.find(tostring(err), substr, 1, true), tostring(err))
end
local short = {}; for n=1,15 do short[n]=0 end
expect_err("bad matrix size", matrix.position, short)
local long = {}; for n=1,17 do long[n]=0 end
expect_err("matrix index out of range: 17", matrix.position, long)
local dup = matrix.identity(); dup["3"] = 0
expect_err("bad matrix size", matrix.position, dup)
expect_err("matrix was nil", matrix.position, nil)
expect_err("bad matrix input type: number", matrix.position, 42)
expect_err("bad matrix input type: string", matrix.position, "x")
local bad_elem = matrix.identity(); bad_elem[3] = "x"
expect_err("bad matrix input type: string", matrix.position, bad_elem)
local bad_elem2 = matrix.identity(); bad_elem2[3] = true
expect_err("bad matrix input type: boolean", matrix.position, bad_elem2)
local str_elem = matrix.identity(); str_elem[13] = "7"
assert(matrix.position(str_elem) == 7)
local oor = matrix.identity(); oor[17] = 0
expect_err("matrix index out of range: 17", matrix.position, oor)
local str_key = matrix.identity(); str_key.foo = 0
expect_err("bad value in matrix: foo", matrix.position, str_key)
-- 非有限の要素はエラーにならず、そのまま計算へ流れる
local inf_elem = matrix.identity(); inf_elem[1] = math.huge
assert(matrix.position(inf_elem) == 0)
assert(matrix.multiply(inf_elem, matrix.identity())[1] == math.huge)"##
                .as_bytes(),
        ),
        "=matrix-validation",
    )?;
    Ok(())
}
#[test]
fn matrix_rotation_to_face_xz_matches_game() -> Result<(), Box<dyn Error>> {
    let mut addon = Addon::new(AddonConfig {
        environment: storm_lua_spec::environment::EnvironmentProfile::Extended,
        ..Default::default()
    })?;
    addon.load(
        &assertion_support::with_assertions(
            r##"-- scale は Component Mod 専用で addon には存在しない
assert(matrix.scale == nil)
-- 実ゲームは f32 経由で角度を求めるため、比較は粗めの許容で行う
local function approx(a, b) for n=1,16 do assert(math.abs(a[n]-b[n])<1e-5) end end
approx(matrix.rotationToFaceXZ(1, 0), matrix.rotationY(math.pi/2))
approx(matrix.rotationToFaceXZ(0, 1), matrix.identity())
approx(matrix.rotationToFaceXZ(0, 0), matrix.rotationY(math.pi/2))
approx(matrix.rotationToFaceXZ(-1, 0), matrix.rotationY(-math.pi/2))
approx(matrix.rotationToFaceXZ(3, 4), matrix.rotationY(math.atan(3, 4)))
-- +Z 軸が指定方向 (x, z) へ写る
local m = matrix.rotationToFaceXZ(0.6, 0.8)
local x, y, z, w = matrix.multiplyXYZW(m, 0, 0, 1, 0)
assert(math.abs(x-0.6)<1e-5 and math.abs(z-0.8)<1e-5)"##
                .as_bytes(),
        ),
        "=matrix-facexz",
    )?;
    Ok(())
}
#[test]
fn matrix_arity_scalar_conversion_and_f32_vector() -> Result<(), Box<dyn Error>> {
    let mut addon = Addon::new(AddonConfig {
        environment: storm_lua_spec::environment::EnvironmentProfile::Extended,
        ..Default::default()
    })?;
    addon.load(
        &assertion_support::with_assertions(
            r##"local function expect_err(substr, f, ...)
  local ok, err = pcall(f, ...)
  assert(not ok, "expected error containing: "..substr)
  assert(string.find(tostring(err), substr, 1, true), tostring(err))
end
expect_err("Wrong number of args", matrix.multiply, matrix.identity())
expect_err("Wrong number of args", matrix.translation, 1, 2)
expect_err("Wrong number of args", matrix.rotationToFaceXZ, 1)
-- identity は引数の個数を検査しない（実ゲームに gettop 検査がない）
assert(matrix.identity(1, 2, 3)[1] == 1)
-- スカラーは lua_tonumberx と同じ変換（数値文字列は変換、変換不能は 0）
local x, y, z = matrix.position(matrix.translation("3", "4", "5"))
assert(x==3 and y==4 and z==5)
local x, y, z = matrix.position(matrix.translation(true, nil, "x"))
assert(x==0 and y==0 and z==0)
-- 回転角が非有限でもエラーにならず NaN 要素を返す
local r = matrix.rotationX(math.huge); assert(r[6] ~= r[6])
-- multiplyXYZW のベクトル成分は f32 へ丸められる（実ゲームの確定挙動）
local x = matrix.multiplyXYZW(matrix.identity(), 1e300, 0, 0, 0)
assert(x == math.huge)
local x = matrix.multiplyXYZW(matrix.identity(), 0.1, 0, 0, 0)
assert(x ~= 0.1 and math.abs(x - 0.1) < 1e-7)"##
                .as_bytes(),
        ),
        "=matrix-args",
    )?;
    Ok(())
}
