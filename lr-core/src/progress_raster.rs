//! Shared, platform-neutral rasterizer for the rounded LetRecovery progress bar.
//!
//! Both the PE window and the first-logon Shell paint the returned top-down BGRA pixels through
//! their own Win32 device contexts. Keeping the supersampled geometry here prevents the two
//! endpoints from drifting into visually similar but observably different controls.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RoundedProgressPalette {
    pub background: (u8, u8, u8),
    pub track: (u8, u8, u8),
    pub fill: (u8, u8, u8),
}

impl RoundedProgressPalette {
    /// Inno Setup 6.7 Modern Windows 11 dark colours used by the PE endpoint.
    pub const PE_DARK: Self = Self {
        background: (43, 43, 43),
        track: (31, 31, 31),
        fill: (113, 199, 132),
    };
}

/// Render one complete rounded progress control as opaque top-down BGRA pixels.
pub fn render_rounded_progress_bgra(
    width: i32,
    height: i32,
    percent: u8,
    palette: RoundedProgressPalette,
) -> Vec<u8> {
    if width <= 0 || height <= 0 {
        return Vec::new();
    }
    const SAMPLE_GRID: usize = 4;
    let radius = ((height + 1) / 2).max(1);
    let inner_width = (width - 2).max(0);
    let filled = inner_width.saturating_mul(i32::from(percent.min(100))) / 100;
    let colors = [
        palette.background,
        palette.track,
        palette.track,
        palette.fill,
    ];
    let mut pixels = vec![0_u8; width as usize * height as usize * 4];
    let sample_count = (SAMPLE_GRID * SAMPLE_GRID) as u32;
    for y in 0..height as usize {
        for x in 0..width as usize {
            let mut red = 0_u32;
            let mut green = 0_u32;
            let mut blue = 0_u32;
            for sample_y in 0..SAMPLE_GRID {
                for sample_x in 0..SAMPLE_GRID {
                    let px = x as f64 + (sample_x as f64 + 0.5) / SAMPLE_GRID as f64;
                    let py = y as f64 + (sample_y as f64 + 0.5) / SAMPLE_GRID as f64;
                    let color =
                        colors[progress_sample_layer(px, py, width, height, radius, filled)];
                    red += u32::from(color.0);
                    green += u32::from(color.1);
                    blue += u32::from(color.2);
                }
            }
            let offset = (y * width as usize + x) * 4;
            pixels[offset] = ((blue + sample_count / 2) / sample_count) as u8;
            pixels[offset + 1] = ((green + sample_count / 2) / sample_count) as u8;
            pixels[offset + 2] = ((red + sample_count / 2) / sample_count) as u8;
            pixels[offset + 3] = 255;
        }
    }
    pixels
}

fn progress_sample_layer(
    x: f64,
    y: f64,
    width: i32,
    height: i32,
    radius: i32,
    filled: i32,
) -> usize {
    if !point_in_rounded_rect(x, y, 0.0, 0.0, width as f64, height as f64, radius as f64) {
        return 0;
    }
    let inner_right = (width - 1).max(1) as f64;
    let inner_bottom = (height - 1).max(1) as f64;
    if !point_in_rounded_rect(
        x,
        y,
        1.0,
        1.0,
        inner_right,
        inner_bottom,
        radius.saturating_sub(1) as f64,
    ) {
        return 1;
    }
    if filled > 0 {
        let fill_right = (1 + filled).min(width - 1).max(1) as f64;
        let fill_radius = radius
            .saturating_sub(1)
            .min(filled / 2)
            .min((height - 2).max(0) / 2) as f64;
        if point_in_rounded_rect(x, y, 1.0, 1.0, fill_right, inner_bottom, fill_radius) {
            return 3;
        }
    }
    2
}

fn point_in_rounded_rect(
    x: f64,
    y: f64,
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
    radius: f64,
) -> bool {
    if x < left || x >= right || y < top || y >= bottom {
        return false;
    }
    let radius = radius.max(0.0).min((right - left).min(bottom - top) / 2.0);
    if radius == 0.0 {
        return true;
    }
    let nearest_x = x.clamp(left + radius, right - radius);
    let nearest_y = y.clamp(top + radius, bottom - radius);
    (x - nearest_x).powi(2) + (y - nearest_y).powi(2) <= radius * radius
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pe_dark_raster_preserves_background_outside_the_rounded_track() {
        let pixels = render_rounded_progress_bgra(80, 10, 25, RoundedProgressPalette::PE_DARK);
        assert_eq!(&pixels[..4], &[43, 43, 43, 255]);
        let fill_offset = (5 * 80 + 4) * 4;
        assert_ne!(&pixels[fill_offset..fill_offset + 4], &[43, 43, 43, 255]);
    }

    #[test]
    fn percentage_is_clamped_and_invalid_geometry_is_empty() {
        assert_eq!(
            render_rounded_progress_bgra(40, 10, 100, RoundedProgressPalette::PE_DARK),
            render_rounded_progress_bgra(40, 10, 255, RoundedProgressPalette::PE_DARK)
        );
        assert!(
            render_rounded_progress_bgra(0, 10, 50, RoundedProgressPalette::PE_DARK).is_empty()
        );
    }
}
