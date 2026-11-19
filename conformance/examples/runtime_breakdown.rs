//! 実行時コストの内訳を測る小さなベンチ。Luaの実行とラスタライズを分け、描画命令の種類ごとに測ります。
//! `benchmark`と同じくバッチ平均の中央値であり、個別呼び出しのレイテンシではありません。
use std::{error::Error, hint::black_box, time::Instant};
use storm_lua_microcontroller::{Microcontroller, MicrocontrollerConfig};
use storm_lua_spec::io::CompositeSignal;
use storm_screen_raster::raster::ScreenRaster;

/// 9バッチの平均時間（マイクロ秒）の中央値。
fn measure(
    mut operation: impl FnMut() -> Result<(), Box<dyn Error>>,
    iterations: usize,
) -> Result<f64, Box<dyn Error>> {
    for _ in 0..(iterations / 4).max(3) {
        operation()?;
    }
    let mut samples = Vec::new();
    for _ in 0..9 {
        let start = Instant::now();
        for _ in 0..iterations {
            operation()?;
        }
        samples.push(start.elapsed().as_secs_f64() * 1e6 / iterations as f64);
    }
    samples.sort_by(f64::total_cmp);
    Ok(samples[4])
}
fn vehicle(source: &str) -> Result<Microcontroller, Box<dyn Error>> {
    // 既定の制限を使います。どの入力も1回の呼び出しが既定の命令数の上限より十分少なく、
    // 上限の型や既定値を変える版でも同じコードで測れます。
    let mut vm = Microcontroller::new(MicrocontrollerConfig::default())?;
    vm.load(source.as_bytes(), "=breakdown")?;
    Ok(vm)
}

const TICKS: [(&str, &str, usize); 4] = [
    ("empty", "function onTick() end", 20000),
    (
        "loop20k",
        "local s=0 function onTick() for i=1,20000 do s=(s+i*1.5)%1000 end end",
        200,
    ),
    (
        "calls5k",
        "local f=math.floor local s=0 function onTick() for i=1,5000 do s=s+f(i/3)+math.sin(i) end end",
        200,
    ),
    (
        "io32",
        "function onTick() for i=1,32 do output.setNumber(i,input.getNumber(i)*2) output.setBool(i,input.getBool(i)) end end",
        5000,
    ),
];
const DRAWS: [(&str, &str); 10] = [
    (
        "rectF-opaque",
        "function onDraw() screen.setColor(10,20,30) for i=0,49 do screen.drawRectF(i*5,i*3,60,40) end end",
    ),
    (
        "rectF-alpha",
        "function onDraw() screen.setColor(10,20,30,128) for i=0,49 do screen.drawRectF(i*5,i*3,60,40) end end",
    ),
    (
        "clear",
        "function onDraw() screen.setColor(10,20,30) for i=0,9 do screen.drawClear() end end",
    ),
    (
        "circleF",
        "function onDraw() screen.setColor(10,20,30,200) for i=0,49 do screen.drawCircleF(20+i*5,20+i*2,18) end end",
    ),
    (
        "triangleF",
        "function onDraw() screen.setColor(10,20,30,200) for i=0,49 do screen.drawTriangleF(i*5,i*2,i*5+50,i*2+10,i*5+20,i*2+60) end end",
    ),
    (
        "line",
        "function onDraw() screen.setColor(10,20,30) for i=0,99 do screen.drawLine(i*2.5,0,287-i*2.5,159) end end",
    ),
    (
        "rect",
        "function onDraw() screen.setColor(10,20,30) for i=0,49 do screen.drawRect(i*5,i*3,60,40) end end",
    ),
    (
        "circle",
        "function onDraw() screen.setColor(10,20,30) for i=0,49 do screen.drawCircle(20+i*5,20+i*2,18) end end",
    ),
    (
        "text",
        "function onDraw() screen.setColor(10,20,30) for i=0,24 do screen.drawText(2,i*6,'The quick brown fox jumps over the lazy dog 0123456789') end end",
    ),
    (
        "textBox",
        "function onDraw() screen.setColor(10,20,30) for i=0,9 do screen.drawTextBox(0,0,288,160,'The quick brown fox jumps over the lazy dog 0123456789 and again and again and again',0,0) end end",
    ),
];

fn main() -> Result<(), Box<dyn Error>> {
    let input = CompositeSignal::default();
    let mut ticks = serde_json::Map::new();
    for (label, source, iterations) in TICKS {
        let mut vm = vehicle(source)?;
        let micros = measure(
            || {
                vm.tick(&input)?;
                black_box(vm.output());
                Ok(())
            },
            iterations,
        )?;
        ticks.insert(label.into(), micros.into());
    }
    let mut draws = serde_json::Map::new();
    for (label, source) in DRAWS {
        let mut vm = vehicle(source)?;
        let lua = measure(
            || {
                vm.draw(288, 160)?;
                Ok(())
            },
            300,
        )?;
        let mut raster = ScreenRaster::new(288, 160)?;
        vm.draw(288, 160)?;
        let replay = measure(
            || {
                raster.begin_frame();
                vm.replay(&mut raster)?;
                black_box(raster.pixels());
                Ok(())
            },
            300,
        )?;
        draws.insert(
            label.into(),
            serde_json::json!({"luaUs": lua, "rasterUs": replay}),
        );
    }
    println!(
        "{}",
        serde_json::json!({"runtime":"native-release","debugCompiled":true,"screen":[288,160],"tickUs":ticks,"draw":draws})
    );
    Ok(())
}
