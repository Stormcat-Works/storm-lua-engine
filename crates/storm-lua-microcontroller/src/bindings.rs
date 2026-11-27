//! APIの登録および数値変換。描画アルゴリズムは別のモジュールで実装されます。
//!
//! 引数の意味論は実ゲームに合わせます（E1/E2）。検査の順序は
//! (1) 引数の個数（arity）→ 不一致は無言で戻る（戻り値0個）、
//! (2) phase（I/O・描画許可フラグ）→ 違反は実ゲーム文言のLuaエラー、
//! (3) 値の変換 → 変換できない値は0/空として扱う、
//! の順です。個数の検査がないAPI（screen.getWidth/getHeight/drawClear）は
//! 任意の個数を受け付けます。
use super::{read_state, write_state, FastIo, Phase, State};
use std::sync::{atomic::Ordering, Arc, RwLock};
use storm_lua_spec::{draw::DrawCommand, property::PropertyValue, screen::Rgba8};
use storm_lua_vm::{
    backend::{BackendError, BackendResult, FromLua, Lua, MultiValue, Table, Value},
    runner::{ErrorKind, VmError},
};

/// Distinguishes an omitted Lua stack slot (`Missing`) from an explicit `nil`
/// (`Present(Value::Nil)`) without allocating a `Vec` or `VecDeque` per call.
enum StackArg {
    Missing,
    Present(Value),
}

impl FromLua for StackArg {
    #[inline]
    fn from_lua(value: Value, _lua: &Lua) -> BackendResult<Self> {
        Ok(Self::Present(value))
    }

    #[inline]
    fn from_lua_arg(_arg: Value, _i: usize, _to: Option<&str>, _lua: &Lua) -> BackendResult<Self> {
        Ok(Self::Missing)
    }
}

/// Compositeチャンネル引数の変換（E25）。実ゲームはLua数値をcvttsd2siで
/// i32へ変換（小数部は切り捨て、範囲外・NaNでは不定値）してから内部
/// インデックス0..31へ写します。有効範囲1..=32の判定において、Rustの
/// `as i32`（飽和＋切り捨て）は不定値の場合と観測可能な差がありません。
#[inline]
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
#[inline]
fn coerced_number_val(lua: &Lua, value: Value) -> f64 {
    match value {
        Value::Number(n) => n,
        Value::Integer(n) => n as f64,
        Value::Nil | Value::Boolean(_) => 0.0,
        other => lua.coerce_number(other).ok().flatten().unwrap_or(0.0),
    }
}

/// lua_tolstring相当で文字列を得ます。変換できない値は空文字列にします。
#[inline]
fn coerced_text_val(lua: &Lua, value: Value) -> Vec<u8> {
    match value {
        Value::String(s) => s.as_bytes().to_vec(),
        Value::Number(_) | Value::Integer(_) => lua
            .coerce_string(value)
            .ok()
            .flatten()
            .map(|s| s.as_bytes().to_vec())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// 色成分の変換（E3）。実ゲームはLua数値を`cvttsd2si`でint32化し、下位8bitを
/// 格納します（ゼロ方向への切り捨て、クランプなしのラップ）。NaN/±Infや
/// int32の範囲外は不定値`0x80000000`となり、保存byteは0です。非数値は
/// 変換ヘルパーで0.0になるため、ここでも0になります。
#[inline]
fn color_byte(n: f64) -> u8 {
    let truncated = n.trunc();
    if !truncated.is_finite() || truncated < f64::from(i32::MIN) || truncated > f64::from(i32::MAX)
    {
        return 0;
    }
    (truncated as i32) as u8
}

/// `map.screenToMap` / `map.mapToScreen`の座標変換（E15）。仕様の変換式を
/// float32の演算順で評価し、結果をdoubleへ拡張して返します。
#[allow(clippy::too_many_arguments)]
#[inline]
fn map_convert(
    to_world: bool,
    map_x: f64,
    map_y: f64,
    zoom: f64,
    width: f64,
    height: f64,
    x: f64,
    y: f64,
) -> (f64, f64) {
    let (map_x, map_y, zoom, width, height, x, y) = (
        map_x as f32,
        map_y as f32,
        zoom as f32,
        width as f32,
        height as f32,
        x as f32,
        y as f32,
    );
    let span_x = zoom * 1000.0;
    let span_y = span_x / (width / height);
    let (out_x, out_y) = if to_world {
        (
            (map_x - span_x * 0.5) + (x / width) * span_x,
            (map_y - span_y * 0.5) + ((height - y) / height) * span_y,
        )
    } else {
        (
            ((span_x * 0.5 + (x - map_x)) / span_x) * width,
            height - ((span_y * 0.5 + (y - map_y)) / span_y) * height,
        )
    };
    (f64::from(out_x), f64::from(out_y))
}

#[inline]
fn truthy_val(value: &Value) -> bool {
    !matches!(value, Value::Nil | Value::Boolean(false))
}

fn resource(message: &str) -> BackendError {
    BackendError::external(VmError::new(ErrorKind::Limit, message))
}

#[inline]
fn push_draw(state: &RwLock<State>, command: DrawCommand) -> BackendResult<()> {
    write_state(state)
        .commands
        .push(command)
        .map_err(|e| resource(&e.to_string()))
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

/// I/O許可フラグの検査（E4）。onTickの実行中だけ許可されます。
#[inline]
fn tick_only(lua: &Lua, phase: Phase, family: &str) -> BackendResult<()> {
    if phase == Phase::Tick {
        Ok(())
    } else {
        Err(outside(lua, family, "onTick"))
    }
}

/// 描画許可フラグの検査（E4）。onDrawの実行中だけ許可されます。
#[inline]
fn draw_only(lua: &Lua, phase: Phase) -> BackendResult<()> {
    if phase == Phase::Draw {
        Ok(())
    } else {
        Err(outside(lua, "screen", "onDraw"))
    }
}

#[inline]
fn single_return(value: Value) -> MultiValue {
    let mut out = MultiValue::new();
    out.push_back(value);
    out
}

pub(super) fn install(
    lua: &Lua,
    env: &Table,
    io: Arc<FastIo>,
    state: Arc<RwLock<State>>,
) -> BackendResult<()> {
    let input = lua.create_table()?;
    let output = lua.create_table()?;
    let property = lua.create_table()?;
    let screen = lua.create_table()?;

    {
        let io = Arc::clone(&io);
        input.raw_set(
            "getNumber",
            lua.create_function(move |lua, (a0, extra): (StackArg, StackArg)| {
                let (StackArg::Present(v0), StackArg::Missing) = (a0, extra) else {
                    return Ok(MultiValue::new());
                };
                tick_only(lua, io.phase(), "input")?;
                let value = channel(coerced_number_val(lua, v0)).map_or(0.0, |i| {
                    f64::from(f32::from_bits(io.input_numbers[i].load(Ordering::Relaxed)))
                });
                Ok(single_return(Value::Number(value)))
            })?,
        )?;
    }
    {
        let io = Arc::clone(&io);
        input.raw_set(
            "getBool",
            lua.create_function(move |lua, (a0, extra): (StackArg, StackArg)| {
                let (StackArg::Present(v0), StackArg::Missing) = (a0, extra) else {
                    return Ok(MultiValue::new());
                };
                tick_only(lua, io.phase(), "input")?;
                let value = channel(coerced_number_val(lua, v0))
                    .is_some_and(|i| io.input_booleans[i].load(Ordering::Relaxed));
                Ok(single_return(Value::Boolean(value)))
            })?,
        )?;
    }
    {
        let io = Arc::clone(&io);
        output.raw_set(
            "setNumber",
            lua.create_function(
                move |lua, (a0, a1, extra): (StackArg, StackArg, StackArg)| {
                    let (StackArg::Present(v0), StackArg::Present(v1), StackArg::Missing) =
                        (a0, a1, extra)
                    else {
                        return Ok(());
                    };
                    tick_only(lua, io.phase(), "output")?;
                    if let Some(i) = channel(coerced_number_val(lua, v0)) {
                        let bits = (coerced_number_val(lua, v1) as f32).to_bits();
                        io.output_numbers[i].store(bits, Ordering::Relaxed);
                    }
                    Ok(())
                },
            )?,
        )?;
    }
    {
        let io = Arc::clone(&io);
        output.raw_set(
            "setBool",
            lua.create_function(
                move |lua, (a0, a1, extra): (StackArg, StackArg, StackArg)| {
                    let (StackArg::Present(v0), StackArg::Present(v1), StackArg::Missing) =
                        (a0, a1, extra)
                    else {
                        return Ok(());
                    };
                    tick_only(lua, io.phase(), "output")?;
                    if let Some(i) = channel(coerced_number_val(lua, v0)) {
                        io.output_booleans[i].store(truthy_val(&v1), Ordering::Relaxed);
                    }
                    Ok(())
                },
            )?,
        )?;
    }

    {
        let s = Arc::clone(&state);
        property.raw_set(
            "getNumber",
            lua.create_function(move |lua, (a0, extra): (StackArg, StackArg)| {
                let (StackArg::Present(v0), StackArg::Missing) = (a0, extra) else {
                    return Ok(MultiValue::new());
                };
                let value = match v0 {
                    Value::String(label) => {
                        let s = read_state(&s);
                        match s.properties.get(label.as_bytes().as_ref()) {
                            Some(PropertyValue::Number(n)) => *n,
                            _ => 0.0,
                        }
                    }
                    other => {
                        let label = coerced_text_val(lua, other);
                        let s = read_state(&s);
                        match s.properties.get(&label) {
                            Some(PropertyValue::Number(n)) => *n,
                            _ => 0.0,
                        }
                    }
                };
                Ok(single_return(Value::Number(value)))
            })?,
        )?;
    }
    {
        let s = Arc::clone(&state);
        property.raw_set(
            "getBool",
            lua.create_function(move |lua, (a0, extra): (StackArg, StackArg)| {
                let (StackArg::Present(v0), StackArg::Missing) = (a0, extra) else {
                    return Ok(MultiValue::new());
                };
                let value = match v0 {
                    Value::String(label) => {
                        let s = read_state(&s);
                        matches!(
                            s.properties.get(label.as_bytes().as_ref()),
                            Some(PropertyValue::Bool(true))
                        )
                    }
                    other => {
                        let label = coerced_text_val(lua, other);
                        let s = read_state(&s);
                        matches!(s.properties.get(&label), Some(PropertyValue::Bool(true)))
                    }
                };
                Ok(single_return(Value::Boolean(value)))
            })?,
        )?;
    }
    {
        let s = Arc::clone(&state);
        property.raw_set(
            "getText",
            lua.create_function(move |lua, (a0, extra): (StackArg, StackArg)| {
                let (StackArg::Present(v0), StackArg::Missing) = (a0, extra) else {
                    return Ok(MultiValue::new());
                };
                let string = match v0 {
                    Value::String(label) => {
                        let s = read_state(&s);
                        let bytes = match s.properties.get(label.as_bytes().as_ref()) {
                            Some(PropertyValue::Text(bytes)) => bytes.as_slice(),
                            _ => b"",
                        };
                        lua.create_string(bytes)?
                    }
                    other => {
                        let label = coerced_text_val(lua, other);
                        let s = read_state(&s);
                        let bytes = match s.properties.get(&label) {
                            Some(PropertyValue::Text(bytes)) => bytes.as_slice(),
                            _ => b"",
                        };
                        lua.create_string(bytes)?
                    }
                };
                Ok(single_return(Value::String(string)))
            })?,
        )?;
    }

    {
        let io = Arc::clone(&io);
        screen.raw_set(
            "getWidth",
            lua.create_function(move |lua, ()| {
                // 個数の検査はありません（実ゲームはgettopを見ません）。
                // 戻り値は実ゲームと同じくdoubleです（E19）。
                draw_only(lua, io.phase())?;
                Ok(f64::from(io.size().0))
            })?,
        )?;
    }
    {
        let io = Arc::clone(&io);
        screen.raw_set(
            "getHeight",
            lua.create_function(move |lua, ()| {
                draw_only(lua, io.phase())?;
                Ok(f64::from(io.size().1))
            })?,
        )?;
    }

    {
        let (io, s) = (Arc::clone(&io), Arc::clone(&state));
        screen.raw_set(
            "setColor",
            lua.create_function(
                move |lua,
                      (r, g, b, a, extra): (StackArg, StackArg, StackArg, StackArg, StackArg)| {
                    let (
                        StackArg::Present(r),
                        StackArg::Present(g),
                        StackArg::Present(b),
                        a,
                        StackArg::Missing,
                    ) = (r, g, b, a, extra)
                    else {
                        return Ok(());
                    };
                    draw_only(lua, io.phase())?;
                    let rgba = Rgba8([
                        color_byte(coerced_number_val(lua, r)),
                        color_byte(coerced_number_val(lua, g)),
                        color_byte(coerced_number_val(lua, b)),
                        match a {
                            StackArg::Missing => 255,
                            StackArg::Present(v) => color_byte(coerced_number_val(lua, v)),
                        },
                    ]);
                    push_draw(&s, DrawCommand::SetColor(rgba))
                },
            )?,
        )?;
    }
    {
        let (io, s) = (Arc::clone(&io), Arc::clone(&state));
        screen.raw_set(
            "drawClear",
            lua.create_function(move |lua, ()| {
                draw_only(lua, io.phase())?;
                push_draw(&s, DrawCommand::Clear)
            })?,
        )?;
    }
    {
        let (io, s) = (Arc::clone(&io), Arc::clone(&state));
        screen.raw_set(
            "drawLine",
            lua.create_function(
                move |lua,
                      (x1, y1, x2, y2, extra): (
                    StackArg,
                    StackArg,
                    StackArg,
                    StackArg,
                    StackArg,
                )| {
                    let (
                        StackArg::Present(x1),
                        StackArg::Present(y1),
                        StackArg::Present(x2),
                        StackArg::Present(y2),
                        StackArg::Missing,
                    ) = (x1, y1, x2, y2, extra)
                    else {
                        return Ok(());
                    };
                    draw_only(lua, io.phase())?;
                    let cmd = DrawCommand::Line([
                        [coerced_number_val(lua, x1), coerced_number_val(lua, y1)],
                        [coerced_number_val(lua, x2), coerced_number_val(lua, y2)],
                    ]);
                    push_draw(&s, cmd)
                },
            )?,
        )?;
    }
    for (name, fill) in [("drawRect", false), ("drawRectF", true)] {
        let (io, s) = (Arc::clone(&io), Arc::clone(&state));
        screen.raw_set(
            name,
            lua.create_function(
                move |lua,
                      (x, y, w, h, extra): (StackArg, StackArg, StackArg, StackArg, StackArg)| {
                    let (
                        StackArg::Present(x),
                        StackArg::Present(y),
                        StackArg::Present(w),
                        StackArg::Present(h),
                        StackArg::Missing,
                    ) = (x, y, w, h, extra)
                    else {
                        return Ok(());
                    };
                    draw_only(lua, io.phase())?;
                    let cmd = DrawCommand::Rect(
                        [
                            coerced_number_val(lua, x),
                            coerced_number_val(lua, y),
                            coerced_number_val(lua, w),
                            coerced_number_val(lua, h),
                        ],
                        fill,
                    );
                    push_draw(&s, cmd)
                },
            )?,
        )?;
    }
    for (name, fill) in [("drawCircle", false), ("drawCircleF", true)] {
        let (io, s) = (Arc::clone(&io), Arc::clone(&state));
        screen.raw_set(
            name,
            lua.create_function(
                move |lua, (x, y, r, extra): (StackArg, StackArg, StackArg, StackArg)| {
                    let (
                        StackArg::Present(x),
                        StackArg::Present(y),
                        StackArg::Present(r),
                        StackArg::Missing,
                    ) = (x, y, r, extra)
                    else {
                        return Ok(());
                    };
                    draw_only(lua, io.phase())?;
                    let cmd = DrawCommand::Circle(
                        [
                            coerced_number_val(lua, x),
                            coerced_number_val(lua, y),
                            coerced_number_val(lua, r),
                        ],
                        fill,
                    );
                    push_draw(&s, cmd)
                },
            )?,
        )?;
    }
    for (name, fill) in [("drawTriangle", false), ("drawTriangleF", true)] {
        let (io, s) = (Arc::clone(&io), Arc::clone(&state));
        screen.raw_set(
            name,
            lua.create_function(
                move |lua,
                      (x1, y1, x2, y2, x3, y3, extra): (
                    StackArg,
                    StackArg,
                    StackArg,
                    StackArg,
                    StackArg,
                    StackArg,
                    StackArg,
                )| {
                    let (
                        StackArg::Present(x1),
                        StackArg::Present(y1),
                        StackArg::Present(x2),
                        StackArg::Present(y2),
                        StackArg::Present(x3),
                        StackArg::Present(y3),
                        StackArg::Missing,
                    ) = (x1, y1, x2, y2, x3, y3, extra)
                    else {
                        return Ok(());
                    };
                    draw_only(lua, io.phase())?;
                    let cmd = DrawCommand::Triangle(
                        [
                            [coerced_number_val(lua, x1), coerced_number_val(lua, y1)],
                            [coerced_number_val(lua, x2), coerced_number_val(lua, y2)],
                            [coerced_number_val(lua, x3), coerced_number_val(lua, y3)],
                        ],
                        fill,
                    );
                    push_draw(&s, cmd)
                },
            )?,
        )?;
    }
    {
        let (io, s) = (Arc::clone(&io), Arc::clone(&state));
        screen.raw_set(
            "drawText",
            lua.create_function(
                move |lua, (x, y, text, extra): (StackArg, StackArg, StackArg, StackArg)| {
                    let (
                        StackArg::Present(x),
                        StackArg::Present(y),
                        StackArg::Present(text),
                        StackArg::Missing,
                    ) = (x, y, text, extra)
                    else {
                        return Ok(());
                    };
                    draw_only(lua, io.phase())?;
                    let cmd = DrawCommand::Text(
                        [coerced_number_val(lua, x), coerced_number_val(lua, y)],
                        coerced_text_val(lua, text),
                    );
                    push_draw(&s, cmd)
                },
            )?,
        )?;
    }
    {
        let (io, s) = (Arc::clone(&io), Arc::clone(&state));
        screen.raw_set(
            "drawTextBox",
            lua.create_function(
                move |lua,
                      (x, y, w, h, text, ha, va, extra): (
                    StackArg,
                    StackArg,
                    StackArg,
                    StackArg,
                    StackArg,
                    StackArg,
                    StackArg,
                    StackArg,
                )| {
                    let (
                        StackArg::Present(x),
                        StackArg::Present(y),
                        StackArg::Present(w),
                        StackArg::Present(h),
                        StackArg::Present(text),
                        ha,
                        va,
                        StackArg::Missing,
                    ) = (x, y, w, h, text, ha, va, extra)
                    else {
                        return Ok(());
                    };
                    draw_only(lua, io.phase())?;
                    let align = |arg: StackArg| match arg {
                        StackArg::Missing => -1.0,
                        StackArg::Present(v) => coerced_number_val(lua, v).clamp(-1.0, 1.0),
                    };
                    let coords = [
                        coerced_number_val(lua, x),
                        coerced_number_val(lua, y),
                        coerced_number_val(lua, w),
                        coerced_number_val(lua, h),
                        align(ha),
                        align(va),
                    ];
                    push_draw(
                        &s,
                        DrawCommand::TextBox(coords, coerced_text_val(lua, text)),
                    )
                },
            )?,
        )?;
    }
    {
        let (io, maps) = (Arc::clone(&io), Arc::clone(&state));
        screen.raw_set(
            "drawMap",
            lua.create_function(
                move |lua, (x, z, zoom, extra): (StackArg, StackArg, StackArg, StackArg)| {
                    let (
                        StackArg::Present(x),
                        StackArg::Present(z),
                        StackArg::Present(zoom),
                        StackArg::Missing,
                    ) = (x, z, zoom, extra)
                    else {
                        return Ok(());
                    };
                    if io.map_used.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    draw_only(lua, io.phase())?;
                    io.map_used.store(true, Ordering::Relaxed);
                    let values = [
                        coerced_number_val(lua, x),
                        coerced_number_val(lua, z),
                        coerced_number_val(lua, zoom),
                    ];
                    push_draw(&maps, DrawCommand::Map(values))
                },
            )?,
        )?;
    }
    for (kind, name) in [
        "Ocean", "Shallows", "Land", "Grass", "Sand", "Snow", "Rock", "Gravel",
    ]
    .into_iter()
    .enumerate()
    {
        let (io, palette) = (Arc::clone(&io), Arc::clone(&state));
        let kind = storm_lua_spec::map::MapColorKind::try_from(kind as u8)
            .map_err(BackendError::external)?;
        screen.raw_set(
            format!("setMapColor{name}"),
            lua.create_function(
                move |lua,
                      (r, g, b, a, extra): (StackArg, StackArg, StackArg, StackArg, StackArg)| {
                    let (
                        StackArg::Present(r),
                        StackArg::Present(g),
                        StackArg::Present(b),
                        a,
                        StackArg::Missing,
                    ) = (r, g, b, a, extra)
                    else {
                        return Ok(());
                    };
                    draw_only(lua, io.phase())?;
                    let rgba = Rgba8([
                        color_byte(coerced_number_val(lua, r)),
                        color_byte(coerced_number_val(lua, g)),
                        color_byte(coerced_number_val(lua, b)),
                        match a {
                            StackArg::Missing => 255,
                            StackArg::Present(v) => color_byte(coerced_number_val(lua, v)),
                        },
                    ]);
                    push_draw(&palette, DrawCommand::MapColor(kind, rgba))
                },
            )?,
        )?;
    }
    // map.screenToMap / map.mapToScreen（E15）。7引数で、個数が合わない場合は
    // エラーにならず戻り値0個です。onDraw許可判定はありません。
    let map = lua.create_table()?;
    for to_world in [true, false] {
        let name = if to_world {
            "screenToMap"
        } else {
            "mapToScreen"
        };
        map.raw_set(
            name,
            lua.create_function(
                move |lua,
                      (a0, a1, a2, a3, a4, a5, a6, extra): (
                    StackArg,
                    StackArg,
                    StackArg,
                    StackArg,
                    StackArg,
                    StackArg,
                    StackArg,
                    StackArg,
                )| {
                    let (
                        StackArg::Present(v0),
                        StackArg::Present(v1),
                        StackArg::Present(v2),
                        StackArg::Present(v3),
                        StackArg::Present(v4),
                        StackArg::Present(v5),
                        StackArg::Present(v6),
                        StackArg::Missing,
                    ) = (a0, a1, a2, a3, a4, a5, a6, extra)
                    else {
                        return Ok(MultiValue::new());
                    };
                    let (out_x, out_y) = map_convert(
                        to_world,
                        coerced_number_val(lua, v0),
                        coerced_number_val(lua, v1),
                        coerced_number_val(lua, v2),
                        coerced_number_val(lua, v3),
                        coerced_number_val(lua, v4),
                        coerced_number_val(lua, v5),
                        coerced_number_val(lua, v6),
                    );
                    let mut out = MultiValue::new();
                    out.push_back(Value::Number(out_x));
                    out.push_back(Value::Number(out_y));
                    Ok(out)
                },
            )?,
        )?;
    }
    // async.httpGet（2引数）。個数が合わない場合はエラーにならず戻り値0個です。
    let http = lua.create_table()?;
    let requests = Arc::clone(&state);
    http.raw_set(
        "httpGet",
        lua.create_function(
            move |lua, (port_arg, request_arg, extra): (StackArg, StackArg, StackArg)| {
                let (
                    StackArg::Present(port_value),
                    StackArg::Present(request_value),
                    StackArg::Missing,
                ) = (port_arg, request_arg, extra)
                else {
                    return Ok(MultiValue::new());
                };
                let port = match port_value {
                    Value::Integer(n) => n,
                    other => coerced_number_val(lua, other) as i64,
                };
                let request = coerced_text_val(lua, request_value);
                write_state(&requests)
                    .http
                    .request(port, &request)
                    .map_err(|error| BackendError::external(super::http_error(error)))?;
                Ok(MultiValue::new())
            },
        )?,
    )?;
    env.raw_set("async", http)?;
    env.raw_set("map", map)?;
    env.raw_set("input", input)?;
    env.raw_set("output", output)?;
    env.raw_set("property", property)?;
    env.raw_set("screen", screen)?;
    Ok(())
}
