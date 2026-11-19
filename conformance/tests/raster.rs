//! 採用済みの画面契約を直接描画とLua実行の両経路で検証します。
mod support;
use std::{collections::HashSet, error::Error};
use storm_lua_spec::{
    draw::{CommandBuffer, DrawCommand, ScreenSink},
    screen::Rgba8,
};
use storm_screen_raster::raster::ScreenRaster;

#[test]
fn accepted_screen_contract_matches_direct_and_lua_paths() -> Result<(), Box<dyn Error>> {
    let root: serde_json::Value =
        serde_json::from_str(include_str!("../../fixtures/screen/cases-v1.json"))?;
    assert_eq!(root["format"], 1);
    assert_eq!(root["contract"], "storm-lua-screen-rgba-v1");
    let cases = root["cases"].as_array().ok_or("cases missing")?;
    assert_eq!(cases.len(), 743);
    let mut names = HashSet::new();
    let mut failures = Vec::new();
    for case in cases {
        let id = case["id"].as_str().ok_or("id missing")?;
        assert!(names.insert(id));
        let width = u32::try_from(case["width"].as_u64().ok_or("width missing")?)?;
        let height = u32::try_from(case["height"].as_u64().ok_or("height missing")?)?;
        let commands: Vec<_> = case["ops"]
            .as_array()
            .ok_or("ops missing")?
            .iter()
            .map(support::command)
            .collect::<Result<_, _>>()?;
        let mut raster = ScreenRaster::new(width, height)?;
        raster.draw_batch(&commands)?;
        let mut expected = Vec::new();
        for run in case["expectedRgbaRle"].as_array().ok_or("RLE missing")? {
            let n = run[0].as_u64().ok_or("run count missing")?;
            assert!(n > 0 && n <= u64::from(width) * u64::from(height));
            let pixel: [u8; 4] = [
                u8::try_from(run[1].as_u64().ok_or("R missing")?)?,
                u8::try_from(run[2].as_u64().ok_or("G missing")?)?,
                u8::try_from(run[3].as_u64().ok_or("B missing")?)?,
                u8::try_from(run[4].as_u64().ok_or("A missing")?)?,
            ];
            for _ in 0..n {
                expected.extend_from_slice(&pixel);
            }
        }
        assert_eq!(expected.len(), width as usize * height as usize * 4);
        let mut vm = storm_lua_microcontroller::Microcontroller::new(Default::default())?;
        let source = support::lua_source(case["ops"].as_array().ok_or("ops missing")?)?;
        assert_eq!(
            vm.load(source.as_bytes(), "=screen-contract")?,
            storm_lua_vm::runner::RunOutcome::Completed
        );
        assert_eq!(
            vm.draw(width, height)?,
            storm_lua_vm::runner::RunOutcome::Completed
        );
        let mut through_lua = ScreenRaster::new(width, height)?;
        vm.replay(&mut through_lua)?;
        for (path, pixels) in [("direct", raster.pixels()), ("Lua", through_lua.pixels())] {
            let diff = pixels.iter().zip(&expected).filter(|(a, b)| a != b).count();
            if diff != 0 {
                failures.push(format!("{id} [{path}]: {diff} differing bytes"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}
#[test]
fn clear_is_replacement_and_frame_start_resets_white() -> Result<(), Box<dyn Error>> {
    let mut raster = ScreenRaster::new(8, 8)?;
    raster.draw_batch(&[
        DrawCommand::SetColor(Rgba8([90, 120, 150, 64])),
        DrawCommand::Clear,
    ])?;
    assert!(raster
        .pixels()
        .chunks_exact(4)
        .all(|p| p == [90, 120, 150, 64]));
    raster.begin_frame();
    assert!(raster.pixels().iter().all(|p| *p == 0));
    raster.submit(&DrawCommand::Rect([0.0, 0.0, 1.0, 1.0], true))?;
    assert_eq!(&raster.pixels()[..4], &[255; 4]);
    Ok(())
}
#[test]
fn errors_do_not_become_empty_successes() -> Result<(), Box<dyn Error>> {
    for (w, h) in [(0, 1), (1, 0), (u32::MAX, 2), (4096, 4096)] {
        assert!(ScreenRaster::new(w, h).is_err());
    }
    let mut raster = ScreenRaster::new(8, 8)?;
    assert!(raster
        .submit(&DrawCommand::Text([0.0, 0.0], vec![255]))
        .is_err());
    let mut commands = CommandBuffer::new(2, 2);
    commands.push(DrawCommand::Text([0.0, 0.0], b"AB".to_vec()))?;
    assert!(commands
        .push(DrawCommand::Text([0.0, 0.0], b"C".to_vec()))
        .is_err());
    assert_eq!(commands.commands().len(), 1);
    commands.clear();
    commands.push(DrawCommand::Clear)?;
    commands.push(DrawCommand::Clear)?;
    assert!(commands.push(DrawCommand::Clear).is_err());
    Ok(())
}
#[test]
fn draw_text_matches_bytes_to_glyph_ids_without_advancing_unknown_bytes(
) -> Result<(), Box<dyn Error>> {
    // drawTextはbyte単位でglyph IDを照合します（E21）。UTF-8の各byteには
    // 対応glyphがないため送りも描画もなく、"AéB"は"AB"と同じ画素になります。
    // glyphが1個もない文字列は何も描きません。
    let mut with_unknown = ScreenRaster::new(32, 8)?;
    with_unknown.submit(&DrawCommand::Text([0.0, 0.0], "AéB".as_bytes().to_vec()))?;
    let mut plain = ScreenRaster::new(32, 8)?;
    plain.submit(&DrawCommand::Text([0.0, 0.0], b"AB".to_vec()))?;
    assert_eq!(with_unknown.pixels(), plain.pixels());
    let mut only_unknown = ScreenRaster::new(32, 8)?;
    only_unknown.submit(&DrawCommand::Text([0.0, 0.0], "é".as_bytes().to_vec()))?;
    assert!(only_unknown.pixels().iter().all(|p| *p == 0));
    Ok(())
}

#[test]
fn draw_text_box_wraps_by_tokens_and_handles_tab_and_unknown_codepoints(
) -> Result<(), Box<dyn Error>> {
    // 仕様の例: "A BC DE"をwidth 14で分割すると A / BC / 空白 / DE（E22）。
    let mut boxed = ScreenRaster::new(32, 40)?;
    boxed.submit(&DrawCommand::TextBox(
        [0.0, 0.0, 14.0, 30.0, -1.0, -1.0],
        b"A BC DE".to_vec(),
    ))?;
    let mut expected = ScreenRaster::new(32, 40)?;
    for (y, line) in [(0.0, "A "), (6.0, "BC"), (12.0, " "), (18.0, "DE")] {
        expected.submit(&DrawCommand::Text([0.0, y], line.as_bytes().to_vec()))?;
    }
    assert_eq!(boxed.pixels(), expected.pixels());
    // TABは空白glyphの送り4倍（20）です。"A\tB"のBはx=25に来ます。
    let mut tabbed = ScreenRaster::new(64, 8)?;
    tabbed.submit(&DrawCommand::TextBox(
        [0.0, 0.0, 100.0, 8.0, -1.0, -1.0],
        b"A\tB".to_vec(),
    ))?;
    let mut tab_expected = ScreenRaster::new(64, 8)?;
    tab_expected.submit(&DrawCommand::Text([0.0, 0.0], b"A".to_vec()))?;
    tab_expected.submit(&DrawCommand::Text([25.0, 0.0], b"B".to_vec()))?;
    assert_eq!(tabbed.pixels(), tab_expected.pixels());
    // glyphがないcodepointはトークンにならず送りも生じません。"AéB"のBはx=5です。
    let mut unknown = ScreenRaster::new(32, 8)?;
    unknown.submit(&DrawCommand::TextBox(
        [0.0, 0.0, 100.0, 8.0, -1.0, -1.0],
        "AéB".as_bytes().to_vec(),
    ))?;
    let mut unknown_expected = ScreenRaster::new(32, 8)?;
    unknown_expected.submit(&DrawCommand::Text([0.0, 0.0], b"AB".to_vec()))?;
    assert_eq!(unknown.pixels(), unknown_expected.pixels());
    Ok(())
}

#[test]
fn circle_points_follow_f32_sequential_angle_addition() -> Result<(), Box<dyn Error>> {
    // 円の周辺点はfloat32のtau/分割数を0から逐次加算して作ります（E24）。
    // 半径31（分割数15）のoutlineは16点になり、仕様どおりに独立に計算した
    // 線分列と同じ画素になります。
    let (cx, cy, radius) = (40.0_f64, 40.0_f64, 31.0_f64);
    let tau: f32 = std::f32::consts::TAU;
    let step = tau / 15.0_f32;
    let mut points: Vec<[f64; 2]> = Vec::new();
    let mut angle: f32 = 0.0;
    while angle <= tau {
        let a = f64::from(angle);
        points.push([
            f64::from((cx + radius * a.cos()) as f32),
            f64::from((cy + radius * a.sin()) as f32),
        ]);
        angle += step;
    }
    assert_eq!(points.len(), 16);
    let mut expected = ScreenRaster::new(96, 96)?;
    for i in 0..points.len() {
        expected.submit(&DrawCommand::Line([
            points[i],
            points[(i + 1) % points.len()],
        ]))?;
    }
    let mut circle = ScreenRaster::new(96, 96)?;
    circle.submit(&DrawCommand::Circle([cx, cy, radius], false))?;
    assert_eq!(circle.pixels(), expected.pixels());
    Ok(())
}

#[test]
fn enormous_and_nonfinite_shapes_remain_bounded() -> Result<(), Box<dyn Error>> {
    let mut raster = ScreenRaster::new(32, 32)?;
    for n in [1e9, -1e9, f64::MAX, f64::NAN, f64::INFINITY] {
        raster.draw_batch(&[
            DrawCommand::Rect([-n, -n, n * 2.0, n * 2.0], true),
            DrawCommand::Circle([16.0, 16.0, n], false),
            DrawCommand::Line([[-n, 0.0], [n, 32.0]]),
        ])?;
    }
    Ok(())
}
