const NAVY: [u8; 4] = [8, 21, 33, 255];
const CYAN: [u8; 4] = [79, 214, 255, 255];
const MINT: [u8; 4] = [92, 225, 163, 255];
const OFF_WHITE: [u8; 4] = [242, 250, 255, 255];
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

/// Renders the project mark: two opposing relay paths around a data spark.
#[must_use]
pub fn render(size: u32) -> IconBitmap {
    let mut rgba = vec![0_u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let mut accumulated = [0_u32; 4];
            for sy in 0..SUPERSAMPLING {
                for sx in 0..SUPERSAMPLING {
                    let px = (f64::from(x) + (f64::from(sx) + 0.5) / f64::from(SUPERSAMPLING))
                        / f64::from(size);
                    let py = (f64::from(y) + (f64::from(sy) + 0.5) / f64::from(SUPERSAMPLING))
                        / f64::from(size);
                    let color = sample(px, py);
                    for (channel, value) in accumulated.iter_mut().zip(color) {
                        *channel += u32::from(value);
                    }
                }
            }
            let samples = SUPERSAMPLING * SUPERSAMPLING;
            let index = ((y * size + x) * 4) as usize;
            for channel in 0..4 {
                rgba[index + channel] = u8::try_from(accumulated[channel] / samples)
                    .expect("averaged icon channel fits u8");
            }
        }
    }
    IconBitmap {
        width: size,
        height: size,
        rgba,
    }
}

fn sample(x: f64, y: f64) -> [u8; 4] {
    if !inside_rounded_square(x, y, 0.08, 0.22) {
        return [0, 0, 0, 0];
    }

    let upper_ring = on_ring(x, y, 0.50, 0.50, 0.30, 0.105) && y <= 0.50;
    let lower_ring = on_ring(x, y, 0.50, 0.50, 0.30, 0.105) && y >= 0.50;
    let cyan_arrow = point_in_polygon(
        x,
        y,
        &[
            (0.65, 0.27),
            (0.87, 0.27),
            (0.87, 0.20),
            (0.96, 0.35),
            (0.87, 0.50),
            (0.87, 0.43),
            (0.65, 0.43),
        ],
    );
    let mint_arrow = point_in_polygon(
        x,
        y,
        &[
            (0.35, 0.57),
            (0.13, 0.57),
            (0.13, 0.50),
            (0.04, 0.65),
            (0.13, 0.80),
            (0.13, 0.73),
            (0.35, 0.73),
        ],
    );
    let cyan_node = on_ring(x, y, 0.22, 0.44, 0.065, 0.035);
    let mint_node = on_ring(x, y, 0.78, 0.56, 0.065, 0.035);
    let spark = (x - 0.5).abs() + (y - 0.5).abs() <= 0.09;

    if spark {
        OFF_WHITE
    } else if cyan_arrow || cyan_node || upper_ring {
        CYAN
    } else if mint_arrow || mint_node || lower_ring {
        MINT
    } else {
        NAVY
    }
}

fn inside_rounded_square(x: f64, y: f64, inset: f64, radius: f64) -> bool {
    let center_x = x.clamp(inset + radius, 1.0 - inset - radius);
    let center_y = y.clamp(inset + radius, 1.0 - inset - radius);
    distance_squared(x, y, center_x, center_y) <= radius.powi(2)
}

fn on_ring(x: f64, y: f64, cx: f64, cy: f64, radius: f64, half_width: f64) -> bool {
    let distance = distance_squared(x, y, cx, cy).sqrt();
    (distance - radius).abs() <= half_width
}

fn distance_squared(x: f64, y: f64, cx: f64, cy: f64) -> f64 {
    (x - cx).powi(2) + (y - cy).powi(2)
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
    fn icon_has_transparent_corners_and_opaque_brand_center() {
        let icon = render(32);
        assert_eq!(icon.rgba.len(), 32 * 32 * 4);
        assert_eq!(&icon.rgba[..4], &[0, 0, 0, 0]);

        let center = ((16 * 32 + 16) * 4) as usize;
        assert_eq!(icon.rgba[center + 3], 255);
        assert!(icon.rgba[center] >= OFF_WHITE[0] - 8);
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
