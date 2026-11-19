//! ベースラインの安全なLua環境およびVMごとの乱数状態。
use mlua::{Lua, Table, Value, Variadic};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};

/// `math.random` が使うストリームの所有者ハンドル。状態はVMヒープの外にあるため
/// スナップショットには含まれず、アイドル境界で明示的に往復させます。
pub(crate) enum MathRandom {
    /// VMローカルの決定論ストリーム（Extended既定、または `use_deterministic_random`）。
    /// 値は次回生成前のベース状態です。
    Local(Arc<AtomicU64>),
    /// Gameプロファイル既定の組み込みMSVC CRT互換ストリーム（J2）。
    BuiltinCrt(Arc<crate::random::MsvcCrtRandom>),
    /// ホスト共有CRTストリームへ差し替え済み。状態はホストが所有します。
    HostShared,
}

pub(crate) fn environment(
    lua: &Lua,
    profile: storm_lua_spec::environment::EnvironmentProfile,
) -> mlua::Result<(Table, MathRandom)> {
    let globals = lua.globals();
    let env = lua.create_table()?;
    for name in storm_lua_spec::environment::GAME_FUNCTIONS
        .iter()
        .copied()
        .chain(
            storm_lua_spec::environment::EXTENDED_FUNCTIONS
                .iter()
                .copied()
                .filter(|_| profile == storm_lua_spec::environment::EnvironmentProfile::Extended)
                .filter(|name| !matches!(*name, "print" | "unpack")),
        )
    {
        env.raw_set(name, globals.raw_get::<Value>(name)?)?;
    }
    for name in ["string", "table", "math"] {
        env.raw_set(name, globals.raw_get::<Table>(name)?)?;
    }
    // string ライブラリは実ゲームと同じ完全版です（string.dump を含む）。
    // チャンクの読み込みはテキスト専用のため、ダンプしたバイトコードを
    // 実行に戻す経路はありません（E14）。
    if profile == storm_lua_spec::environment::EnvironmentProfile::Extended {
        env.raw_set(
            "unpack",
            globals.get::<Table>("table")?.raw_get::<Value>("unpack")?,
        )?;
    }
    let math = env.get::<Table>("math")?;
    let stream = if profile == storm_lua_spec::environment::EnvironmentProfile::Game {
        // Gameプロファイルの既定は、実ゲームと同じCRT互換の共有ストリームです（J2）。
        // VMごとの決定的ストリームはホストが明示的に選択した場合だけ使います。
        let crt = crate::random::shared_builtin_crt_arc();
        crate::random::install(
            lua,
            &math,
            Arc::clone(&crt) as crate::random::SharedCrtRandom,
        )?;
        MathRandom::BuiltinCrt(crt)
    } else {
        let local = Arc::new(AtomicU64::new(1_u64));
        install_random(lua, &math, Arc::clone(&local))?;
        MathRandom::Local(local)
    };
    Ok((env, stream))
}
/// `math.random` の区間引数を PUC Lua 5.3 の `luaL_checkinteger` と同じ規則で検証します。
/// mlua の `i64` 変換は小数部を切り捨ててしまうため、ここでは `lua_tointegerx` 相当の
/// `coerce_integer`（整数表現を正確に持つ値だけを受理し、数値文字列も強制する）を使います。
/// 整数表現を持たない数は PUC と同じ文面で、数でない値は型名つきの PUC 文面で拒否します。
fn random_interval_arg(lua: &Lua, value: &Value, index: usize) -> mlua::Result<i64> {
    if let Some(integer) = lua.coerce_integer(value.clone())? {
        return Ok(integer);
    }
    if lua.coerce_number(value.clone())?.is_some() {
        Err(mlua::Error::RuntimeError(format!(
            "bad argument #{index} to 'random' (number has no integer representation)"
        )))
    } else {
        let type_name = match value {
            Value::LightUserData(_) => "userdata",
            other => other.type_name(),
        };
        Err(mlua::Error::RuntimeError(format!(
            "bad argument #{index} to 'random' (number expected, got {type_name})"
        )))
    }
}

pub(crate) fn install_random(lua: &Lua, math: &Table, seed: Arc<AtomicU64>) -> mlua::Result<()> {
    // VMごとの決定的ストリームです。Gameプロファイルの既定は実ゲームと同じ
    // CRT互換の共有ストリームであり、こちらはホストが明示的に選択した場合
    // （およびExtendedプロファイル）だけ使われます（J2）。
    let state = Arc::clone(&seed);
    math.raw_set(
        "randomseed",
        lua.create_function(move |_, value: i64| {
            state.store(value as u64, Ordering::Relaxed);
            Ok(())
        })?,
    )?;
    math.raw_set(
        "random",
        lua.create_function(move |lua, args: Variadic<Value>| {
            let next = || {
                let z = seed
                    .fetch_add(0x9e3779b97f4a7c15, Ordering::Relaxed)
                    .wrapping_add(0x9e3779b97f4a7c15);
                let z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                let z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
                z ^ (z >> 31)
            };
            match args.len() {
                0 => Ok(Value::Number(
                    (next() >> 11) as f64 * (1.0 / (1_u64 << 53) as f64),
                )),
                1 | 2 => {
                    // PUC と同じく第1引数から順に検証します（範囲外や小数部で
                    // 切り捨てて受理することはしません）。
                    let low = if args.len() == 1 {
                        1
                    } else {
                        random_interval_arg(lua, &args[0], 1)?
                    };
                    let high = random_interval_arg(lua, &args[args.len() - 1], args.len())?;
                    let span = i128::from(high) - i128::from(low) + 1;
                    if span <= 0 || span > i128::from(u64::MAX) {
                        return Err(mlua::Error::RuntimeError("invalid random interval".into()));
                    }
                    let span = span as u64;
                    let threshold = span.wrapping_neg() % span;
                    let mut value = next();
                    while value < threshold {
                        value = next();
                    }
                    Ok(Value::Integer(
                        (i128::from(low) + i128::from(value % span)) as i64,
                    ))
                }
                _ => Err(mlua::Error::RuntimeError(
                    "random expects zero, one or two integers".into(),
                )),
            }
        })?,
    )?;
    Ok(())
}

/// 保護された呼び出し（pcall/xpcall）の後にバジェット枯渇エラーを再送出します。これにより、
/// yield不可能なCコールバック内の入れ子pcallループがフックのバジェット例外を繰り返し捕捉するのを防ぎます。
pub(crate) fn protect_calls(
    lua: &Lua,
    env: &Table,
    control: Arc<Mutex<crate::runner::HookControl>>,
    limits: crate::ExecutionLimits,
) -> mlua::Result<()> {
    let check = lua.create_function(move |_, ()| {
        let state = crate::runner::lock(&control);
        if state.time_exceeded {
            Err(mlua::Error::RuntimeError(
                limits.timeout_message(state.limit_line),
            ))
        } else if state.exhausted {
            Err(mlua::Error::RuntimeError(
                "instruction budget exceeded".into(),
            ))
        } else {
            Ok(())
        }
    })?;
    let pcall: mlua::Function = env.raw_get("pcall")?;
    let xpcall: mlua::Function = env.raw_get("xpcall")?;
    let table: Table = env.raw_get("table")?;
    let pack: mlua::Function = table.raw_get("pack")?;
    let unpack: mlua::Function = table.raw_get("unpack")?;
    let factory = lua
        .load(
            r#"
        local p, x, check, pack, unpack = ...
        return function(...) check(); local r=pack(p(...)); check(); return unpack(r,1,r.n) end,
               function(...) check(); local r=pack(x(...)); check(); return unpack(r,1,r.n) end
    "#,
        )
        .into_function()?;
    let (p, x): (mlua::Function, mlua::Function) =
        factory.call((pcall, xpcall, check, pack, unpack))?;
    env.raw_set("pcall", p)?;
    env.raw_set("xpcall", x)?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn sandbox_math(profile: storm_lua_spec::environment::EnvironmentProfile) -> (Lua, Table) {
        let lua = Lua::new();
        let (env, _) = environment(&lua, profile).unwrap();
        let math = env.get::<Table>("math").unwrap();
        (lua, math)
    }

    fn random_error(math: &Table, args: impl mlua::IntoLuaMulti) -> String {
        let random: mlua::Function = math.get("random").unwrap();
        let err = random.call::<Value>(args).unwrap_err();
        format!("{err:?}")
    }

    #[test]
    fn random_rejects_numbers_without_integer_representation() {
        let (_lua, math) = sandbox_math(storm_lua_spec::environment::EnvironmentProfile::Extended);
        for arg in [1.5, 2.5, 0.5, -1.5, 1e19, f64::NAN, f64::INFINITY] {
            let message = random_error(&math, arg);
            assert!(
                message
                    .contains("bad argument #1 to 'random' (number has no integer representation)"),
                "arg {arg}: {message}"
            );
        }
        let message = random_error(&math, "3.5");
        assert!(
            message.contains("number has no integer representation"),
            "{message}"
        );
        let message = random_error(&math, (1, 3.5));
        assert!(
            message.contains("bad argument #2 to 'random' (number has no integer representation)"),
            "{message}"
        );
        let message = random_error(&math, (1.5, 2.5));
        assert!(message.contains("bad argument #1 to 'random'"), "{message}");
    }

    #[test]
    fn random_rejects_non_numbers_with_puc_type_error() {
        let (lua, math) = sandbox_math(storm_lua_spec::environment::EnvironmentProfile::Extended);
        let message = random_error(&math, true);
        assert!(
            message.contains("bad argument #1 to 'random' (number expected, got boolean)"),
            "{message}"
        );
        let message = random_error(&math, lua.create_table().unwrap());
        assert!(
            message.contains("bad argument #1 to 'random' (number expected, got table)"),
            "{message}"
        );
        let message = random_error(&math, "abc");
        assert!(
            message.contains("bad argument #1 to 'random' (number expected, got string)"),
            "{message}"
        );
    }

    #[test]
    fn random_accepts_integral_and_coerced_forms_unchanged() {
        let (_lua, math) = sandbox_math(storm_lua_spec::environment::EnvironmentProfile::Extended);
        let random: mlua::Function = math.get("random").unwrap();
        let value: i64 = random.call(3.0).unwrap();
        assert!((1..=3).contains(&value));
        let value: i64 = random.call((2.0, 2.0)).unwrap();
        assert_eq!(value, 2);
        let value: i64 = random.call("3").unwrap();
        assert!((1..=3).contains(&value));
        let value: i64 = random.call("0x10").unwrap();
        assert!((1..=16).contains(&value));
        let value: i64 = random.call(5).unwrap();
        assert!((1..=5).contains(&value));
        let value: i64 = random.call((2, 9)).unwrap();
        assert!((2..=9).contains(&value));
        let value: f64 = random.call(()).unwrap();
        assert!((0.0..1.0).contains(&value));
        let randomseed: mlua::Function = math.get("randomseed").unwrap();
        randomseed.call::<()>(42.9).unwrap();
    }

    #[test]
    fn random_interval_contract_and_draw_accounting_unchanged() {
        let (_lua, math) = sandbox_math(storm_lua_spec::environment::EnvironmentProfile::Extended);
        let message = random_error(&math, (3, 1));
        assert!(message.contains("invalid random interval"), "{message}");
        let message = random_error(&math, 0);
        assert!(message.contains("invalid random interval"), "{message}");

        let (_lua2, math2) =
            sandbox_math(storm_lua_spec::environment::EnvironmentProfile::Extended);
        let random2: mlua::Function = math2.get("random").unwrap();
        let expected: i64 = random2.call(10).unwrap();
        let _ = random_error(&math, 1.5);
        let random: mlua::Function = math.get("random").unwrap();
        let actual: i64 = random.call(10).unwrap();
        assert_eq!(actual, expected);
    }
}
