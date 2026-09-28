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
            pixels,
            color: Rgba8([255; 4]),
            map_provider: None,
            map_colors: [None; 8],
        })
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
        let mut points = [[0.0; 2]; 16];
        for (i, p) in points.iter_mut().enumerate().take(n) {
            let a = 2.0 * std::f64::consts::PI * i as f64 / n as f64;
            *p = [
                f64::from((x + radius * a.cos()) as f32),
                f64::from((y + radius * a.sin()) as f32),
            ];
        }
        if fill {
            self.convex(&points[..n], 0.0, -1.0);
        } else {
            self.outline(&points[..n]);
        }
    }
    fn text(&mut self, position: [f64; 2], text: &str) {
        if !finite(position) {
            return;
        }
        let ox = position[0].floor();
        let (mut x, mut y) = (ox, position[1].floor());
        for ch in text.chars() {
            if ch == '\n' {
                x = ox;
                y += 6.0;
                continue;
            }
            let rows = font::glyph_for(ch);
            for (row, bits) in rows.iter().enumerate() {
                for col in 0..4 {
                    if bits & (1 << (3 - col)) != 0 {
                        self.plot(x + f64::from(col), y + row as f64);
                    }
                }
            }
            x += 5.0;
        }
    }
    fn textbox(&mut self, v: [f64; 6], text: &str) {
        if !finite(v) {
            return;
        }
        let [x, y, w, h, ha, va] = v;
        let cap = (w / 5.0).floor().max(1.0) as usize;
        // JSの文字列スライスと文字長は、UTF-8バイトではなくUTF-16コードユニットを基準とします。
        let units: Vec<u16> = text.encode_utf16().collect();
        let mut lines: Vec<&[u16]> = Vec::new();
        for paragraph in units.split(|u| *u == 10) {
            let mut pos: usize = 0;
            loop {
                let mut end = pos.saturating_add(cap).min(paragraph.len());
                if end < paragraph.len() && paragraph[end] != 32 {
                    if let Some(space) = paragraph[pos..end].iter().rposition(|u| *u == 32) {
                        end = pos + space + 1;
                    }
                }
                lines.push(&paragraph[pos..end]);
                pos = end;
                if pos >= paragraph.len() {
                    break;
                }
            }
        }
        let height = lines.len() as f64 * 6.0 - 1.0;
        let top = if va < 0.0 {
            y
        } else if va > 0.0 {
            y + h - height
        } else {
            y + (h - height) / 2.0
        };
        for (i, line) in lines.iter().enumerate() {
            let width = if line.is_empty() {
                0.0
            } else {
                line.len() as f64 * 5.0 - 1.0
            };
            let left = if ha < 0.0 {
                x
            } else if ha > 0.0 {
                x + w - width
            } else {
                x + (w - width) / 2.0
            };
            let decoded: String = char::decode_utf16(line.iter().copied())
                .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect();
            self.text([left.floor(), top.floor() + i as f64 * 6.0], &decoded);
        }
    }
}
impl ScreenSink for ScreenRaster {
    fn submit(&mut self, command: &DrawCommand) -> Result<(), ScreenError> {
        if command.text_bytes() > 1024 * 1024 {
            return Err(ScreenError::LimitExceeded);
        }
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
                    self.convex(p, 1.0, -1.0);
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
            width: self.width,
            height: self.height,
            center: [coordinates[0], coordinates[1]],
            zoom: coordinates[2],
            colors: self.map_colors,
        };
        let pixels = provider.render(&request)?;
        if pixels.len() != self.pixels.len() {
            return Err(ScreenError::Host(
                "map provider returned an invalid RGBA byte length".into(),
            ));
        }
        self.pixels.copy_from_slice(&pixels);
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
