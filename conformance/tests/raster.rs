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
