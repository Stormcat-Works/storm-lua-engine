//! 画面契約に従うCPUラスタライザ。
//! Copyright (c) 2026 Shannon-Toppo. MIT許諾原文: licenses/screen-components-MIT.txt。
use crate::{blend::blend_pixel, font};
use std::rc::Rc;
use storm_lua_spec::map::{MapProvider, MapRequest};
use storm_lua_spec::{
    draw::{DrawCommand, ScreenError, ScreenSink},
    screen::{FrameView, PixelFormat, Rgba8},
};

/// 単体ラスタライザが許容する最大バッキングストレージ（16 MiB）。
pub const MAX_RASTER_BYTES: usize = 16 * 1024 * 1024;
/// 決定論的なCPUラスタライザ。フレーム開始時は透明な黒、現在の描画色は白になります。
pub struct ScreenRaster {
    width: u32,
    height: u32,
    margin: u32,
    pixels: Vec<u8>,
    color: Rgba8,
    map_provider: Option<Rc<dyn MapProvider>>,
    map_colors: [Option<Rgba8>; 8],
}
impl ScreenRaster {
    /// 境界付きフレームバッファを割り当てます。Luaバックエンドは不要です。
    pub fn new(width: u32, height: u32) -> Result<Self, ScreenError> {
        let len = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or(ScreenError::InvalidSize)?;
        if width == 0 || height == 0 || width > 4096 || height > 4096 || len > MAX_RASTER_BYTES {
            return Err(ScreenError::InvalidSize);
        }
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(len)
            .map_err(|_| ScreenError::LimitExceeded)?;
        pixels.resize(len, 0);
        Ok(Self {
            width,
            height,
            margin: 0,
            pixels,
            color: Rgba8([255; 4]),
            map_provider: None,
            map_colors: [None; 8],
        })
    }
    /// Development viewport with an integer overflow margin.
    pub fn with_margin(width: u32, height: u32, margin: u32) -> Result<Self, ScreenError> {
        let twice = margin.checked_mul(2).ok_or(ScreenError::InvalidSize)?;
        let mut raster = Self::new(
            width.checked_add(twice).ok_or(ScreenError::InvalidSize)?,
            height.checked_add(twice).ok_or(ScreenError::InvalidSize)?,
        )?;
        raster.margin = margin;
        Ok(raster)
    }
    /// Output overflow margin in pixels.
    pub fn margin(&self) -> u32 {
        self.margin
    }
    /// drawClear コマンドとは独立した、新規フレームの開始処理。
    pub fn begin_frame(&mut self) {
        self.pixels.fill(0);
        self.color = Rgba8([255; 4]);
        self.map_colors = [None; 8];
    }
    /// 事前乗算RGBAではなく、ソースアルファ規則に従うネイティブの生RGBAを参照します。
    pub fn frame(&self) -> FrameView<'_> {
        FrameView {
            width: self.width,
            height: self.height,
            stride_bytes: self.width as usize * 4,
            format: PixelFormat::GameRgba8,
            pixels: &self.pixels,
        }
    }
    /// 次のミュータブル操作が行われるまで、生のピクセルバイト列を参照します。
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
    /// フレームの寸法（幅・高さ）。
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    /// 順序付けられたバッチを実行します。無効なテキストやリソースエラーが報告されます。
    pub fn draw_batch(&mut self, commands: &[DrawCommand]) -> Result<(), ScreenError> {
        for command in commands {
            self.submit(command)?;
        }
        Ok(())
    }
    fn plot(&mut self, x: f64, y: f64) {
        if !x.is_finite()
            || !y.is_finite()
            || x < 0.0
            || y < 0.0
            || x >= f64::from(self.width)
            || y >= f64::from(self.height)
        {
            return;
        }
        let index = (y as usize * self.width as usize + x as usize) * 4;
        if self.color.0[3] == 255 {
            self.pixels[index..index + 4].copy_from_slice(&self.color.0);
        } else if self.color.0[3] != 0 {
            let d = Rgba8([
                self.pixels[index],
                self.pixels[index + 1],
                self.pixels[index + 2],
                self.pixels[index + 3],
            ]);
            self.pixels[index..index + 4].copy_from_slice(&blend_pixel(self.color, d).0);
        }
    }
    fn run(&mut self, left: f64, right: f64, y: f64) {
        if y < 0.0 || y >= f64::from(self.height) || !y.is_finite() {
            return;
        }
        let from = left.max(0.0);
        let to = right.min(f64::from(self.width));
        if !from.is_finite() || !to.is_finite() || to <= from {
            return;
        }
        for x in from as u32..to as u32 {
            self.plot(f64::from(x), y);
        }
    }
    fn line(&mut self, mut p: [[f64; 2]; 2]) {
        if !finite(p.iter().flatten().copied()) {
            return;
        }
        if p.iter().flatten().any(|v| v.abs() > 65536.0) {
            let [x, y] = p[0];
            let dx = p[1][0] - x;
            let dy = p[1][1] - y;
            if !dx.is_finite() || !dy.is_finite() {
                return;
            }
            let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
            for (a, b) in [
                (-dx, x + 64.0),
                (dx, f64::from(self.width) + 64.0 - x),
                (-dy, y + 64.0),
                (dy, f64::from(self.height) + 64.0 - y),
            ] {
                if a == 0.0 {
                    if b < 0.0 {
                        return;
                    }
                    continue;
                }
                let t = b / a;
                if a < 0.0 {
                    lo = lo.max(t);
                } else {
                    hi = hi.min(t);
                }
                if lo > hi {
                    return;
                }
            }
            p = [[x + dx * lo, y + dy * lo], [x + dx * hi, y + dy * hi]];
        }
        let (w, h) = (self.width, self.height);
        let [x1, y1] = [snap_units(p[0][0], w), snap_units(p[0][1], h)];
        let [x2, y2] = [snap_units(p[1][0], w), snap_units(p[1][1], h)];
        if x1 == x2 && y1 == y2 {
            return;
        }
        let xmajor = (x2 - x1).abs() >= (y2 - y1).abs();
        let corners: &[[f64; 2]] = if xmajor {
            &[[0.0, -128.0]]
        } else {
            &[[128.0, 0.0], [0.0, -128.0]]
        };
        let (a1, b1, a2, b2) = if xmajor {
            (x1, y1, x2, y2)
        } else {
            (y1, x1, y2, x2)
        };
        let sign = if a2 > a1 { 1.0 } else { -1.0 };
        let da = (a2 - a1) * sign;
        let db = (b2 - b1) * sign;
        let limit = f64::from(if xmajor { self.width } else { self.height }) - 1.0;
        let lo = ((a1.min(a2) / 256.0).floor() - 1.0).max(0.0);
        let hi = ((a1.max(a2) / 256.0).ceil() + 1.0).min(limit);
        if lo > hi {
            return;
        }
        for ip in lo as u32..=hi as u32 {
            let p = f64::from(ip);
            let n = b1 * da + (p * 256.0 - a1) * db;
            let q = if xmajor {
                floor_div(n + 128.0 * da, 256.0 * da)
            } else {
                -floor_div(-(n - 128.0 * da), 256.0 * da)
            };
            let (cx, cy) = if xmajor {
                (p * 256.0, q * 256.0)
            } else {
                (q * 256.0, p * 256.0)
            };
            let distance = (x2 - cx).abs() + (y2 - cy).abs();
            if distance < 128.0
                || (distance == 128.0
                    && corners
                        .iter()
                        .any(|[ox, oy]| x2 - cx == *ox && y2 - cy == *oy))
            {
                continue;
            }
            if meets([x1, y1, x2, y2], cx, cy)
                || corners
                    .iter()
                    .any(|[ox, oy]| on_segment([x1, y1, x2, y2], cx + ox, cy + oy))
            {
                if xmajor {
                    self.plot(p, q);
                } else {
                    self.plot(q, p);
                }
            }
        }
    }
    fn outline(&mut self, points: &[[f64; 2]]) {
        if !finite(points.iter().flatten().copied()) {
            return;
        }
        for i in 0..points.len() {
            self.line([points[i], points[(i + 1) % points.len()]]);
        }
    }
    fn convex(&mut self, points: &[[f64; 2]], offset: f64, ysign: f64) {
        if !finite(points.iter().flatten().copied()) {
            return;
        }
        let mut snapped = [[0.0; 2]; 16];
        for (out, point) in snapped.iter_mut().zip(points) {
            *out = [snap(point[0], self.width), snap(point[1], self.height)];
        }
        let pts = &snapped[..points.len()];
        if !finite(pts.iter().flatten().copied()) {
            return;
        }
        let ymin = pts.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min);
        let ymax = pts.iter().map(|p| p[1]).fold(f64::NEG_INFINITY, f64::max);
        let top = (ymin - offset).floor().max(0.0);
        let bottom = (ymax - offset).ceil().min(f64::from(self.height) - 1.0);
        if top > bottom {
            return;
        }
        for py in top as u32..=bottom as u32 {
            let y = f64::from(py) + offset;
            let (mut left, mut right) = (f64::INFINITY, f64::NEG_INFINITY);
            for i in 0..pts.len() {
                let [ax, ay] = pts[i];
                let [bx, by] = pts[(i + 1) % pts.len()];
                let low = ay.min(by);
                let high = ay.max(by);
                if low == high
                    || y < low
                    || y > high
                    || (y == low && ysign < 0.0)
                    || (y == high && ysign > 0.0)
                {
                    continue;
                }
                let x = ax + (bx - ax) * ((y - ay) / (by - ay));
                left = left.min(x);
                right = right.max(x);
            }
            if left < right {
                self.run(left.ceil(), right.ceil(), f64::from(py));
            }
        }
    }
    fn rectangle(&mut self, v: [f64; 4], fill: bool) {
        let [x, y, w, h] = v;
        if !finite(v) {
            return;
        }
        if !fill {
            self.outline(&[[x, y], [x + w, y], [x + w, y + h], [x, y + h]]);
            return;
        }
        let (x0, x1) = (snap(x, self.width), snap(x + w, self.width));
        let (y0, y1) = (snap(y, self.height), snap(y + h, self.height));
        if !finite([x0, x1, y0, y1]) {
            return;
        }
        let left = x0.min(x1).ceil();
        let right = x0.max(x1).ceil();
        let top = y0.min(y1).floor().max(0.0);
        let bottom = y0.max(y1).floor().min(f64::from(self.height));
        if top >= bottom {
            return;
        }
        for py in top as u32..bottom as u32 {
            self.run(left, right, f64::from(py));
        }
    }
    fn circle(&mut self, v: [f64; 3], fill: bool) {
        let [x, y, r] = v;
        if !finite(v) {
            return;
        }
        let radius = r.abs();
        let n = (radius / 2.0).floor().clamp(8.0, 16.0) as usize;
        // 実ゲームはfloat32のtauを分割数で割ったstepを0から逐次加算して周辺点を
        // 作ります（E24）。outlineは角度がtau以下の間、filledはtau未満の間点を
        // 追加するため、実際の点数は分割数とずれることがあります
        // （例: 分割数15ではoutline 16点・filled 15点、分割数10ではどちらも10点）。
        let tau: f32 = std::f32::consts::TAU;
        let step = tau / n as f32;
        let mut points: Vec<[f64; 2]> = Vec::new();
        let mut angle: f32 = 0.0;
        while if fill { angle < tau } else { angle <= tau } {
            let a = f64::from(angle);
            points.push([
                f64::from((x + radius * a.cos()) as f32),
                f64::from((y + radius * a.sin()) as f32),
            ]);
            angle += step;
        }
        if fill {
            self.convex(&points, 0.0, -1.0);
        } else {
            self.outline(&points);
        }
    }
    /// 1文字分のglyphを指定位置へ描きます。
    fn draw_glyph(&mut self, ch: char, x: f64, y: f64) {
        let rows = font::glyph_for(ch);
        for (row, bits) in rows.iter().enumerate() {
            for col in 0..4 {
                if bits & (1 << (3 - col)) != 0 {
                    self.plot(x + f64::from(col), y + row as f64);
                }
            }
        }
    }
    fn text(&mut self, position: [f64; 2], text: &str) {
        if !finite(position) {
            return;
        }
        let ox = position[0].floor();
        let (mut x, mut y) = (ox, position[1].floor());
        // 実ゲームのdrawTextは文字列をbyte単位でglyph IDとして照合します
        // （UTF-8のdecodeはしません。E21）。対応glyphがないbyteは横送りも
        // しません。空白は5送り、改行はXを開始位置へ戻してYを6進めます。
        // glyphが1個もない文字列は頂点を生成しません。
        for byte in text.bytes() {
            match byte {
                b'\n' => {
                    x = ox;
                    y += 6.0;
                }
                32..=126 => {
                    self.draw_glyph(char::from(byte), x, y);
                    x += 5.0;
                }
                _ => {}
            }
        }
    }
    fn textbox(&mut self, v: [f64; 6], text: &str) {
        if !finite(v) {
            return;
        }
        let [x, y, w, h, ha, va] = v;
        // 実ゲームのdrawTextBoxはUTF-8をdecodeし、glyphがあるcodepoint
        // （32〜126）だけをトークンとして残します（E22）。対応しないcodepointは
        // 送り幅も生じません。LFは現在行の末尾トークンになる改行、TABは空白
        // glyphの幅・送りを4倍にしたトークンです。
        struct Token {
            glyph: char,
            advance: f64,
            space: bool,
            line_feed: bool,
        }
        let mut tokens: Vec<Token> = Vec::new();
        for ch in text.chars() {
            match ch {
                '\n' => tokens.push(Token {
                    glyph: ' ',
                    advance: 5.0,
                    space: false,
                    line_feed: true,
                }),
                '\t' => tokens.push(Token {
                    glyph: ' ',
                    advance: 20.0,
                    space: false,
                    line_feed: false,
                }),
                ' '..='~' => tokens.push(Token {
                    glyph: ch,
                    advance: 5.0,
                    space: ch == ' ',
                    line_feed: false,
                }),
                _ => {}
            }
        }
        // 行分割はトークン単位です。送り幅の合計が指定widthを超え、かつ行に
        // 2トークン以上あるとき戻し処理へ入ります（ちょうど等しい場合は
        // 折り返しません）。戻す場合も現在行に最低1トークンは残します。
        let mut lines: Vec<Vec<Token>> = Vec::new();
        let mut line: Vec<Token> = Vec::new();
        let mut line_width = 0.0_f64;
        for token in tokens {
            if token.line_feed {
                line.push(token);
                lines.push(std::mem::take(&mut line));
                line_width = 0.0;
                continue;
            }
            if line_width + token.advance > w && line.len() >= 2 {
                if let Some(space) = line.iter().rposition(|t| t.space) {
                    let carry: Vec<Token> = line.drain(space + 1..).collect();
                    lines.push(std::mem::take(&mut line));
                    line_width = carry.iter().map(|t| t.advance).sum();
                    line = carry;
                } else {
                    lines.push(std::mem::take(&mut line));
                    line_width = 0.0;
                }
            }
            line_width += token.advance;
            line.push(token);
        }
        lines.push(line);
        // 配置に使う全行高さは (行数 − 1) × 6 + 5、行の配置用幅は
        // (トークン個数 − 1) × 5 + 4 です（折返し判定の送り幅合計とは別です）。
        let total_height = (lines.len() as f64 - 1.0) * 6.0 + 5.0;
        let top = if va < 0.0 {
            y
        } else if va > 0.0 {
            y + h - total_height
        } else {
            y + (h - total_height) / 2.0
        };
        for (i, line) in lines.iter().enumerate() {
            let layout_width = if line.is_empty() {
                0.0
            } else {
                (line.len() as f64 - 1.0) * 5.0 + 4.0
            };
            let left = if ha < 0.0 {
                x
            } else if ha > 0.0 {
                x + w - layout_width
            } else {
                x + (w - layout_width) / 2.0
            };
            let mut cursor = left.floor();
            let row_y = top.floor() + i as f64 * 6.0;
            for token in line {
                if token.glyph != ' ' {
                    self.draw_glyph(token.glyph, cursor, row_y);
                }
                cursor += token.advance;
            }
        }
    }
}
impl ScreenSink for ScreenRaster {
    fn submit(&mut self, command: &DrawCommand) -> Result<(), ScreenError> {
        if command.text_bytes() > 1024 * 1024 {
            return Err(ScreenError::LimitExceeded);
        }
        let offset = f64::from(self.margin);
        let translated;
        let command = if self.margin == 0 {
            command
        } else {
            translated = match command {
                DrawCommand::Line(p) => DrawCommand::Line(p.map(|[x, y]| [x + offset, y + offset])),
                DrawCommand::Rect(v, fill) => {
                    let mut v = *v;
                    v[0] += offset;
                    v[1] += offset;
                    DrawCommand::Rect(v, *fill)
                }
                DrawCommand::Circle(v, fill) => {
                    let mut v = *v;
                    v[0] += offset;
                    v[1] += offset;
                    DrawCommand::Circle(v, *fill)
                }
                DrawCommand::Triangle(p, fill) => {
                    DrawCommand::Triangle(p.map(|[x, y]| [x + offset, y + offset]), *fill)
                }
                DrawCommand::Text(p, text) => {
                    DrawCommand::Text([p[0] + offset, p[1] + offset], text.clone())
                }
                DrawCommand::TextBox(v, text) => {
                    let mut v = *v;
                    v[0] += offset;
                    v[1] += offset;
                    DrawCommand::TextBox(v, text.clone())
                }
                other => other.clone(),
            };
            &translated
        };
        match command {
            DrawCommand::Map(coordinates) => self.draw_map(*coordinates)?,
            DrawCommand::MapColor(kind, color) => self.map_colors[*kind as usize] = Some(*color),
            DrawCommand::SetColor(color) => self.color = *color,
            DrawCommand::Clear => {
                for pixel in self.pixels.chunks_exact_mut(4) {
                    pixel.copy_from_slice(&self.color.0);
                }
            }
            DrawCommand::Line(p) => self.line(*p),
            DrawCommand::Rect(v, fill) => self.rectangle(*v, *fill),
            DrawCommand::Circle(v, fill) => self.circle(*v, *fill),
            DrawCommand::Triangle(p, fill) => {
                if *fill {
                    // 実ゲームのwrapperは塗り三角形の各頂点を (f(x), f(f(y) − 1))
                    // へ移してから渡します（E23）。従来のY sample offsetによる
                    // 近似から、頂点生成規則そのものへ置き換えます。全743ケースの
                    // 採用画素は不変であることを契約テストで確認済みです。
                    let shifted =
                        p.map(|[px, py]| [f64::from(px as f32), f64::from((py as f32) - 1.0)]);
                    self.convex(&shifted, 0.0, -1.0);
                } else {
                    self.outline(p);
                }
            }
            DrawCommand::Text(p, text) => self.text(
                *p,
                std::str::from_utf8(text).map_err(|_| ScreenError::InvalidText)?,
            ),
            DrawCommand::TextBox(v, text) => self.textbox(
                *v,
                std::str::from_utf8(text).map_err(|_| ScreenError::InvalidText)?,
            ),
        }
        Ok(())
    }
}
fn finite(values: impl IntoIterator<Item = f64>) -> bool {
    values.into_iter().all(f64::is_finite)
}
fn js_round(v: f64) -> f64 {
    let floor = v.floor();
    if v - floor < 0.5 {
        floor
    } else {
        floor + 1.0
    }
}
/// 1/256px単位の頂点スナップ。`size`はxなら画面幅、yなら画面高さです。
///
/// 格子点どうしのちょうど中間（k+1/512）に乗った値は上下の格子点までの距離が等しく、
/// スクリプト座標上の丸め規則ではなく、画面へ届くまでのf32演算で丸める向きが決まります。
/// 投影（半画素オフセットを含む）、viewportの乗算と加算、最近接偶数丸めの固定小数点化を
/// 順に再現します。同じ値でも画面の大きさで向きが変わります
/// （幅64では59+1/512が下へ、幅96では上へ丸まります）。
/// f32へ収まらない値は画面から遠く、丸める向きが結果に影響しないため従来の丸めを使います。
fn snap_units(v: f64, size: u32) -> f64 {
    // 辺長は4096以下なのでf32で正確です。
    let size = size as f32;
    let scale = 2.0_f32 / size;
    let half = size / 2.0;
    let ndc = (v as f32) * scale + (-1.0 + 0.5 * scale);
    let screen = ndc * half + half;
    if !screen.is_finite() {
        return js_round(v * 256.0);
    }
    // f32の値を256倍して128を引く演算はf64で正確です。
    let units = f64::from(screen) * 256.0 - 128.0;
    let low = units.floor();
    let fraction = units - low;
    if fraction > 0.5 || (fraction == 0.5 && low % 2.0 != 0.0) {
        low + 1.0
    } else {
        low
    }
}
fn snap(v: f64, size: u32) -> f64 {
    snap_units(v, size) / 256.0
}
fn floor_div(n: f64, d: f64) -> f64 {
    let remainder = ((n % d) + d) % d;
    (n - remainder) / d
}
fn on_segment([x1, y1, x2, y2]: [f64; 4], px: f64, py: f64) -> bool {
    (x2 - x1) * (py - y1) == (y2 - y1) * (px - x1)
        && px >= x1.min(x2)
        && px <= x1.max(x2)
        && py >= y1.min(y2)
        && py <= y1.max(y2)
}
fn meets([x1, y1, x2, y2]: [f64; 4], cx: f64, cy: f64) -> bool {
    let dx = x2 - x1;
    let dy = y2 - y1;
    let (mut ln, mut ld, mut hn, mut hd, mut lopen, mut hopen) = (0.0, 1.0, 1.0, 1.0, false, false);
    for (sx, sy) in [(1.0, 1.0), (1.0, -1.0), (-1.0, 1.0), (-1.0, -1.0)] {
        let a = sx * (x1 - cx) + sy * (y1 - cy);
        let b = sx * dx + sy * dy;
        if b == 0.0 {
            if a >= 128.0 {
                return false;
            }
            continue;
        }
        let n = if b > 0.0 { 128.0 - a } else { a - 128.0 };
        let d = b.abs();
        if b > 0.0 {
            if n * hd <= hn * d {
                hn = n;
                hd = d;
                hopen = true;
            }
        } else if n * ld >= ln * d {
            ln = n;
            ld = d;
            lopen = true;
        }
    }
    let order = ln * hd - hn * ld;
    order < 0.0 || (order == 0.0 && !lopen && !hopen)
}

impl ScreenRaster {
    /// 任意のホスト地形プロバイダを差し替えます。通常のジオメトリ描画はプロバイダから独立しています。
    pub fn set_map_provider(&mut self, provider: Option<Rc<dyn MapProvider>>) {
        self.map_provider = provider;
    }
    fn draw_map(&mut self, coordinates: [f64; 3]) -> Result<(), ScreenError> {
        if !coordinates.iter().all(|v| v.is_finite()) {
            return Err(ScreenError::InvalidCommand);
        }
        let provider = self
            .map_provider
            .as_ref()
            .ok_or(ScreenError::MissingMapProvider)?;
        let request = MapRequest {
            width: self.width - self.margin * 2,
            height: self.height - self.margin * 2,
            center: [coordinates[0], coordinates[1]],
            zoom: coordinates[2],
            colors: self.map_colors,
        };
        let pixels = provider.render(&request)?;
        if pixels.len() != request.width as usize * request.height as usize * 4 {
            return Err(ScreenError::Host(
                "map provider returned an invalid RGBA byte length".into(),
            ));
        }
        if self.margin == 0 {
            self.pixels.copy_from_slice(&pixels);
        } else {
            let stride = request.width as usize * 4;
            for y in 0..request.height as usize {
                let dst =
                    ((y + self.margin as usize) * self.width as usize + self.margin as usize) * 4;
                self.pixels[dst..dst + stride]
                    .copy_from_slice(&pixels[y * stride..(y + 1) * stride]);
            }
        }
        Ok(())
    }
}
impl std::fmt::Debug for ScreenRaster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScreenRaster")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("map_provider", &self.map_provider.is_some())
            .finish_non_exhaustive()
    }
}
