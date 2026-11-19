//! ホスト地形は明示的に順序付けられます。組み込みのジオメトリ描画が偽のマップで代用されることはありません。
use std::{
    error::Error,
    sync::{Arc, Mutex},
};
use storm_lua_microcontroller::Microcontroller;
use storm_lua_spec::{
    draw::{DrawCommand, ScreenError, ScreenSink},
    map::{MapColorKind, MapRequest},
    screen::Rgba8,
};
use storm_screen_raster::raster::ScreenRaster;
#[test]
fn provider_receives_palette_and_map_order_without_changing_draw_color(
) -> Result<(), Box<dyn Error>> {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&calls);
    let mut raster = ScreenRaster::new(2, 2)?;
    raster.set_map_provider(Some(std::sync::Arc::new(move |request: &MapRequest| {
        captured
            .lock()
            .map_err(|_| ScreenError::Host("capture is poisoned".into()))?
            .push(request.clone());
        Ok([0, 0, 64, 255].repeat((request.width * request.height) as usize))
    })));
    let mut vm = Microcontroller::new(Default::default())?;
    vm.load(b"function onDraw() screen.setColor(255,0,0);screen.drawClear();screen.setMapColorOcean(0,0,64);screen.drawMap(4,5,6);screen.drawRectF(0,0,1,1) end","=map")?;
    vm.draw(2, 2)?;
    vm.replay(&mut raster)?;
    assert_eq!(&raster.pixels()[..4], &[255, 0, 0, 255]);
    assert_eq!(&raster.pixels()[4..], &[0, 0, 64, 255].repeat(3));
    let request = &calls.lock().map_err(|_| "capture is poisoned")?[0];
    assert_eq!(request.center, [4.0, 5.0]);
    assert_eq!(request.zoom, 6.0);
    assert_eq!(request.colors[0], Some(Rgba8([0, 0, 64, 255])));
    assert!(request.colors[1..].iter().all(Option::is_none));
    Ok(())
}
#[test]
fn invalid_provider_output_preserves_the_existing_prefix() -> Result<(), Box<dyn Error>> {
    let mut raster = ScreenRaster::new(2, 2)?;
    raster.submit(&DrawCommand::SetColor(Rgba8([10, 20, 30, 255])))?;
    raster.submit(&DrawCommand::Clear)?;
    let before = raster.pixels().to_vec();
    assert_eq!(
        raster.submit(&DrawCommand::Map([0.0, 0.0, 1.0])),
        Err(ScreenError::MissingMapProvider)
    );
    raster.set_map_provider(Some(std::sync::Arc::new(|_: &MapRequest| Ok(vec![0; 3]))));
    assert!(matches!(
        raster.submit(&DrawCommand::Map([0.0, 0.0, 1.0])),
        Err(ScreenError::Host(_))
    ));
    assert_eq!(raster.pixels(), before);
    Ok(())
}
#[test]
fn map_palette_is_frame_local_and_binary_batches_preserve_the_slot() -> Result<(), Box<dyn Error>> {
    let mut wire = vec![13, 0, 0, 0, 5, 0, 0, 0, 7, 1, 2, 3, 4];
    wire.extend_from_slice(&[12, 0, 0, 0, 24, 0, 0, 0]);
    for n in [1.5_f64, -2.5, 3.0] {
        wire.extend_from_slice(&n.to_le_bytes());
    }
    let commands = storm_lua_spec::command_wire::decode(&wire)?;
    assert_eq!(
        commands,
        vec![
            DrawCommand::MapColor(MapColorKind::Gravel, Rgba8([1, 2, 3, 4])),
            DrawCommand::Map([1.5, -2.5, 3.0])
        ]
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&seen);
    let mut raster = ScreenRaster::new(1, 1)?;
    raster.set_map_provider(Some(std::sync::Arc::new(move |request: &MapRequest| {
        captured
            .lock()
            .map_err(|_| ScreenError::Host("capture is poisoned".into()))?
            .push(request.colors);
        Ok(vec![0; 4])
    })));
    raster.draw_batch(&commands)?;
    raster.begin_frame();
    raster.submit(&commands[1])?;
    let seen = seen.lock().map_err(|_| "capture is poisoned")?;
    assert!(seen[0][7].is_some());
    assert!(seen[1].iter().all(Option::is_none));
    wire[8] = 8;
    assert!(storm_lua_spec::command_wire::decode(&wire).is_err());
    Ok(())
}
