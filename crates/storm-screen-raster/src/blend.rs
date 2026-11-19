//! 承認されたRGBAソースアルファブレンド方程式の整数表現。
use storm_lua_spec::screen::Rgba8;

#[inline]
pub(crate) fn div255(x: u32) -> u8 {
    (((x + 1) * 257) >> 16) as u8
}

#[inline]
fn channel(source: u8, alpha: u8, destination: u8) -> u8 {
    let a = u32::from(alpha);
    div255(u32::from(source) * a + u32::from(destination) * (255 - a) + 127)
}

/// 同一ソース色を複数ピクセルへ連続適用する際の事前計算済みブレンド係数。
#[derive(Clone, Copy)]
pub(crate) struct PreparedBlend {
    base: [u32; 4],
    inv_a: u32,
}

impl PreparedBlend {
    #[inline]
    pub(crate) fn new(source: Rgba8) -> Self {
        let [r, g, b, a] = source.0.map(u32::from);
        Self {
            base: [r * a + 127, g * a + 127, b * a + 127, a * a + 127],
            inv_a: 255 - a,
        }
    }

    #[inline]
    pub(crate) fn apply(self, pixel: &mut [u8; 4]) {
        let inv_a = self.inv_a;
        pixel[0] = div255(self.base[0] + u32::from(pixel[0]) * inv_a);
        pixel[1] = div255(self.base[1] + u32::from(pixel[1]) * inv_a);
        pixel[2] = div255(self.base[2] + u32::from(pixel[2]) * inv_a);
        pixel[3] = div255(self.base[3] + u32::from(pixel[3]) * inv_a);
    }
}

/// ソースアルファを用いてRGBおよびアルファをブレンドします。アルファ値は As*As + Ad*(1-As) となります。
/// これは意図的に、通常の事前乗算済み（premultiplied）source-overアルファ方程式ではありません。
pub fn blend_pixel(source: Rgba8, destination: Rgba8) -> Rgba8 {
    let s = source.0;
    let d = destination.0;
    Rgba8([
        channel(s[0], s[3], d[0]),
        channel(s[1], s[3], d[1]),
        channel(s[2], s[3], d[2]),
        channel(s[3], s[3], d[3]),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_div255_matches_reference_division_for_all_possible_inputs() {
        for x in 0..=(255 * 255 + 127) {
            assert_eq!(div255(x), (x / 255) as u8, "mismatch at x={x}");
        }
        for s in [0_u8, 1, 63, 127, 128, 192, 254, 255] {
            for a in 0..=255_u8 {
                let prep = PreparedBlend::new(Rgba8([s, 255 - s, s / 2, a]));
                for d in 0..=255_u8 {
                    let mut px = [d, 255 - d, d / 3, d];
                    let expected = blend_pixel(Rgba8([s, 255 - s, s / 2, a]), Rgba8(px)).0;
                    prep.apply(&mut px);
                    assert_eq!(px, expected);
                }
            }
        }
    }
}
