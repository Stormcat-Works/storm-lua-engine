//! 画面契約に従うCPUラスタライザ。
//! Copyright (c) 2026 Shannon-Toppo. MIT許諾原文: licenses/screen-components-MIT.txt。
use crate::{blend::PreparedBlend, font};
use std::sync::Arc;
use storm_lua_spec::map::{MapProvider, MapRequest};
use storm_lua_spec::{
    draw::{DrawCommand, ScreenError, ScreenSink},
    screen::{FrameView, PixelFormat, Rgba8},
};

/// 単体ラスタライザが許容する最大バッキングストレージ（16 MiB）。
pub const MAX_RASTER_BYTES: usize = 16 * 1024 * 1024;

#[inline]
fn fill_opaque_slice(slice: &mut [u8], rgba: [u8; 4]) {
    let (pixels, _) = slice.as_chunks_mut::<4>();
    pixels.fill(rgba);
}

#[inline]
fn blend_slice(slice: &mut [u8], blend: PreparedBlend) {
    let (pixels, _) = slice.as_chunks_mut::<4>();
    for px in pixels {
        blend.apply(px);
    }
}

/// 決定論的なCPUラスタライザ。フレーム開始時は透明な黒、現在の描画色は白になります。
pub struct ScreenRaster {
    width: u32,
    height: u32,
    margin: u32,
    pixels: Vec<u8>,
    color: Rgba8,
    map_provider: Option<Arc<dyn MapProvider + Send + Sync>>,
    map_colors: [Option<Rgba8>; 8],
    /// 差分テスト専用。行スパンを1画素ずつ描き、床除算を剰余2回で求める参照経路を選びます。
    #[cfg(test)]
    reference_paths: bool,
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
            #[cfg(test)]
            reference_paths: false,
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
    #[inline]
    fn plot_with_blend(&mut self, x: f64, y: f64, blend: Option<PreparedBlend>) {
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
        match blend {
            None => self.pixels[index..index + 4].copy_from_slice(&self.color.0),
            Some(prep) => {
                if let Ok(dst) = (&mut self.pixels[index..index + 4]).try_into() {
                    prep.apply(dst);
                }
            }
        }
    }
    #[inline]
    fn run_row(&mut self, left: f64, right: f64, py: u32, blend: Option<PreparedBlend>) {
        let from = left.max(0.0);
        let to = right.min(f64::from(self.width));
        if !from.is_finite() || !to.is_finite() || to <= from {
            return;
        }
        let x0 = from as usize;
        let x1 = to as usize;
        if x0 >= x1 {
            return;
        }
        #[cfg(test)]
        if self.reference_paths {
            for x in x0..x1 {
                self.plot_with_blend(f64::from(x as u32), f64::from(py), blend);
            }
            return;
        }
        let row = py as usize * self.width as usize;
        let slice = &mut self.pixels[(row + x0) * 4..(row + x1) * 4];
        match blend {
            None => fill_opaque_slice(slice, self.color.0),
            Some(prep) => blend_slice(slice, prep),
        }
    }
    fn line(&mut self, mut p: [[f64; 2]; 2]) {
        let alpha = self.color.0[3];
        if alpha == 0 || !finite(p.iter().flatten().copied()) {
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
        #[cfg(test)]
        let floor_div = if self.reference_paths {
            floor_div_reference
        } else {
            floor_div
        };
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
        let blend = (alpha < 255).then(|| PreparedBlend::new(self.color));
        let seg = [x1, y1, x2, y2];
        let da_128 = 128.0 * da;
        let da_256 = 256.0 * da;
        for ip in lo as u32..=hi as u32 {
            let p = f64::from(ip);
            let n = b1 * da + (p * 256.0 - a1) * db;
            let q = if xmajor {
                floor_div(n + da_128, da_256)
            } else {
                -floor_div(-(n - da_128), da_256)
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
            if meets(seg, cx, cy)
                || corners
                    .iter()
                    .any(|[ox, oy]| on_segment(seg, cx + ox, cy + oy))
            {
                if xmajor {
                    self.plot_with_blend(p, q, blend);
                } else {
                    self.plot_with_blend(q, p, blend);
                }
            }
        }
    }
    fn outline(&mut self, points: &[[f64; 2]]) {
        if self.color.0[3] == 0 || !finite(points.iter().flatten().copied()) {
            return;
        }
        let n = points.len();
        for i in 0..n {
            let next = if i + 1 == n { 0 } else { i + 1 };
            self.line([points[i], points[next]]);
        }
    }
    fn convex(&mut self, points: &[[f64; 2]], offset: f64, ysign: f64) {
        let alpha = self.color.0[3];
        if alpha == 0 || !finite(points.iter().flatten().copied()) {
            return;
        }
        let mut snapped = [[0.0; 2]; 20];
        for (out, point) in snapped.iter_mut().zip(points) {
            *out = [snap(point[0], self.width), snap(point[1], self.height)];
        }
        let n = points.len().min(snapped.len());
        let pts = &snapped[..n];
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
        let blend = (alpha < 255).then(|| PreparedBlend::new(self.color));
        for py in top as u32..=bottom as u32 {
            let y = f64::from(py) + offset;
            let (mut left, mut right) = (f64::INFINITY, f64::NEG_INFINITY);
            for i in 0..n {
                let [ax, ay] = pts[i];
                let next = if i + 1 == n { 0 } else { i + 1 };
                let [bx, by] = pts[next];
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
                self.run_row(left.ceil(), right.ceil(), py, blend);
            }
        }
    }
    fn rectangle(&mut self, v: [f64; 4], fill: bool) {
        let alpha = self.color.0[3];
        if alpha == 0 {
            return;
        }
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
        let from = left.max(0.0);
        let to = right.min(f64::from(self.width));
        if !from.is_finite() || !to.is_finite() || to <= from {
            return;
        }
        let (ix0, ix1) = (from as usize, to as usize);
        if ix0 >= ix1 {
            return;
        }
        let (iy0, iy1) = (top as usize, bottom as usize);
        let stride = self.width as usize;
        if alpha == 255 {
            let rgba = self.color.0;
            if ix0 == 0 && ix1 == stride {
                fill_opaque_slice(&mut self.pixels[iy0 * stride * 4..iy1 * stride * 4], rgba);
            } else {
                for py in iy0..iy1 {
                    let row = py * stride;
                    fill_opaque_slice(&mut self.pixels[(row + ix0) * 4..(row + ix1) * 4], rgba);
                }
            }
        } else {
            let blend = PreparedBlend::new(self.color);
            if ix0 == 0 && ix1 == stride {
                blend_slice(&mut self.pixels[iy0 * stride * 4..iy1 * stride * 4], blend);
            } else {
                for py in iy0..iy1 {
                    let row = py * stride;
                    blend_slice(&mut self.pixels[(row + ix0) * 4..(row + ix1) * 4], blend);
                }
            }
        }
    }
    fn circle(&mut self, v: [f64; 3], fill: bool) {
        if self.color.0[3] == 0 {
            return;
        }
        let [x, y, r] = v;
        if !finite(v) {
            return;
        }
        let radius = r.abs();
        let n = (radius / 2.0).floor().clamp(8.0, 16.0) as usize;
        // 実ゲームはfloat32のtauを分割数で割ったstepを0から逐次加算して周辺点を
        // 作ります（E24）。outlineは角度がtau以下の間、filledはtau未満の間点を
        // 追加するため、実際の点数は分割数とずれることがあります。
        let tau: f32 = std::f32::consts::TAU;
        let step = tau / n as f32;
        let mut points = [[0.0_f64; 2]; 20];
        let mut count = 0_usize;
        let mut angle: f32 = 0.0;
        while (if fill { angle < tau } else { angle <= tau }) && count < points.len() {
            let a = f64::from(angle);
            points[count] = [
                f64::from((x + radius * a.cos()) as f32),
                f64::from((y + radius * a.sin()) as f32),
            ];
            count += 1;
            angle += step;
        }
        if fill {
            self.convex(&points[..count], 0.0, -1.0);
        } else {
            self.outline(&points[..count]);
        }
    }
    #[inline]
    fn blit_glyph(&mut self, ch: char, x: f64, y: f64, blend: Option<PreparedBlend>) {
        if y + 5.0 <= 0.0
            || y >= f64::from(self.height)
            || x + 4.0 <= 0.0
            || x >= f64::from(self.width)
        {
            return;
        }
        let ix = x as i32;
        let iy = y as i32;
        let w = self.width as usize;
        let rgba = self.color.0;
        let rows = font::glyph_for(ch);
        if ix >= 0 && (ix as u32) + 4 <= self.width && iy >= 0 && (iy as u32) + 5 <= self.height {
            let ux = ix as usize;
            let uy = iy as usize;
            match blend {
                None => {
                    for (row, &bits) in rows.iter().enumerate() {
                        if bits == 0 {
                            continue;
                        }
                        let base = ((uy + row) * w + ux) * 4;
                        if bits & 8 != 0 {
                            self.pixels[base..base + 4].copy_from_slice(&rgba);
                        }
                        if bits & 4 != 0 {
                            self.pixels[base + 4..base + 8].copy_from_slice(&rgba);
                        }
                        if bits & 2 != 0 {
                            self.pixels[base + 8..base + 12].copy_from_slice(&rgba);
                        }
                        if bits & 1 != 0 {
                            self.pixels[base + 12..base + 16].copy_from_slice(&rgba);
                        }
                    }
                }
                Some(prep) => {
                    for (row, &bits) in rows.iter().enumerate() {
                        if bits == 0 {
                            continue;
                        }
                        let base = ((uy + row) * w + ux) * 4;
                        for col in 0..4_usize {
                            if bits & (1 << (3 - col)) != 0 {
                                let idx = base + col * 4;
                                if let Ok(dst) = (&mut self.pixels[idx..idx + 4]).try_into() {
                                    prep.apply(dst);
                                }
                            }
                        }
                    }
                }
            }
        } else {
            let w_i = self.width as i32;
            let h_i = self.height as i32;
            for (row, &bits) in rows.iter().enumerate() {
                let py = iy + row as i32;
                if py < 0 || py >= h_i || bits == 0 {
                    continue;
                }
                for col in 0..4_i32 {
                    if bits & (1 << (3 - col)) != 0 {
                        let px = ix + col;
                        if px >= 0 && px < w_i {
                            let idx = (py as usize * w + px as usize) * 4;
                            match blend {
                                None => self.pixels[idx..idx + 4].copy_from_slice(&rgba),
                                Some(prep) => {
                                    if let Ok(dst) = (&mut self.pixels[idx..idx + 4]).try_into() {
                                        prep.apply(dst);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    fn text(&mut self, position: [f64; 2], text: &str) {
        let alpha = self.color.0[3];
        if alpha == 0 || !finite(position) {
            return;
        }
        let ox = position[0].floor();
        let (mut x, mut y) = (ox, position[1].floor());
        let h_f = f64::from(self.height);
        let blend = (alpha < 255).then(|| PreparedBlend::new(self.color));
        // 実ゲームのdrawTextは文字列をbyte単位でglyph IDとして照合します（E21）。
        for byte in text.bytes() {
            match byte {
                b'\n' => {
                    x = ox;
                    y += 6.0;
                    if y >= h_f {
                        break;
                    }
                }
                32..=126 => {
                    if y >= h_f {
                        break;
                    }
                    self.blit_glyph(char::from(byte), x, y, blend);
                    x += 5.0;
                }
                _ => {}
            }
        }
    }
    fn textbox(&mut self, v: [f64; 6], text: &str) {
        let alpha = self.color.0[3];
        if alpha == 0 || !finite(v) {
            return;
        }
        let [x, y, w, h, ha, va] = v;
        #[derive(Clone, Copy)]
        struct Token {
            glyph: char,
            advance: f64,
            space: bool,
            line_feed: bool,
        }
        const EMPTY_TOKEN: Token = Token {
            glyph: ' ',
            advance: 0.0,
            space: false,
            line_feed: false,
        };
        let mut render_tokens = |tokens: &[Token]| {
            let mut line_ranges = [(0_usize, 0_usize); 64];
            let mut overflow_ranges: Vec<(usize, usize)> = Vec::new();
            let mut range_count = 0_usize;
            let mut push_range = |start: usize, end: usize| {
                if overflow_ranges.is_empty() && range_count < line_ranges.len() {
                    line_ranges[range_count] = (start, end);
                    range_count += 1;
                } else {
                    if overflow_ranges.is_empty() {
                        overflow_ranges.extend_from_slice(&line_ranges[..range_count]);
                    }
                    overflow_ranges.push((start, end));
                }
            };
            let mut line_start = 0_usize;
            let mut line_width = 0.0_f64;
            let mut idx = 0_usize;
            while idx < tokens.len() {
                let token = tokens[idx];
                if token.line_feed {
                    push_range(line_start, idx + 1);
                    line_start = idx + 1;
                    line_width = 0.0;
                    idx += 1;
                    continue;
                }
                let cur_len = idx - line_start;
                if line_width + token.advance > w && cur_len >= 2 {
                    if let Some(rel_space) = tokens[line_start..idx].iter().rposition(|t| t.space) {
                        let split_at = line_start + rel_space + 1;
                        push_range(line_start, split_at);
                        line_start = split_at;
                        line_width = tokens[line_start..idx].iter().map(|t| t.advance).sum();
                    } else {
                        push_range(line_start, idx);
                        line_start = idx;
                        line_width = 0.0;
                    }
                }
                line_width += token.advance;
                idx += 1;
            }
            push_range(line_start, tokens.len());
            let ranges = if overflow_ranges.is_empty() {
                &line_ranges[..range_count]
            } else {
                overflow_ranges.as_slice()
            };
            let total_height = (ranges.len() as f64 - 1.0) * 6.0 + 5.0;
            let top = if va < 0.0 {
                y
            } else if va > 0.0 {
                y + h - total_height
            } else {
                y + (h - total_height) / 2.0
            };
            let top_fl = top.floor();
            let blend = (alpha < 255).then(|| PreparedBlend::new(self.color));
            for (i, &(start, end)) in ranges.iter().enumerate() {
                let line = &tokens[start..end];
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
                let row_y = top_fl + i as f64 * 6.0;
                for token in line {
                    if token.glyph != ' ' {
                        self.blit_glyph(token.glyph, cursor, row_y, blend);
                    }
                    cursor += token.advance;
                }
            }
        };

        if text.len() <= 256 {
            let mut stack_tokens = [EMPTY_TOKEN; 256];
            let mut len = 0_usize;
            for ch in text.chars() {
                let tok = match ch {
                    '\n' => Some(Token {
                        glyph: ' ',
                        advance: 5.0,
                        space: false,
                        line_feed: true,
                    }),
                    '\t' => Some(Token {
                        glyph: ' ',
                        advance: 20.0,
                        space: false,
                        line_feed: false,
                    }),
                    ' '..='~' => Some(Token {
                        glyph: ch,
                        advance: 5.0,
                        space: ch == ' ',
                        line_feed: false,
                    }),
                    _ => None,
                };
                if let Some(tok) = tok {
                    stack_tokens[len] = tok;
                    len += 1;
                }
            }
            render_tokens(&stack_tokens[..len]);
        } else {
            let mut heap_tokens: Vec<Token> = Vec::with_capacity(text.len());
            for ch in text.chars() {
                match ch {
                    '\n' => heap_tokens.push(Token {
                        glyph: ' ',
                        advance: 5.0,
                        space: false,
                        line_feed: true,
                    }),
                    '\t' => heap_tokens.push(Token {
                        glyph: ' ',
                        advance: 20.0,
                        space: false,
                        line_feed: false,
                    }),
                    ' '..='~' => heap_tokens.push(Token {
                        glyph: ch,
                        advance: 5.0,
                        space: ch == ' ',
                        line_feed: false,
                    }),
                    _ => {}
                }
            }
            render_tokens(&heap_tokens);
        }
    }
}
impl ScreenSink for ScreenRaster {
    fn submit(&mut self, command: &DrawCommand) -> Result<(), ScreenError> {
        if command.text_bytes() > 1024 * 1024 {
            return Err(ScreenError::LimitExceeded);
        }
        let offset = f64::from(self.margin);
        match command {
            DrawCommand::Map(coordinates) => self.draw_map(*coordinates)?,
            DrawCommand::MapColor(kind, color) => self.map_colors[*kind as usize] = Some(*color),
            DrawCommand::SetColor(color) => self.color = *color,
            DrawCommand::Clear => fill_opaque_slice(&mut self.pixels, self.color.0),
            DrawCommand::Line(p) => {
                if self.margin == 0 {
                    self.line(*p);
                } else {
                    self.line(p.map(|[x, y]| [x + offset, y + offset]));
                }
            }
            DrawCommand::Rect(v, fill) => {
                let mut v = *v;
                if self.margin != 0 {
                    v[0] += offset;
                    v[1] += offset;
                }
                self.rectangle(v, *fill);
            }
            DrawCommand::Circle(v, fill) => {
                let mut v = *v;
                if self.margin != 0 {
                    v[0] += offset;
                    v[1] += offset;
                }
                self.circle(v, *fill);
            }
            DrawCommand::Triangle(p, fill) => {
                let p = if self.margin == 0 {
                    *p
                } else {
                    p.map(|[x, y]| [x + offset, y + offset])
                };
                if *fill {
                    let shifted =
                        p.map(|[px, py]| [f64::from(px as f32), f64::from((py as f32) - 1.0)]);
                    self.convex(&shifted, 0.0, -1.0);
                } else {
                    self.outline(&p);
                }
            }
            DrawCommand::Text(p, text) => {
                let text = std::str::from_utf8(text).map_err(|_| ScreenError::InvalidText)?;
                let p = if self.margin == 0 {
                    *p
                } else {
                    [p[0] + offset, p[1] + offset]
                };
                self.text(p, text);
            }
            DrawCommand::TextBox(v, text) => {
                let text = std::str::from_utf8(text).map_err(|_| ScreenError::InvalidText)?;
                let mut v = *v;
                if self.margin != 0 {
                    v[0] += offset;
                    v[1] += offset;
                }
                self.textbox(v, text);
            }
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
fn snap_units(v: f64, size: u32) -> f64 {
    let size = size as f32;
    let scale = 2.0_f32 / size;
    let half = size / 2.0;
    let ndc = (v as f32) * scale + (-1.0 + 0.5 * scale);
    let screen = ndc * half + half;
    if !screen.is_finite() {
        return js_round(v * 256.0);
    }
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
/// 整数除算で床除算を正確に求められる被演算数の絶対値の上限（2^52）。
const EXACT_INTEGER_LIMIT: f64 = 4_503_599_627_370_496.0;
/// 負の無限大方向へ丸める除算。直線の画素ごとに呼ばれます。
///
/// 直線の被演算数は1/256px単位の整数値なので、通常は整数除算で求めます。両方が2^52以下の
/// 整数値でdが正なら、剰余2回の式も全段が丸めなしで計算され、結果は同じ値になります。
/// それ以外（巨大座標の代替丸め、nがゼロで符号を保つ場合など）は元の式をそのまま使います。
fn floor_div(n: f64, d: f64) -> f64 {
    if d > 0.0
        && d <= EXACT_INTEGER_LIMIT
        && n != 0.0
        && n.abs() <= EXACT_INTEGER_LIMIT
        && n.fract() == 0.0
        && d.fract() == 0.0
    {
        return (n as i64).div_euclid(d as i64) as f64;
    }
    floor_div_reference(n, d)
}
fn floor_div_reference(n: f64, d: f64) -> f64 {
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
    pub fn set_map_provider(&mut self, provider: Option<Arc<dyn MapProvider + Send + Sync>>) {
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
/// Owned continuation data for one raster: dimensions, margin, the full
/// pixel buffer, the current draw colour and the map colour slots. The
/// host terrain map provider is a process-local service, not data: a
/// raster that has one installed is outside this inverse (see
/// `ScreenRaster::continuation`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenRasterContinuation {
    /// Total backing width in pixels, including any margin.
    pub width: u32,
    /// Total backing height in pixels, including any margin.
    pub height: u32,
    /// Overflow margin in pixels.
    pub margin: u32,
    /// Raw RGBA8 backing bytes, row-major.
    pub pixels: Vec<u8>,
    /// Current draw colour.
    pub color: [u8; 4],
    /// Map colour slots.
    pub map_colors: [Option<[u8; 4]>; 8],
}

impl ScreenRaster {
    /// Whether a host terrain map provider is installed on this raster.
    pub fn has_map_provider(&self) -> bool {
        self.map_provider.is_some()
    }

    /// Capture the complete retained raster state. Fails while a map
    /// provider is installed: provider output is host service state and
    /// has no data inverse.
    pub fn continuation(&self) -> Result<ScreenRasterContinuation, ScreenError> {
        if self.map_provider.is_some() {
            return Err(ScreenError::Host(
                "raster with a map provider has no continuation inverse".into(),
            ));
        }
        Ok(ScreenRasterContinuation {
            width: self.width,
            height: self.height,
            margin: self.margin,
            pixels: self.pixels.clone(),
            color: self.color.0,
            map_colors: self.map_colors.map(|slot| slot.map(|rgba| rgba.0)),
        })
    }

    /// Construct a raster from continuation data, without a map provider.
    pub fn from_continuation(image: ScreenRasterContinuation) -> Result<Self, ScreenError> {
        if image.margin != 0 {
            // The doubled margin is untrusted input: check the multiply
            // itself before either subtraction (R6-31).
            let twice = image
                .margin
                .checked_mul(2)
                .ok_or(ScreenError::InvalidSize)?;
            image
                .width
                .checked_sub(twice)
                .ok_or(ScreenError::InvalidSize)?;
            image
                .height
                .checked_sub(twice)
                .ok_or(ScreenError::InvalidSize)?;
        }
        let len = (image.width as usize)
            .checked_mul(image.height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or(ScreenError::InvalidSize)?;
        if image.width == 0
            || image.height == 0
            || image.width > 4096
            || image.height > 4096
            || len > MAX_RASTER_BYTES
            || image.pixels.len() != len
        {
            return Err(ScreenError::InvalidSize);
        }
        Ok(Self {
            width: image.width,
            height: image.height,
            margin: image.margin,
            pixels: image.pixels,
            color: Rgba8(image.color),
            map_provider: None,
            #[cfg(test)]
            reference_paths: false,
            map_colors: image.map_colors.map(|slot| slot.map(Rgba8)),
        })
    }

    /// Overlay continuation data onto a raster the host constructed with
    /// identical geometry. The raster must not have a map provider.
    pub fn restore_continuation(
        &mut self,
        image: ScreenRasterContinuation,
    ) -> Result<(), ScreenError> {
        if self.map_provider.is_some() {
            return Err(ScreenError::Host(
                "raster with a map provider has no continuation inverse".into(),
            ));
        }
        if image.width != self.width
            || image.height != self.height
            || image.margin != self.margin
            || image.pixels.len() != self.pixels.len()
        {
            return Err(ScreenError::InvalidSize);
        }
        self.pixels = image.pixels;
        self.color = Rgba8(image.color);
        self.map_colors = image.map_colors.map(|slot| slot.map(Rgba8));
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// R6-31: `margin * 2` overflowed before the checked subtraction could
    /// guard it. A margin of 0x8000_0000 must yield InvalidSize, not a
    /// panic, under checked arithmetic (dev/test profiles).
    #[test]
    fn from_continuation_rejects_margin_whose_double_overflows() {
        let image = ScreenRasterContinuation {
            width: 1,
            height: 1,
            margin: 0x8000_0000,
            pixels: Vec::new(),
            color: [0; 4],
            map_colors: [None; 8],
        };
        assert!(matches!(
            ScreenRaster::from_continuation(image),
            Err(ScreenError::InvalidSize)
        ));
    }

    /// A valid margined raster still round-trips through continuation.
    #[test]
    fn continuation_round_trip_with_margin() {
        let raster = ScreenRaster::with_margin(4, 3, 2).unwrap();
        let image = raster.continuation().unwrap();
        let ptr = image.pixels.as_ptr();
        let restored = ScreenRaster::from_continuation(image).unwrap();
        assert_eq!(restored.margin(), 2);
        assert_eq!(restored.pixels().as_ptr(), ptr);
    }

    #[test]
    fn alpha_zero_still_validates_utf8_and_clear_replaces_pixels() {
        let mut raster = ScreenRaster::new(8, 8).unwrap();
        raster
            .submit(&DrawCommand::SetColor(Rgba8([10, 20, 30, 0])))
            .unwrap();
        assert_eq!(
            raster.submit(&DrawCommand::Text([0.0, 0.0], vec![0xff])),
            Err(ScreenError::InvalidText)
        );
        assert_eq!(
            raster.submit(&DrawCommand::TextBox(
                [0.0, 0.0, 8.0, 8.0, 0.0, 0.0],
                vec![0xff]
            )),
            Err(ScreenError::InvalidText)
        );
        raster.submit(&DrawCommand::Clear).unwrap();
        assert!(raster
            .pixels()
            .chunks_exact(4)
            .all(|px| px == [10, 20, 30, 0]));
    }

    /// 依存を増やさない決定的な擬似乱数（SplitMix64）。
    struct Random(u64);
    impl Random {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        fn below(&mut self, bound: u64) -> u64 {
            self.next() % bound
        }
        /// 画面の内外、半端な小数、境界値、巨大値、非有限値を混ぜた座標。
        fn coordinate(&mut self, size: u32) -> f64 {
            let size = f64::from(size);
            match self.below(16) {
                0 => -(self.below(40) as f64),
                1 => size + self.below(40) as f64,
                2 => self.below(u64::from(size as u32) + 1) as f64,
                3 => self.below(u64::from(size as u32) + 1) as f64 + 0.5,
                4 => [1e9, -1e9, 70000.0, -70000.0, 1e300, -1e300][self.below(6) as usize],
                5 => [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.0][self.below(4) as usize],
                6 => (self.below(1 << 20) as f64 / 256.0) - 64.0,
                _ => (self.next() as f64 / u64::MAX as f64) * (size + 40.0) - 20.0,
            }
        }
        fn extent(&mut self, size: u32) -> f64 {
            match self.below(8) {
                0 => -(self.below(u64::from(size)) as f64),
                1 => 0.0,
                2 => self.coordinate(size),
                _ => (self.next() as f64 / u64::MAX as f64) * f64::from(size) * 1.2,
            }
        }
        fn command(&mut self, width: u32, height: u32) -> DrawCommand {
            let x = |r: &mut Self| r.coordinate(width);
            let y = |r: &mut Self| r.coordinate(height);
            match self.below(8) {
                0 | 1 => {
                    let alpha = [0, 1, 127, 128, 254, 255, 255, 255][self.below(8) as usize];
                    DrawCommand::SetColor(Rgba8([
                        self.below(256) as u8,
                        self.below(256) as u8,
                        self.below(256) as u8,
                        alpha,
                    ]))
                }
                2 | 3 => DrawCommand::Rect(
                    [x(self), y(self), self.extent(width), self.extent(height)],
                    true,
                ),
                4 | 5 => {
                    DrawCommand::Circle([x(self), y(self), self.extent(width.min(height))], true)
                }
                6 => DrawCommand::Triangle(
                    [[x(self), y(self)], [x(self), y(self)], [x(self), y(self)]],
                    self.below(4) != 0,
                ),
                _ => match self.below(8) {
                    0 => DrawCommand::Clear,
                    1 | 2 => DrawCommand::Rect(
                        [x(self), y(self), self.extent(width), self.extent(height)],
                        false,
                    ),
                    3 => DrawCommand::Circle(
                        [x(self), y(self), self.extent(width.min(height))],
                        false,
                    ),
                    _ => DrawCommand::Line([[x(self), y(self)], [x(self), y(self)]]),
                },
            }
        }
    }

    /// 行スパンの一括書き込みと整数の床除算が、従来の経路（1画素ずつの`plot`と剰余2回の床除算）と
    /// 全バイト一致することを確かめます。
    #[test]
    fn fast_paths_match_the_reference_paths() -> Result<(), ScreenError> {
        let mut random = Random(0x5EED_2026_1116);
        let mut compared = 0_u32;
        for (width, height, margin) in [
            (1, 1, 0),
            (32, 32, 0),
            (96, 96, 0),
            (64, 32, 3),
            (288, 160, 0),
            (17, 250, 1),
            (4096, 2, 0),
        ] {
            for _ in 0..40 {
                let mut fast = ScreenRaster::with_margin(width, height, margin)?;
                let mut reference = ScreenRaster::with_margin(width, height, margin)?;
                reference.reference_paths = true;
                let (w, h) = fast.dimensions();
                for _ in 0..60 {
                    let command = random.command(w, h);
                    fast.submit(&command)?;
                    reference.submit(&command)?;
                    assert!(
                        fast.pixels() == reference.pixels(),
                        "{width}x{height} margin {margin}: {command:?}"
                    );
                    compared += 1;
                }
            }
        }
        assert_eq!(compared, 7 * 40 * 60);
        Ok(())
    }

    /// 整数の床除算が、剰余2回の式とビット単位で同じ値を返すことを確かめます。
    #[test]
    fn floor_div_matches_the_reference_bit_for_bit() {
        let mut random = Random(0xF100_D1F0);
        let edges = [
            0.0,
            -0.0,
            1.0,
            -1.0,
            255.0,
            256.0,
            -256.0,
            0.5,
            -0.5,
            1.0 / 256.0,
            EXACT_INTEGER_LIMIT,
            -EXACT_INTEGER_LIMIT,
            EXACT_INTEGER_LIMIT + 2.0,
            -(EXACT_INTEGER_LIMIT + 2.0),
            EXACT_INTEGER_LIMIT * 2.0,
            9_007_199_254_740_992.0,
            1e300,
            -1e300,
            f64::MIN_POSITIVE,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
        ];
        let mut compared = 0_u32;
        let mut check = |n: f64, d: f64| {
            assert!(
                floor_div(n, d).to_bits() == floor_div_reference(n, d).to_bits(),
                "floor_div({n:?}, {d:?})"
            );
            compared += 1;
        };
        for n in edges {
            for d in edges {
                check(n, d);
            }
        }
        for _ in 0..200_000 {
            // 直線が実際に渡す規模（分子は2^50程度まで、分母は256の倍数）と、より広い整数。
            let n_bits = 1 + random.below(53);
            let d_bits = 1 + random.below(53);
            let n = (random.next() >> (64 - n_bits)) as f64;
            let d = (random.next() >> (64 - d_bits)) as f64;
            let n = if random.below(2) == 0 { n } else { -n };
            check(n, d);
            check(n, -d);
            check(n, d * 256.0);
            check(n + 0.25, d);
            check(n, d + 0.5);
            check(n * 256.0 + 128.0 * d, 256.0 * d);
        }
        assert_eq!(compared, 22 * 22 + 200_000 * 6);
    }

    /// 参照経路との比較が空振りしないよう、塗りつぶしが実際に画素を書くことを確かめます。
    #[test]
    fn span_fill_writes_opaque_and_blended_pixels() -> Result<(), ScreenError> {
        let mut raster = ScreenRaster::new(8, 4)?;
        raster.submit(&DrawCommand::SetColor(Rgba8([10, 20, 30, 255])))?;
        raster.submit(&DrawCommand::Rect([1.0, 1.0, 3.0, 2.0], true))?;
        let written = raster
            .pixels()
            .chunks_exact(4)
            .filter(|pixel| *pixel == [10, 20, 30, 255])
            .count();
        assert_eq!(written, 6);
        raster.submit(&DrawCommand::SetColor(Rgba8([200, 100, 50, 128])))?;
        raster.submit(&DrawCommand::Rect([0.0, 0.0, 8.0, 4.0], true))?;
        let expected =
            crate::blend::blend_pixel(Rgba8([200, 100, 50, 128]), Rgba8([10, 20, 30, 255]));
        let blended = raster
            .pixels()
            .chunks_exact(4)
            .filter(|pixel| *pixel == expected.0)
            .count();
        assert_eq!(blended, 6);
        assert_eq!(
            &raster.pixels()[..4],
            &crate::blend::blend_pixel(Rgba8([200, 100, 50, 128]), Rgba8([0; 4])).0
        );
        Ok(())
    }
}
