//! 4x4行列演算（`matrix` テーブル）。addon と Component Mod で共有する。
//!
//! Lua table は row-major の16要素（キー1〜16）で、平行移動成分は要素13〜15
//! （0-based で 12〜14）。意味論は実ゲームの静的解析で確定した内容に従う:
//!
//! - `multiply(a, b)` の合成順序と `multiplyXYZW` のベクトル変換は、実ゲームの
//!   `matrix_multiply` / `matrix_multiply_xyzw` を読み、現行実装（旧
//!   storm-lua-addon の matrix）と一致することを乱数行列200ケースの数値比較で
//!   確認した。教科書的な row-major 積 `a * b` ではない（裁定 E10 の当初方向は
//!   バイナリ実測と食い違うため採用していない。経緯は v0.5.0 の matrix
//!   レーン記録を参照）。
//! - rotation 系コンストラクタが返す table も実ゲームと一致する。
//! - `multiplyXYZW` のベクトル成分は実ゲームで f32 へ丸めてから f64 で計算する。
//! - `rotationToFaceXZ` は実ゲームの `matrix_rotation_to_face_xz` から確定した
//!   式（f32 正規化 + 符号付き角度）を実装する。
//!
//! 引数の検証とエラー文字列も実ゲームに合わせる:
//!
//! - nil 引数: `matrix was nil`
//! - table でない引数・number へ変換できない要素: `bad matrix input type: <型名>`
//! - 数値キーの総数が16でない: `bad matrix size`
//! - 数値でないキー: `bad value in matrix: <キー>`
//! - 1〜16 の範囲外の数値キー: `matrix index out of range: <キー>`
//! - 引数の個数（identity 以外で完全一致を要求）: `Wrong number of args`
//!
//! 有限性は要求しない（NaN・無限大もそのまま通す）。スカラー引数は
//! `lua_tonumberx` と同じ変換で読み、変換できない値は 0 として扱う。
use crate::backend::{BackendError, BackendResult, Lua, Table, Value, Variadic};
use crate::runner::{ErrorKind, VmError};

fn invalid(message: impl Into<String>) -> BackendError {
    BackendError::external(VmError::new(ErrorKind::InvalidArgument, message.into()))
}

fn identity() -> [f64; 16] {
    let mut m = [0.0_f64; 16];
    for i in 0..4 {
        m[i * 5] = 1.0;
    }
    m
}

/// `lua_tonumberx` と同じ変換。変換できない値は 0。
fn number(lua: &Lua, value: &Value) -> f64 {
    lua.coerce_number(value.clone())
        .ok()
        .flatten()
        .unwrap_or(0.0)
}

/// lua_typename と同じ型名（整数と浮動小数点はどちらも number）。
fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Integer(_) | Value::Number(_) => "number",
        other => other.type_name(),
    }
}

/// エラーメッセージ用のキー表記（Lua 5.3 の数値→文字列に近づける）。
fn key_text(value: &Value) -> String {
    match value {
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => {
            if n.is_finite() && *n == n.trunc() && n.abs() < 1e15 {
                format!("{n:.1}")
            } else {
                format!("{n}")
            }
        }
        Value::String(s) => String::from_utf8_lossy(&s.as_bytes()).into_owned(),
        other => other.type_name().to_string(),
    }
}

/// Lua table を row-major の16要素として読みます。検証とエラー文字列は
/// matrix テーブル本体と同一で、Component Mod の matrix 引数（renderMesh 等）も
/// 同じ変換を使います。
pub fn read_matrix(lua: &Lua, value: &Value) -> BackendResult<[f64; 16]> {
    let table = match value {
        Value::Nil => return Err(invalid("matrix was nil")),
        Value::Table(table) => table,
        other => {
            return Err(invalid(format!(
                "bad matrix input type: {}",
                type_name(other)
            )))
        }
    };
    let mut m = [0.0_f64; 16];
    let mut count = 0_usize;
    for pair in table.pairs::<Value, Value>() {
        let (key, element) = pair?;
        let index = match &key {
            Value::Integer(i) => *i as f64,
            Value::Number(n) => *n,
            _ => match lua.coerce_number(key.clone()).ok().flatten() {
                Some(n) => n,
                None => return Err(invalid(format!("bad value in matrix: {}", key_text(&key)))),
            },
        };
        if index.fract() != 0.0 || !(1.0..=16.0).contains(&index) {
            return Err(invalid(format!(
                "matrix index out of range: {}",
                key_text(&key)
            )));
        }
        let element = match lua.coerce_number(element.clone()).ok().flatten() {
            Some(n) => n,
            None => {
                return Err(invalid(format!(
                    "bad matrix input type: {}",
                    type_name(&element)
                )))
            }
        };
        m[(index as usize) - 1] = element;
        count += 1;
    }
    if count != 16 {
        return Err(invalid("bad matrix size"));
    }
    Ok(m)
}

/// 16要素を Lua の連番 table（キー1〜16）として構築します。
pub fn write_matrix(lua: &Lua, m: [f64; 16]) -> BackendResult<Table> {
    lua.create_sequence_from(m)
}

fn arity(args: &Variadic<Value>, expected: usize) -> BackendResult<()> {
    if args.len() != expected {
        return Err(invalid("Wrong number of args"));
    }
    Ok(())
}

/// 実ゲームと一致する合成順序（検証済みのため変更しないこと）。
fn multiply(a: [f64; 16], b: [f64; 16]) -> [f64; 16] {
    let mut c = [0.0_f64; 16];
    for col in 0..4 {
        for row in 0..4 {
            for k in 0..4 {
                c[col * 4 + row] += a[k * 4 + row] * b[col * 4 + k];
            }
        }
    }
    c
}

fn inverse(m: [f64; 16]) -> BackendResult<[f64; 16]> {
    let mut rows = [[0.0; 8]; 4];
    for r in 0..4 {
        for c in 0..4 {
            rows[r][c] = m[c * 4 + r];
        }
        rows[r][r + 4] = 1.0;
    }
    for col in 0..4 {
        let mut pivot = col;
        for r in col + 1..4 {
            if rows[r][col].abs() > rows[pivot][col].abs() {
                pivot = r;
            }
        }
        if rows[pivot][col] == 0.0 {
            return Err(invalid("cannot invert a singular matrix"));
        }
        rows.swap(col, pivot);
        let scale = rows[col][col];
        for value in &mut rows[col] {
            *value /= scale;
        }
        for r in 0..4 {
            if r != col {
                let scale = rows[r][col];
                let pivot_row = rows[col];
                for (value, pivot_value) in rows[r].iter_mut().zip(pivot_row) {
                    *value -= scale * pivot_value;
                }
            }
        }
    }
    let mut result = [0.0_f64; 16];
    for r in 0..4 {
        for c in 0..4 {
            result[c * 4 + r] = rows[r][c + 4];
        }
    }
    Ok(result)
}

fn rotation(axis: usize, angle: f64) -> [f64; 16] {
    let (s, c) = angle.sin_cos();
    let mut m = identity();
    match axis {
        0 => {
            m[5] = c;
            m[6] = s;
            m[9] = -s;
            m[10] = c;
        }
        1 => {
            m[0] = c;
            m[2] = -s;
            m[8] = s;
            m[10] = c;
        }
        _ => {
            m[0] = c;
            m[1] = s;
            m[4] = -s;
            m[5] = c;
        }
    }
    m
}

/// 実ゲーム `matrix_rotation_to_face_xz` の静的解析による確定式。
/// 引数を f32 化して長さで正規化し（長さが正でない場合は (1, 0)）、
/// (0, 1) との符号付き角度 θ = ±acos(nz)（符号は nx の向き）で Y 回転を作る。
fn rotation_to_face_xz(x: f64, z: f64) -> [f64; 16] {
    let x = x as f32;
    let z = z as f32;
    let len = (z * z + x * x).sqrt();
    let (nx, nz) = if len > 0.0 {
        (x / len, z / len)
    } else {
        (1.0, 0.0)
    };
    let angle = nz.clamp(-1.0, 1.0).acos();
    let angle = if nx > 0.0 { angle } else { -angle };
    rotation(1, angle as f64)
}

fn scale(x: f64, y: f64, z: f64) -> [f64; 16] {
    let mut m = identity();
    m[0] = x;
    m[5] = y;
    m[10] = z;
    m
}

fn install_impl(lua: &Lua, env: &Table, component: bool) -> BackendResult<()> {
    let matrix = lua.create_table()?;
    matrix.raw_set(
        "identity",
        lua.create_function(|lua, args: Variadic<Value>| {
            arity(&args, 0)?;
            write_matrix(lua, identity())
        })?,
    )?;
    matrix.raw_set(
        "translation",
        lua.create_function(|lua, args: Variadic<Value>| {
            arity(&args, 3)?;
            let mut m = identity();
            m[12] = number(lua, &args[0]);
            m[13] = number(lua, &args[1]);
            m[14] = number(lua, &args[2]);
            write_matrix(lua, m)
        })?,
    )?;
    matrix.raw_set(
        "position",
        lua.create_function(|lua, args: Variadic<Value>| {
            arity(&args, 1)?;
            let m = read_matrix(lua, &args[0])?;
            Ok((m[12], m[13], m[14]))
        })?,
    )?;
    matrix.raw_set(
        "multiply",
        lua.create_function(|lua, args: Variadic<Value>| {
            arity(&args, 2)?;
            write_matrix(lua, multiply(read_matrix(lua, &args[0])?, read_matrix(lua, &args[1])?))
        })?,
    )?;
    matrix.raw_set(
        "transpose",
        lua.create_function(|lua, args: Variadic<Value>| {
            arity(&args, 1)?;
            let m = read_matrix(lua, &args[0])?;
            let mut t = [0.0_f64; 16];
            for r in 0..4 {
                for c in 0..4 {
                    t[c * 4 + r] = m[r * 4 + c];
                }
            }
            write_matrix(lua, t)
        })?,
    )?;
    matrix.raw_set(
        "invert",
        lua.create_function(|lua, args: Variadic<Value>| {
            arity(&args, 1)?;
            write_matrix(lua, inverse(read_matrix(lua, &args[0])?)?)
        })?,
    )?;
    matrix.raw_set(
        "distance",
        lua.create_function(|lua, args: Variadic<Value>| {
            arity(&args, 2)?;
            let a = read_matrix(lua, &args[0])?;
            let b = read_matrix(lua, &args[1])?;
            Ok(
                ((a[12] - b[12]).powi(2) + (a[13] - b[13]).powi(2) + (a[14] - b[14]).powi(2))
                    .sqrt(),
            )
        })?,
    )?;
    matrix.raw_set(
        "multiplyXYZW",
        lua.create_function(|lua, args: Variadic<Value>| {
            arity(&args, 5)?;
            let m = read_matrix(lua, &args[0])?;
            // 実ゲームはベクトル成分を f32 へ丸めてから f64 で計算する。
            let v = [
                number(lua, &args[1]) as f32 as f64,
                number(lua, &args[2]) as f32 as f64,
                number(lua, &args[3]) as f32 as f64,
                number(lua, &args[4]) as f32 as f64,
            ];
            let mut out = [0.0; 4];
            for r in 0..4 {
                for c in 0..4 {
                    out[r] += m[c * 4 + r] * v[c];
                }
            }
            Ok((out[0], out[1], out[2], out[3]))
        })?,
    )?;
    for (name, axis) in [("rotationX", 0), ("rotationY", 1), ("rotationZ", 2)] {
        matrix.raw_set(
            name,
            lua.create_function(move |lua, args: Variadic<Value>| {
                arity(&args, 1)?;
                write_matrix(lua, rotation(axis, number(lua, &args[0])))
            })?,
        )?;
    }
    matrix.raw_set(
        "rotationToFaceXZ",
        lua.create_function(|lua, args: Variadic<Value>| {
            arity(&args, 2)?;
            write_matrix(
                lua,
                rotation_to_face_xz(number(lua, &args[0]), number(lua, &args[1])),
            )
        })?,
    )?;
    if component {
        matrix.raw_set(
            "scale",
            lua.create_function(|lua, args: Variadic<Value>| {
                arity(&args, 3)?;
                write_matrix(
                    lua,
                    scale(
                        number(lua, &args[0]),
                        number(lua, &args[1]),
                        number(lua, &args[2]),
                    ),
                )
            })?,
        )?;
    }
    env.raw_set("matrix", matrix)?;
    Ok(())
}

/// addon の `matrix` テーブル（12関数）を環境へ組み込む。
pub fn install(lua: &Lua, env: &Table) -> BackendResult<()> {
    install_impl(lua, env, false)
}

/// Component Mod の `matrix` テーブル（addon の集合 + `scale`）を組み込む。
pub fn install_component(lua: &Lua, env: &Table) -> BackendResult<()> {
    install_impl(lua, env, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendResult, Function, Lua};

    fn env_with(component: bool) -> BackendResult<(Lua, Table)> {
        let lua = Lua::new();
        let env = lua.create_table()?;
        if component {
            install_component(&lua, &env)?;
        } else {
            install(&lua, &env)?;
        }
        Ok((lua, env))
    }

    #[test]
    fn addon_set_has_no_scale_but_component_set_does() -> BackendResult<()> {
        let (_lua, env) = env_with(false)?;
        let matrix: Table = env.get("matrix")?;
        assert_eq!(matrix.get::<Value>("scale")?, Value::Nil);
        assert_ne!(matrix.get::<Value>("rotationToFaceXZ")?, Value::Nil);
        let (_lua, env) = env_with(true)?;
        let matrix: Table = env.get("matrix")?;
        let scale: Function = matrix.get("scale")?;
        let m: Table = scale.call((2.0, 3.0, 4.0))?;
        let values: Vec<f64> = m.sequence_values().collect::<BackendResult<_>>()?;
        assert_eq!(
            values,
            vec![
                2.0, 0.0, 0.0, 0.0, //
                0.0, 3.0, 0.0, 0.0, //
                0.0, 0.0, 4.0, 0.0, //
                0.0, 0.0, 0.0, 1.0
            ]
        );
        Ok(())
    }

    #[test]
    fn rotation_to_face_xz_faces_the_given_direction() {
        // (1, 0) は +X 向き = rotationY(π/2)、(0, 0) は (1, 0) へのフォールバック。
        let m = rotation_to_face_xz(1.0, 0.0);
        let expected = rotation(1, std::f64::consts::FRAC_PI_2);
        for (a, b) in m.iter().zip(expected) {
            assert!((a - b).abs() < 1e-6, "{a} != {b}");
        }
        assert_eq!(rotation_to_face_xz(0.0, 0.0), m);
        // (0, 1) は単位回転、(-1, 0) は rotationY(-π/2)。
        let m = rotation_to_face_xz(0.0, 1.0);
        assert_eq!(m, identity());
        let m = rotation_to_face_xz(-1.0, 0.0);
        let expected = rotation(1, -std::f64::consts::FRAC_PI_2);
        for (a, b) in m.iter().zip(expected) {
            assert!((a - b).abs() < 1e-6, "{a} != {b}");
        }
    }
}
