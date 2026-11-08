//! The showcase band's colours, taken from its cover.

/// The three stops of a showcase band's 135° gradient for a cover in straight RGBA rows: the
/// cover's most colourful common colour, a common one unlike it, and its darkest common one,
/// each mixed into near-black so the band stays dark and white text reads on it.
pub fn showcase_band(rgba: &[u8], width: u32, height: u32, row_stride: u32) -> [[u8; 3]; 3] {
    const GRID: u32 = 24;
    let mut bins = vec![(0u32, [0u32; 3]); 512];
    if width > 0 && height > 0 {
        for y in 0..GRID {
            for x in 0..GRID {
                let offset = ((y * height / GRID) * row_stride + (x * width / GRID) * 4) as usize;
                let Some(&[r, g, b, a]) = rgba.get(offset..offset + 4) else {
                    continue;
                };
                if a < 128 {
                    continue;
                }
                let bin = &mut bins
                    [(usize::from(r >> 5) << 6) | (usize::from(g >> 5) << 3) | usize::from(b >> 5)];
                bin.0 += 1;
                for (sum, value) in bin.1.iter_mut().zip([r, g, b]) {
                    *sum += u32::from(value);
                }
            }
        }
    }
    let mut common: Vec<(u32, [u8; 3])> = bins
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, sums)| (count, sums.map(|sum| (sum / count) as u8)))
        .collect();
    common.sort_by(|a, b| b.0.cmp(&a.0));
    common.truncate(8);
    let Some(&first) = common.first() else {
        let grey = [58, 61, 69];
        return stops(grey, grey, grey);
    };
    let weight = |(count, color): (u32, [u8; 3])| count as f32 * (0.35 + saturation(color));
    let primary = common
        .iter()
        .copied()
        .max_by(|a, b| weight(*a).total_cmp(&weight(*b)))
        .unwrap_or(first)
        .1;
    let secondary = common
        .iter()
        .copied()
        .filter(|(_, color)| distance(*color, primary) >= 64.0)
        .max_by(|a, b| weight(*a).total_cmp(&weight(*b)))
        .map_or(primary, |(_, color)| color);
    let tertiary = common
        .iter()
        .map(|(_, color)| *color)
        .filter(|color| *color != primary && *color != secondary)
        .min_by(|a, b| luminance(*a).total_cmp(&luminance(*b)))
        .unwrap_or(secondary);
    stops(primary, secondary, tertiary)
}

/// The band's mix: the cover's colours at 69 %, 48 % and 40 % over the page's near-black, each
/// held at a brightness white text still reads on (a pale cover would otherwise give a pale band).
fn stops(primary: [u8; 3], secondary: [u8; 3], tertiary: [u8; 3]) -> [[u8; 3]; 3] {
    [
        mix(primary, [0x11, 0x12, 0x14], 0.69),
        mix(secondary, [0x17, 0x18, 0x1c], 0.48),
        mix(tertiary, [0x11, 0x12, 0x14], 0.40),
    ]
    .map(|stop| {
        let scale = (110.0 / luminance(stop).max(1.0)).min(1.0);
        stop.map(|channel| (f32::from(channel) * scale).round() as u8)
    })
}

fn mix(color: [u8; 3], base: [u8; 3], amount: f32) -> [u8; 3] {
    std::array::from_fn(|channel| {
        (f32::from(color[channel]) * amount + f32::from(base[channel]) * (1.0 - amount)).round()
            as u8
    })
}

fn saturation(color: [u8; 3]) -> f32 {
    let max = color.iter().copied().max().unwrap_or(0);
    let min = color.iter().copied().min().unwrap_or(0);
    f32::from(max - min) / 255.0
}

fn luminance([r, g, b]: [u8; 3]) -> f32 {
    0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b)
}

fn distance(a: [u8; 3], b: [u8; 3]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(a, b)| (f32::from(*a) - f32::from(b)).powi(2))
        .sum::<f32>()
        .sqrt()
}
