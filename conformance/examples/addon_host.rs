//! ネイティブコンシューマの例: 明示的なワールドサービス、ログ、チェックポイント、およびマップ描画。
use std::{
    collections::BTreeMap,
    error::Error,
    sync::{Arc, Mutex},
};
use storm_lua_addon::{Addon, AddonConfig};
use storm_lua_microcontroller::Microcontroller;
use storm_lua_spec::{map::MapRequest, screen::Rgba8};
use storm_lua_vm::{
    runner::{ErrorKind, VmError},
    value::{HostFunction, LuaValue},
};
use storm_screen_raster::raster::ScreenRaster;

fn main() -> Result<(), Box<dyn Error>> {
    let messages = Arc::new(Mutex::new(Vec::<Vec<LuaValue>>::new()));
    let captured = Arc::clone(&messages);
    let announce: HostFunction = Arc::new(move |args| {
        if args.len() != 2 {
            return Err(VmError::new(
                ErrorKind::Host,
                "announce expects title and message",
            ));
        }
        captured
            .lock()
            .map_err(|_| VmError::new(ErrorKind::Host, "message log is poisoned"))?
            .push(args.to_vec());
        Ok(Vec::new())
    });
    let mut addon = Addon::new(AddonConfig {
        server: BTreeMap::from([("announce".to_owned(), announce)]),
        dev_logs: false,
        ..Default::default()
    })?;
    addon.load(
        br#"g_savedata={ticks=0}
function onCreate(new) server.announce('native','ready');debug.log('started') end
function onTick(dt) g_savedata.ticks=g_savedata.ticks+dt;debug.log('tick') end"#,
        "=native-addon",
    )?;
    addon.start()?;
    addon.tick(400)?;
    // これらのバイト列をターミナル、IDEパネル、または録画のいずれに出力するかはホストが決定します。
    for record in addon.drain_log_records() {
        println!(
            "{:?}: {}",
            record.source,
            std::str::from_utf8(&record.bytes)?
        );
    }
    let checkpoint = addon.savedata()?;
    addon.reload(checkpoint)?;
    addon.start()?;
    addon.tick(1)?;
    let LuaValue::Table(saved) = addon.savedata()? else {
        return Err("savedata is not a table".into());
    };
    if !saved
        .iter()
        .any(|(key, value)| *key == LuaValue::text("ticks") && *value == LuaValue::Integer(401))
    {
        return Err("checkpoint did not survive reload".into());
    }
    if messages
        .lock()
        .map_err(|_| "message log is poisoned")?
        .len()
        != 2
    {
        return Err("server callback was not invoked".into());
    }
    addon.destroy()?;

    let mut raster = ScreenRaster::new(16, 16)?;
    raster.set_map_provider(Some(std::sync::Arc::new(|request: &MapRequest| {
        // バンドルされたゲームワールドの実装ではなく、テスト用の合成地形。
        let water = request.colors[0].unwrap_or(Rgba8([16, 48, 80, 255]));
        Ok(water.0.repeat((request.width * request.height) as usize))
    })));
    let mut vehicle = Microcontroller::new(Default::default())?;
    vehicle.load(b"function onDraw() screen.drawMap(100,200,1);screen.setColor(255,255,255);screen.drawText(1,1,'MAP') end","=native-map")?;
    vehicle.draw(16, 16)?;
    raster.begin_frame();
    vehicle.replay(&mut raster)?;
    if raster.pixels().len() != 1024 {
        return Err("unexpected frame size".into());
    }
    println!("Native addon services and explicit map provider passed.");
    Ok(())
}
