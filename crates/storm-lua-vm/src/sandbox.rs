//! ベースラインの安全なLua環境およびVMごとの乱数状態。
use mlua::{Lua, Table, Value, Variadic};
use std::{cell::Cell, rc::Rc};

pub(crate) fn environment(
    lua: &Lua,
    profile: storm_lua_spec::environment::EnvironmentProfile,
) -> mlua::Result<Table> {
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
    // Lua文字列値はこのメタテーブルを共有するため、元のテーブルからもバイトコードダンプ（string.dump）を削除します。
    globals
        .get::<Table>("string")?
        .raw_set("dump", Value::Nil)?;
    if profile == storm_lua_spec::environment::EnvironmentProfile::Extended {
        env.raw_set(
            "unpack",
            globals.get::<Table>("table")?.raw_get::<Value>("unpack")?,
        )?;
    }
    install_random(lua, &env.get::<Table>("math")?)?;
    Ok(env)
}
/// `math.random` の区間引数を PUC Lua 5.3 の `luaL_checkinteger` と同じ規則で検証します。
/// mlua の `i64` 変換は小数部を切り捨ててしまうため、ここでは `lua_tointegerx` 相当の
/// `coerce_integer`（整数表現を正確に持つ値だけを受理し、数値文字列も強制する）を使います。
/// 整数表現を持たない数は PUC と同じ文面で、数でない値は型名つきの PUC 文面で拒否します。
/// 位置接頭辞は Rust コールバックからは付けられないため文面のみを揃えます（接頭辞が
/// 無いのは VM のコールバック・エラー全体に共通する既存の形で、区間エラー
/// `invalid random interval` の契約差と同じ扱い）。
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

fn install_random(lua: &Lua, math: &Table) -> mlua::Result<()> {
    // PUC-Lua 5.3のC言語RNGはプロセス全体で共有されます。ローカルな決定論的ストリームによりVM間の結合を回避します。
    // これは再現性と分離を保証するものであり、ゲームの未ドキュメントの乱数列を再現するものではありません。
    let seed = Rc::new(Cell::new(1_u64));
    let state = Rc::clone(&seed);
    math.raw_set(
        "randomseed",
        lua.create_function(move |_, value: i64| {
            state.set(value as u64);
            Ok(())
        })?,
    )?;
    math.raw_set(
        "random",
        lua.create_function(move |lua, args: Variadic<Value>| {
            let next = || {
                let z = seed.get().wrapping_add(0x9e3779b97f4a7c15);
                seed.set(z);
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
    control: Rc<std::cell::RefCell<crate::runner::HookControl>>,
) -> mlua::Result<()> {
    let check = lua.create_function(move |_, ()| {
        if control.borrow().exhausted {
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
        let env = environment(&lua, profile).unwrap();
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
        let (_lua, math) = sandbox_math(storm_lua_spec::environment::EnvironmentProfile::Game);
        // わずかでも小数部を持つ数、範囲外の数、NaN は PUC と同じ文面で拒否します
        // （以前は mlua の i64 変換が切り捨てて受理していました）。
        for arg in [1.5, 2.5, 0.5, -1.5, 1e19, f64::NAN, f64::INFINITY] {
            let message = random_error(&math, arg);
            assert!(
                message
                    .contains("bad argument #1 to 'random' (number has no integer representation)"),
                "arg {arg}: {message}"
            );
        }
        // 数値文字列の小数形も同様に拒否します（PUC の lua_tointegerx と同じ）。
        let message = random_error(&math, "3.5");
        assert!(
            message.contains("number has no integer representation"),
            "{message}"
        );
        // 第2引数の検証は引数番号つきで報告します。
        let message = random_error(&math, (1, 3.5));
        assert!(
            message.contains("bad argument #2 to 'random' (number has no integer representation)"),
            "{message}"
        );
        // 両方とも不正なら第1引数から報告します。
        let message = random_error(&math, (1.5, 2.5));
        assert!(message.contains("bad argument #1 to 'random'"), "{message}");
    }

    #[test]
    fn random_rejects_non_numbers_with_puc_type_error() {
        let (lua, math) = sandbox_math(storm_lua_spec::environment::EnvironmentProfile::Game);
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
        let (_lua, math) = sandbox_math(storm_lua_spec::environment::EnvironmentProfile::Game);
        let random: mlua::Function = math.get("random").unwrap();
        // 整数値 float は従来どおり受理します。
        let value: i64 = random.call(3.0).unwrap();
        assert!((1..=3).contains(&value));
        let value: i64 = random.call((2.0, 2.0)).unwrap();
        assert_eq!(value, 2);
        // 数値文字列の強制も従来どおり受理します。
        let value: i64 = random.call("3").unwrap();
        assert!((1..=3).contains(&value));
        let value: i64 = random.call("0x10").unwrap();
        assert!((1..=16).contains(&value));
        // 整数引数と無引数形も不変です。
        let value: i64 = random.call(5).unwrap();
        assert!((1..=5).contains(&value));
        let value: i64 = random.call((2, 9)).unwrap();
        assert!((2..=9).contains(&value));
        let value: f64 = random.call(()).unwrap();
        assert!((0.0..1.0).contains(&value));
        // randomseed の小数切り捨ては PUC 自身と同じため対象外（不変）です。
        let randomseed: mlua::Function = math.get("randomseed").unwrap();
        randomseed.call::<()>(42.9).unwrap();
    }

    #[test]
    fn random_interval_contract_and_draw_accounting_unchanged() {
        let (_lua, math) = sandbox_math(storm_lua_spec::environment::EnvironmentProfile::Game);
        // 空区間は文書化済みの VM 文面のままです（契約差として記録済み）。
        let message = random_error(&math, (3, 1));
        assert!(message.contains("invalid random interval"), "{message}");
        let message = random_error(&math, 0);
        assert!(message.contains("invalid random interval"), "{message}");

        // 拒否された呼び出しは乱数ドローを消費しません。
        let (_lua2, math2) = sandbox_math(storm_lua_spec::environment::EnvironmentProfile::Game);
        let random2: mlua::Function = math2.get("random").unwrap();
        let expected: i64 = random2.call(10).unwrap();
        let _ = random_error(&math, 1.5);
        let random: mlua::Function = math.get("random").unwrap();
        let actual: i64 = random.call(10).unwrap();
        assert_eq!(actual, expected);
    }
}
