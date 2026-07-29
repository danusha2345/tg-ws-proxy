const TELEGRAM_BLUE: [u8; 4] = [34, 158, 217, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];
const SUPERSAMPLING: u32 = 4;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IconBitmap {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl IconBitmap {
    #[cfg(target_os = "linux")]
    pub fn into_argb(mut self) -> Vec<u8> {
        for pixel in self.rgba.chunks_exact_mut(4) {
            pixel.rotate_right(1);
        }
        self.rgba
    }
}

/// Renders the only expressive visual element in the desktop frontend:
/// Telegram's blue paper plane with a two-node relay trail.
#[must_use]
pub fn render(size: u32) -> IconBitmap {
    let mut rgba = vec![0_u8; (size * size * 4) as usize];
    let plane = [
        (0.235, 0.475),
        (0.790, 0.225),
        (0.625, 0.775),
        (0.500, 0.605),
        (0.395, 0.690),
        (0.400, 0.555),
    ];

    for y in 0..size {
        for x in 0..size {
            let mut blue_coverage = 0_u32;
            let mut white_coverage = 0_u32;
            for sy in 0..SUPERSAMPLING {
                for sx in 0..SUPERSAMPLING {
                    let px = (f64::from(x) + (f64::from(sx) + 0.5) / f64::from(SUPERSAMPLING))
                        / f64::from(size);
                    let py = (f64::from(y) + (f64::from(sy) + 0.5) / f64::from(SUPERSAMPLING))
                        / f64::from(size);
                    if distance_squared(px, py, 0.5, 0.5) <= 0.46_f64.powi(2) {
                        blue_coverage += 1;
                        if point_in_polygon(px, py, &plane)
                            || distance_to_segment(px, py, 0.155, 0.645, 0.365, 0.565) <= 0.025
                            || distance_squared(px, py, 0.155, 0.645) <= 0.047_f64.powi(2)
                            || distance_squared(px, py, 0.275, 0.600) <= 0.033_f64.powi(2)
                        {
                            white_coverage += 1;
                        }
                    }
                }
            }

            let samples = SUPERSAMPLING * SUPERSAMPLING;
            let alpha = u8::try_from(blue_coverage * 255 / samples)
                .expect("supersampling coverage fits u8");
            let white_mix = u8::try_from(
                (white_coverage * 255)
                    .checked_div(blue_coverage)
                    .unwrap_or(0),
            )
            .expect("supersampling coverage fits u8");
            let index = ((y * size + x) * 4) as usize;
            if alpha != 0 {
                rgba[index] = blend(TELEGRAM_BLUE[0], WHITE[0], white_mix);
                rgba[index + 1] = blend(TELEGRAM_BLUE[1], WHITE[1], white_mix);
                rgba[index + 2] = blend(TELEGRAM_BLUE[2], WHITE[2], white_mix);
            }
            rgba[index + 3] = alpha;
        }
    }

    IconBitmap {
        width: size,
        height: size,
        rgba,
    }
}

fn blend(background: u8, foreground: u8, amount: u8) -> u8 {
    let inverse = 255_u16 - u16::from(amount);
    u8::try_from(
        (u16::from(background) * inverse + u16::from(foreground) * u16::from(amount)) / 255,
    )
    .expect("blended color channel fits u8")
}

fn distance_squared(x: f64, y: f64, cx: f64, cy: f64) -> f64 {
    (x - cx).powi(2) + (y - cy).powi(2)
}

fn distance_to_segment(x: f64, y: f64, start_x: f64, start_y: f64, end_x: f64, end_y: f64) -> f64 {
    let dx = end_x - start_x;
    let dy = end_y - start_y;
    let projection = ((x - start_x) * dx + (y - start_y) * dy) / (dx * dx + dy * dy);
    let projection = projection.clamp(0.0, 1.0);
    let nearest_x = start_x + projection * dx;
    let nearest_y = start_y + projection * dy;
    distance_squared(x, y, nearest_x, nearest_y).sqrt()
}

fn point_in_polygon(x: f64, y: f64, polygon: &[(f64, f64)]) -> bool {
    let mut inside = false;
    let mut previous = polygon.len() - 1;
    for current in 0..polygon.len() {
        let (current_x, current_y) = polygon[current];
        let (previous_x, previous_y) = polygon[previous];
        if (current_y > y) != (previous_y > y)
            && x < (previous_x - current_x) * (y - current_y) / (previous_y - current_y) + current_x
        {
            inside = !inside;
        }
        previous = current;
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_has_transparent_corners_and_opaque_blue_body() {
        let icon = render(32);
        assert_eq!(icon.rgba.len(), 32 * 32 * 4);
        assert_eq!(&icon.rgba[..4], &[0, 0, 0, 0]);

        let center = ((16 * 32 + 16) * 4) as usize;
        assert_eq!(icon.rgba[center + 3], 255);
        assert!(icon.rgba[center + 2] >= TELEGRAM_BLUE[2]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_conversion_changes_rgba_to_argb() {
        let icon = IconBitmap {
            width: 1,
            height: 1,
            rgba: vec![1, 2, 3, 4],
        };
        assert_eq!(icon.into_argb(), [4, 1, 2, 3]);
    }
}
