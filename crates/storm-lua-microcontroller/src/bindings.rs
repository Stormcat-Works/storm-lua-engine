//! APIの登録および数値変換。描画アルゴリズムは別のモジュールで実装されます。
//!
//! 引数の意味論は実ゲームに合わせます（E1/E2）。検査の順序は
//! (1) 引数の個数（arity）→ 不一致は無言で戻る（戻り値0個）、
//! (2) phase（I/O・描画許可フラグ）→ 違反は実ゲーム文言のLuaエラー、
//! (3) 値の変換 → 変換できない値は0/空として扱う、
//! の順です。個数の検査がないAPI（screen.getWidth/getHeight/drawClear）は
//! 任意の個数を受け付けます。
use super::{Phase, State};
use std::{cell::RefCell, rc::Rc};
use storm_lua_spec::{draw::DrawCommand, property::PropertyValue, screen::Rgba8};
use storm_lua_vm::{
    backend::{BackendError, BackendResult, Lua, LuaString, MultiValue, Table, Value, Variadic},
    runner::{ErrorKind, VmError},
};

/// Compositeチャンネル引数の変換（E25）。実ゲームはLua数値をcvttsd2siで
/// i32へ変換（小数部は切り捨て、範囲外・NaNでは不定値）してから内部
/// インデックス0..31へ写します。有効範囲1..=32の判定において、Rustの
/// `as i32`（飽和＋切り捨て）は不定値の場合と観測可能な差がありません。
fn channel(value: f64) -> Option<usize> {
    let c = value as i32;
    if (1..=32).contains(&c) {
        Some((c - 1) as usize)
    } else {
        None
    }
}
/// 実ゲームの変換ヘルパー（lua_tonumberx相当）で数値を得ます。実ゲームは
/// 変換の成功フラグを無視するため、変換できない値は0.0になります
/// （数値文字列はLuaの規則で変換され、真偽値は0になります）。
fn coerced_number(lua: &Lua, value: Option<&Value>) -> f64 {
    value
        .and_then(|value| lua.coerce_number(value.clone()).ok().flatten())
        .unwrap_or(0.0)
}
/// lua_tolstring相当で文字列を得ます。変換できない値は空文字列にします
/// （実ゲームでNULLが返る場合の扱いは未確定のため、描画されない空文字列で近似）。
fn coerced_text(lua: &Lua, value: Option<&Value>) -> Vec<u8> {
    value
        .and_then(|value| lua.coerce_string(value.clone()).ok().flatten())
        .map(|string| string.as_bytes().to_vec())
        .unwrap_or_default()
}
/// Lua値の真偽（lua_toboolean相当）。falseとnilだけが偽です。
fn truthy(value: Option<&Value>) -> bool {
    !matches!(value, None | Some(Value::Nil) | Some(Value::Boolean(false)))
}
fn resource(message: &str) -> BackendError {
    BackendError::external(VmError::new(ErrorKind::Limit, message))
}

/// 呼べないphaseで呼ばれたAPIのLuaエラー。文言は実ゲームの表示に合わせます。
/// Luaの実行時エラーと同じく、呼び出したLuaの行を`チャンク表記:行:`として先頭に付けます。
fn outside(lua: &Lua, family: &str, callback: &str) -> BackendError {
    let message = format!("attempting to use {family} function outside {callback}");
    let position = lua.inspect_stack(1).and_then(|frame| {
        let line = frame.curr_line();
        let source = frame.source().short_src?.into_owned();
        (line > 0).then(|| format!("{source}:{line}: "))
    });
    BackendError::RuntimeError(position.unwrap_or_default() + &message)
}
/// I/O許可フラグの検査（E4）。onTickの実行中だけ許可され、それ以外
/// （トップレベル・onDraw・httpReplyを含む）は実ゲームと同じ文言のLuaエラーです。
fn tick_only(lua: &Lua, phase: Phase, family: &str) -> BackendResult<()> {
    if phase == Phase::Tick {
        Ok(())
    } else {
        Err(outside(lua, family, "onTick"))
    }
}
/// 描画許可フラグの検査（E4）。onDrawの実行中だけ許可され、それ以外
/// （トップレベル・onTick・httpReplyを含む）は実ゲームと同じ文言のLuaエラーです。
fn draw_only(lua: &Lua, phase: Phase) -> BackendResult<()> {
    if phase == Phase::Draw {
        Ok(())
    } else {
        Err(outside(lua, "screen", "onDraw"))
    }
}

pub(super) fn install(lua: &Lua, env: &Table, state: Rc<RefCell<State>>) -> BackendResult<()> {
    let input = lua.create_table()?;
    let output = lua.create_table()?;
    let property = lua.create_table()?;
    let screen = lua.create_table()?;
    let s = Rc::clone(&state);
    input.raw_set(
        "getNumber",
        lua.create_function(move |lua, args: Variadic<Value>| {
            if args.len() != 1 {
                return Ok(MultiValue::new());
            }
            let s = s.borrow();
            tick_only(lua, s.phase, "input")?;
            let value = channel(coerced_number(lua, args.get(0)))
                .map_or(0.0, |i| f64::from(s.input.numbers[i]));
            Ok(MultiValue::from_vec(vec![Value::Number(value)]))
        })?,
    )?;
    let s = Rc::clone(&state);
    input.raw_set(
        "getBool",
        lua.create_function(move |lua, args: Variadic<Value>| {
            if args.len() != 1 {
                return Ok(MultiValue::new());
            }
            let s = s.borrow();
            tick_only(lua, s.phase, "input")?;
            let value = channel(coerced_number(lua, args.get(0)))
                .is_some_and(|i| s.input.booleans[i]);
            Ok(MultiValue::from_vec(vec![Value::Boolean(value)]))
        })?,
    )?;
    let s = Rc::clone(&state);
    output.raw_set(
        "setNumber",
        lua.create_function(move |lua, args: Variadic<Value>| {
            if args.len() != 2 {
                return Ok(());
            }
            let mut s = s.borrow_mut();
            tick_only(lua, s.phase, "output")?;
            if let Some(i) = channel(coerced_number(lua, args.get(0))) {
                // 値はf64→f32へ変換して保持します。NaN/Infもそのまま通します。
                s.output.numbers[i] = coerced_number(lua, args.get(1)) as f32;
            }
            Ok(())
        })?,
    )?;
    let s = Rc::clone(&state);
    output.raw_set(
        "setBool",
        lua.create_function(move |lua, args: Variadic<Value>| {
            if args.len() != 2 {
                return Ok(());
            }
            let mut s = s.borrow_mut();
            tick_only(lua, s.phase, "output")?;
            if let Some(i) = channel(coerced_number(lua, args.get(0))) {
                s.output.booleans[i] = truthy(args.get(1));
            }
            Ok(())
        })?,
    )?;
    let s = Rc::clone(&state);
    property.raw_set(
        "getNumber",
        lua.create_function(move |lua, args: Variadic<Value>| {
            if args.len() != 1 {
                return Ok(MultiValue::new());
            }
            let s = s.borrow();
            let label = coerced_text(lua, args.get(0));
            let value = match s.properties.get(&label) {
                Some(PropertyValue::Number(n)) => *n,
                _ => 0.0,
            };
            Ok(MultiValue::from_vec(vec![Value::Number(value)]))
        })?,
    )?;
    let s = Rc::clone(&state);
    property.raw_set(
        "getBool",
        lua.create_function(move |lua, args: Variadic<Value>| {
            if args.len() != 1 {
                return Ok(MultiValue::new());
            }
            let s = s.borrow();
            let label = coerced_text(lua, args.get(0));
            let value = matches!(
                s.properties.get(&label),
                Some(PropertyValue::Bool(true))
            );
            Ok(MultiValue::from_vec(vec![Value::Boolean(value)]))
        })?,
    )?;
    let s = Rc::clone(&state);
    property.raw_set(
        "getText",
        lua.create_function(move |lua, args: Variadic<Value>| {
            if args.len() != 1 {
                return Ok(MultiValue::new());
            }
            let s = s.borrow();
            let label = coerced_text(lua, args.get(0));
            let bytes = match s.properties.get(&label) {
                Some(PropertyValue::Text(bytes)) => bytes.clone(),
                _ => Vec::new(),
            };
            Ok(MultiValue::from_vec(vec![Value::String(
                lua.create_string(&bytes)?,
            )]))
        })?,
    )?;
    let s = Rc::clone(&state);
    screen.raw_set(
        "getWidth",
        lua.create_function(move |lua, _: Variadic<Value>| {
            // 個数の検査はありません（実ゲームはgettopを見ません）。
            let s = s.borrow();
            draw_only(lua, s.phase)?;
            Ok(s.size.0)
        })?,
    )?;
    let s = Rc::clone(&state);
    screen.raw_set(
        "getHeight",
        lua.create_function(move |lua, _: Variadic<Value>| {
            let s = s.borrow();
            draw_only(lua, s.phase)?;
            Ok(s.size.1)
        })?,
    )?;
    for definition in storm_lua_spec::catalog::FUNCTIONS
        .iter()
        .filter(|api| api.effect == "draw-command")
    {
        let name = definition
            .path
            .strip_prefix("screen.")
            .ok_or_else(|| BackendError::RuntimeError("invalid screen API catalog path".into()))?;
        // 実ゲームの引数個数（範囲外は無言で戻ります）。drawClearは個数の検査がありません。
        let arity: std::ops::RangeInclusive<usize> = match name {
            "setColor" => 3..=4,
            "drawClear" => 0..=usize::MAX,
            "drawLine" | "drawRect" | "drawRectF" => 4..=4,
            "drawCircle" | "drawCircleF" => 3..=3,
            "drawTriangle" | "drawTriangleF" => 6..=6,
            "drawText" => 3..=3,
            "drawTextBox" => 5..=7,
            _ => 0..=usize::MAX,
        };
        let s = Rc::clone(&state);
        screen.raw_set(
            name,
            lua.create_function(move |lua, args: Variadic<Value>| {
                if !arity.contains(&args.len()) {
                    return Ok(());
                }
                draw_only(lua, s.borrow().phase)?;
                let number = |i: usize| coerced_number(lua, args.get(i));
                let text = |i: usize| coerced_text(lua, args.get(i));
                let color = |n: f64| -> BackendResult<u8> {
                    if !n.is_finite() {
                        return Err(BackendError::RuntimeError("non-finite color".into()));
                    }
                    Ok((n + 0.5).floor().clamp(0.0, 255.0) as u8)
                };
                let command = match name {
                    "setColor" => DrawCommand::SetColor(Rgba8([
                        color(number(0))?,
                        color(number(1))?,
                        color(number(2))?,
                        if args.len() > 3 {
                            color(number(3))?
                        } else {
                            255
                        },
                    ])),
                    "drawClear" => DrawCommand::Clear,
                    "drawLine" => {
                        DrawCommand::Line([[number(0), number(1)], [number(2), number(3)]])
                    }
                    "drawRect" | "drawRectF" => DrawCommand::Rect(
                        [number(0), number(1), number(2), number(3)],
                        name == "drawRectF",
                    ),
                    "drawCircle" | "drawCircleF" => DrawCommand::Circle(
                        [number(0), number(1), number(2)],
                        name == "drawCircleF",
                    ),
                    "drawTriangle" | "drawTriangleF" => DrawCommand::Triangle(
                        [
                            [number(0), number(1)],
                            [number(2), number(3)],
                            [number(4), number(5)],
                        ],
                        name == "drawTriangleF",
                    ),
                    "drawText" => DrawCommand::Text([number(0), number(1)], text(2)),
                    "drawTextBox" => DrawCommand::TextBox(
                        [
                            number(0),
                            number(1),
                            number(2),
                            number(3),
                            number(5),
                            number(6),
                        ],
                        text(4),
                    ),
                    _ => {
                        return Err(BackendError::RuntimeError(
                            "unregistered draw operation".into(),
                        ))
                    }
                };
                s.borrow_mut()
                    .commands
                    .push(command)
                    .map_err(|e| resource(&e.to_string()))
            })?,
        )?;
    }
    let maps = Rc::clone(&state);
    screen.raw_set(
        "drawMap",
        lua.create_function(move |lua, args: Variadic<Value>| {
            if args.len() != 3 {
                return Ok(());
            }
            let mut state = maps.borrow_mut();
            draw_only(lua, state.phase)?;
            let values = [
                coerced_number(lua, args.get(0)),
                coerced_number(lua, args.get(1)),
                coerced_number(lua, args.get(2)),
            ];
            if !values.iter().all(|n| n.is_finite()) {
                return Err(BackendError::RuntimeError(
                    "map coordinates must be finite".into(),
                ));
            }
            state
                .commands
                .push(DrawCommand::Map(values))
                .map_err(|e| resource(&e.to_string()))
        })?,
    )?;
    for (kind, name) in [
        "Ocean", "Shallows", "Land", "Grass", "Sand", "Snow", "Rock", "Gravel",
    ]
    .into_iter()
    .enumerate()
    {
        let palette = Rc::clone(&state);
        let kind = storm_lua_spec::map::MapColorKind::try_from(kind as u8)
            .map_err(BackendError::external)?;
        screen.raw_set(
            format!("setMapColor{name}"),
            lua.create_function(move |lua, args: Variadic<Value>| {
                if !(3..=4).contains(&args.len()) {
                    return Ok(());
                }
                let mut state = palette.borrow_mut();
                draw_only(lua, state.phase)?;
                let values = [
                    coerced_number(lua, args.get(0)),
                    coerced_number(lua, args.get(1)),
                    coerced_number(lua, args.get(2)),
                    if args.len() > 3 {
                        coerced_number(lua, args.get(3))
                    } else {
                        255.0
                    },
                ];
                if !values.iter().all(|v| v.is_finite()) {
                    return Err(BackendError::RuntimeError(
                        "map palette color must be finite".into(),
                    ));
                }
                let rgba = Rgba8(values.map(|n| (n + 0.5).floor().clamp(0.0, 255.0) as u8));
                state
                    .commands
                    .push(DrawCommand::MapColor(kind, rgba))
                    .map_err(|e| resource(&e.to_string()))
            })?,
        )?;
    }
    let http = lua.create_table()?;
    let requests = Rc::clone(&state);
    http.raw_set(
        "httpGet",
        lua.create_function(move |_, (port, request): (i64, LuaString)| {
            requests
                .borrow_mut()
                .http
                .request(port, request.as_bytes().as_ref())
                .map_err(|error| BackendError::external(super::http_error(error)))
        })?,
    )?;
    env.raw_set("async", http)?;
    env.raw_set("input", input)?;
    env.raw_set("output", output)?;
    env.raw_set("property", property)?;
    env.raw_set("screen", screen)?;
    Ok(())
}
