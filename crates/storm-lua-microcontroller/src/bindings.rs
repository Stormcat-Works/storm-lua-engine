//! APIの登録および数値変換。描画アルゴリズムは別のモジュールで実装されます。
use super::{Phase, State};
use std::{cell::RefCell, rc::Rc};
use storm_lua_spec::{draw::DrawCommand, property::PropertyValue, screen::Rgba8};
use storm_lua_vm::{
    backend::{BackendError, BackendResult, Lua, LuaString, Table, Value, Variadic},
    runner::{ErrorKind, VmError},
};

fn channel(c: i64) -> Option<usize> {
    if (1..=32).contains(&c) {
        Some(c as usize - 1)
    } else {
        None
    }
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
/// input/outputはonDrawの実行中に呼ぶとエラー。トップレベルとhttpReplyでの挙動は
/// 実ゲームで未測定のため、従来の既定値（0/false、書き込み無効）を維持します。
fn tick_only(lua: &Lua, phase: Phase, family: &str) -> BackendResult<()> {
    if phase == Phase::Draw {
        Err(outside(lua, family, "onTick"))
    } else {
        Ok(())
    }
}
/// screenはonTickの実行中に呼ぶとエラー。それ以外の非描画phaseでは従来どおり何もしません。
fn draw_only(lua: &Lua, phase: Phase) -> BackendResult<bool> {
    match phase {
        Phase::Draw => Ok(true),
        Phase::Tick => Err(outside(lua, "screen", "onDraw")),
        Phase::Init | Phase::Idle => Ok(false),
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
        lua.create_function(move |lua, c: i64| {
            let s = s.borrow();
            tick_only(lua, s.phase, "input")?;
            Ok(if s.phase == Phase::Tick {
                channel(c).map_or(0.0, |i| f64::from(s.input.numbers[i]))
            } else {
                0.0
            })
        })?,
    )?;
    let s = Rc::clone(&state);
    input.raw_set(
        "getBool",
        lua.create_function(move |lua, c: i64| {
            let s = s.borrow();
            tick_only(lua, s.phase, "input")?;
            Ok(s.phase == Phase::Tick && channel(c).is_some_and(|i| s.input.booleans[i]))
        })?,
    )?;
    let s = Rc::clone(&state);
    output.raw_set(
        "setNumber",
        lua.create_function(move |lua, (c, v): (i64, f64)| {
            let mut s = s.borrow_mut();
            tick_only(lua, s.phase, "output")?;
            if s.phase == Phase::Tick {
                if let Some(i) = channel(c) {
                    s.output.numbers[i] = v as f32;
                }
            }
            Ok(())
        })?,
    )?;
    let s = Rc::clone(&state);
    output.raw_set(
        "setBool",
        lua.create_function(move |lua, (c, v): (i64, bool)| {
            let mut s = s.borrow_mut();
            tick_only(lua, s.phase, "output")?;
            if s.phase == Phase::Tick {
                if let Some(i) = channel(c) {
                    s.output.booleans[i] = v;
                }
            }
            Ok(())
        })?,
    )?;
    let s = Rc::clone(&state);
    property.raw_set(
        "getNumber",
        lua.create_function(move |_, label: LuaString| {
            let s = s.borrow();
            Ok(match s.properties.get(label.as_bytes().as_ref()) {
                Some(PropertyValue::Number(n)) => *n,
                _ => 0.0,
            })
        })?,
    )?;
    let s = Rc::clone(&state);
    property.raw_set(
        "getBool",
        lua.create_function(move |_, label: LuaString| {
            let s = s.borrow();
            Ok(matches!(
                s.properties.get(label.as_bytes().as_ref()),
                Some(PropertyValue::Bool(true))
            ))
        })?,
    )?;
    let s = Rc::clone(&state);
    property.raw_set(
        "getText",
        lua.create_function(move |lua, label: LuaString| {
            let s = s.borrow();
            let bytes = match s.properties.get(label.as_bytes().as_ref()) {
                Some(PropertyValue::Text(bytes)) => bytes.as_slice(),
                _ => b"",
            };
            lua.create_string(bytes)
        })?,
    )?;
    let s = Rc::clone(&state);
    screen.raw_set(
        "getWidth",
        lua.create_function(move |lua, ()| {
            let s = s.borrow();
            Ok(if draw_only(lua, s.phase)? {
                s.size.0
            } else {
                0
            })
        })?,
    )?;
    let s = Rc::clone(&state);
    screen.raw_set(
        "getHeight",
        lua.create_function(move |lua, ()| {
            let s = s.borrow();
            Ok(if draw_only(lua, s.phase)? {
                s.size.1
            } else {
                0
            })
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
        let s = Rc::clone(&state);
        screen.raw_set(
            name,
            lua.create_function(move |lua, args: Variadic<Value>| {
                if !draw_only(lua, s.borrow().phase)? {
                    return Ok(());
                }
                let number = |i: usize| -> BackendResult<f64> {
                    let value = args.get(i).cloned().ok_or_else(|| {
                        BackendError::RuntimeError(format!("{name}: missing argument {}", i + 1))
                    })?;
                    lua.coerce_number(value)?.ok_or_else(|| {
                        BackendError::RuntimeError(format!(
                            "{name}: argument {} must be numeric",
                            i + 1
                        ))
                    })
                };
                let text = |i: usize| -> BackendResult<Vec<u8>> {
                    let value = args.get(i).cloned().ok_or_else(|| {
                        BackendError::RuntimeError(format!("{name}: missing text"))
                    })?;
                    let string = lua.coerce_string(value)?.ok_or_else(|| {
                        BackendError::RuntimeError(format!(
                            "{name}: text must be a string or number"
                        ))
                    })?;
                    if string.as_bytes().len() > 1024 * 1024 {
                        return Err(resource("screen text exceeds 1 MiB"));
                    }
                    Ok(string.as_bytes().to_vec())
                };
                let color = |n: f64| -> BackendResult<u8> {
                    if !n.is_finite() {
                        return Err(BackendError::RuntimeError("non-finite color".into()));
                    }
                    Ok((n + 0.5).floor().clamp(0.0, 255.0) as u8)
                };
                let command = match name {
                    "setColor" => DrawCommand::SetColor(Rgba8([
                        color(number(0)?)?,
                        color(number(1)?)?,
                        color(number(2)?)?,
                        if args.len() > 3 {
                            color(number(3)?)?
                        } else {
                            255
                        },
                    ])),
                    "drawClear" => DrawCommand::Clear,
                    "drawLine" => {
                        DrawCommand::Line([[number(0)?, number(1)?], [number(2)?, number(3)?]])
                    }
                    "drawRect" | "drawRectF" => DrawCommand::Rect(
                        [number(0)?, number(1)?, number(2)?, number(3)?],
                        name == "drawRectF",
                    ),
                    "drawCircle" | "drawCircleF" => DrawCommand::Circle(
                        [number(0)?, number(1)?, number(2)?],
                        name == "drawCircleF",
                    ),
                    "drawTriangle" | "drawTriangleF" => DrawCommand::Triangle(
                        [
                            [number(0)?, number(1)?],
                            [number(2)?, number(3)?],
                            [number(4)?, number(5)?],
                        ],
                        name == "drawTriangleF",
                    ),
                    "drawText" => DrawCommand::Text([number(0)?, number(1)?], text(2)?),
                    "drawTextBox" => DrawCommand::TextBox(
                        [
                            number(0)?,
                            number(1)?,
                            number(2)?,
                            number(3)?,
                            number(5)?,
                            number(6)?,
                        ],
                        text(4)?,
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
        lua.create_function(move |lua, (x, z, zoom): (f64, f64, f64)| {
            let mut state = maps.borrow_mut();
            if !draw_only(lua, state.phase)? {
                return Ok(());
            }
            if ![x, z, zoom].iter().all(|n| n.is_finite()) {
                return Err(BackendError::RuntimeError(
                    "map coordinates must be finite".into(),
                ));
            }
            state
                .commands
                .push(DrawCommand::Map([x, z, zoom]))
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
            lua.create_function(move |lua, (r, g, b, a): (f64, f64, f64, Option<f64>)| {
                let mut state = palette.borrow_mut();
                if !draw_only(lua, state.phase)? {
                    return Ok(());
                }
                let values = [r, g, b, a.unwrap_or(255.0)];
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
