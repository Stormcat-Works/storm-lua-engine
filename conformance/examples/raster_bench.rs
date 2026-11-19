//! ラスタライザ単体のベンチ。Lua VM を介さず `ScreenRaster` へ直接コマンド列を流し、
//! 描画命令の種類ごとに測ります。各シーンは描画後のフレームバッファの FNV-1a ハッシュも
//! 出力するので、A/B 比較の中で出力の同一性まで確認できます。
//! 数値は9バッチの中央値（バッチ平均のマイクロ秒）であり、`runtime_breakdown` と同じ流儀です。
use std::{error::Error, hint::black_box, time::Instant};
use storm_lua_spec::{draw::DrawCommand, screen::Rgba8};
use storm_screen_raster::raster::ScreenRaster;

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

fn digest(pixels: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in pixels {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn scenes() -> Vec<(&'static str, Vec<DrawCommand>, usize)> {
    let fox: &[u8] = b"The quick brown fox jumps over the lazy dog 0123456789";
    let fox_long: &[u8] =
        b"The quick brown fox jumps over the lazy dog 0123456789 and again and again and again";
    let mut out: Vec<(&'static str, Vec<DrawCommand>, usize)> = Vec::new();
    let mut push = |label: &'static str, commands: Vec<DrawCommand>, iterations: usize| {
        out.push((label, commands, iterations));
    };
    let rects = |alpha: u8| {
        let mut v = vec![DrawCommand::SetColor(Rgba8([10, 20, 30, alpha]))];
        for i in 0..50 {
            v.push(DrawCommand::Rect(
                [f64::from(i * 5), f64::from(i * 3), 60.0, 40.0],
                true,
            ));
        }
        v
    };
    push("rectF-opaque", rects(255), 2000);
    push("rectF-alpha", rects(128), 2000);
    let mut clear = vec![DrawCommand::SetColor(Rgba8([10, 20, 30, 255]))];
    for _ in 0..10 {
        clear.push(DrawCommand::Clear);
    }
    push("clear", clear, 2000);
    let mut circle_f = vec![DrawCommand::SetColor(Rgba8([10, 20, 30, 200]))];
    for i in 0..50 {
        circle_f.push(DrawCommand::Circle(
            [20.0 + f64::from(i * 5), 20.0 + f64::from(i * 2), 18.0],
            true,
        ));
    }
    push("circleF", circle_f, 2000);
    let mut triangle_f = vec![DrawCommand::SetColor(Rgba8([10, 20, 30, 200]))];
    for i in 0..50 {
        let x = f64::from(i * 5);
        let y = f64::from(i * 2);
        triangle_f.push(DrawCommand::Triangle(
            [[x, y], [x + 50.0, y + 10.0], [x + 20.0, y + 60.0]],
            true,
        ));
    }
    push("triangleF", triangle_f, 2000);
    let mut lines = vec![DrawCommand::SetColor(Rgba8([10, 20, 30, 255]))];
    for i in 0..100 {
        let x = f64::from(i) * 2.5;
        lines.push(DrawCommand::Line([[x, 0.0], [287.0 - x, 159.0]]));
    }
    push("line", lines, 2000);
    let mut rect_o = vec![DrawCommand::SetColor(Rgba8([10, 20, 30, 255]))];
    for i in 0..50 {
        rect_o.push(DrawCommand::Rect(
            [f64::from(i * 5), f64::from(i * 3), 60.0, 40.0],
            false,
        ));
    }
    push("rect", rect_o, 2000);
    let mut circle_o = vec![DrawCommand::SetColor(Rgba8([10, 20, 30, 255]))];
    for i in 0..50 {
        circle_o.push(DrawCommand::Circle(
            [20.0 + f64::from(i * 5), 20.0 + f64::from(i * 2), 18.0],
            false,
        ));
    }
    push("circle", circle_o, 2000);
    let mut text = vec![DrawCommand::SetColor(Rgba8([10, 20, 30, 255]))];
    for i in 0..25 {
        text.push(DrawCommand::Text([2.0, f64::from(i * 6)], fox.to_vec()));
    }
    push("text", text, 2000);
    let mut textbox = vec![DrawCommand::SetColor(Rgba8([10, 20, 30, 255]))];
    for _ in 0..10 {
        textbox.push(DrawCommand::TextBox(
            [0.0, 0.0, 288.0, 160.0, 0.0, 0.0],
            fox_long.to_vec(),
        ));
    }
    push("textBox", textbox, 2000);
    out
}

fn main() -> Result<(), Box<dyn Error>> {
    // 低速な計測ホストでは SLE_BENCH_SCALE（百分率）で反復を減らせます。
    let scale: usize = std::env::var("SLE_BENCH_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    let mut results = serde_json::Map::new();
    for (label, commands, iterations) in scenes() {
        let iterations = (iterations * scale / 100).max(50);
        let mut raster = ScreenRaster::new(288, 160)?;
        raster.begin_frame();
        raster.draw_batch(&commands)?;
        let hash = digest(raster.pixels());
        let micros = measure(
            || {
                raster.begin_frame();
                raster.draw_batch(&commands)?;
                black_box(raster.pixels());
                Ok(())
            },
            iterations,
        )?;
        results.insert(
            label.into(),
            serde_json::json!({"us": micros, "digest": format!("{hash:016x}")}),
        );
    }
    println!(
        "{}",
        serde_json::json!({"screen": [288, 160], "raster": results})
    );
    Ok(())
}
