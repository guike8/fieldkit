//! One vector master for Nightjar's app, window and macOS template icons.
//! No GUI, filesystem or platform dependencies: the bundler uses this too.

use tiny_skia::{
    Color, FillRule, GradientStop, LinearGradient, Paint, Path, PathBuilder, Pixmap, Point,
    SpreadMode, Transform,
};

/// Straight-alpha sRGB pixels, suitable for image encoders, egui and native icons.
#[derive(Debug)]
pub struct Icon {
    pub size: u32,
    pub rgba: Vec<u8>,
}

/// The midnight tile with an ivory night-bird and a small blue crescent wing.
/// Sizes are bounded to keep accidental allocations predictable.
pub fn app_icon(size: u32) -> Icon {
    render(size, false)
}

/// Black/transparent artwork, never a miniature app tile. Cocoa supplies the
/// correct foreground for light/dark menu bars and selected menu states.
pub fn template_icon(size: u32) -> Icon {
    render(size, true)
}

fn render(size: u32, template: bool) -> Icon {
    assert!((16..=1024).contains(&size), "icon size must be 16..=1024");
    // Render small artwork at 4× resolution, then average premultiplied pixels.
    // This preserves the tapered curves and avoids dark fringes at alpha edges.
    let scale = if size <= 64 { 4 } else { 1 };
    let dimension = size * scale;
    let mut pixmap = Pixmap::new(dimension, dimension).expect("bounded icon dimensions");
    // App tiles need generous clear space; menu-bar glyphs instead fill roughly
    // 15 of their 18 points. Both use the same unmodified vector master.
    let transform = if template {
        let unit = dimension as f32 / 72.0;
        Transform::from_row(unit, 0.0, 0.0, unit, -14.0 * unit, -14.0 * unit)
    } else {
        Transform::from_scale(dimension as f32 / 100.0, dimension as f32 / 100.0)
    };
    if !template {
        let tile = Paint {
            shader: LinearGradient::new(
                Point::from_xy(20.0, 5.0),
                Point::from_xy(78.0, 98.0),
                vec![
                    GradientStop::new(0.0, Color::from_rgba8(34, 46, 70, 255)),
                    GradientStop::new(1.0, Color::from_rgba8(9, 16, 29, 255)),
                ],
                SpreadMode::Pad,
                Transform::identity(),
            )
            .expect("non-degenerate gradient"),
            ..Default::default()
        };
        pixmap.fill_path(&tile_path(), &tile, FillRule::Winding, transform, None);
    }

    // The far wing also reads as a crescent: an observation/night motif rather
    // than a second badge. It becomes the same black ink in the template.
    let mut paint = Paint::default();
    if template {
        paint.set_color_rgba8(0, 0, 0, 255);
    } else {
        paint.set_color_rgba8(105, 167, 246, 255);
    }
    pixmap.fill_path(&crescent_wing(), &paint, FillRule::Winding, transform, None);
    if !template {
        paint.set_color_rgba8(242, 245, 249, 255);
    }
    pixmap.fill_path(&bird(), &paint, FillRule::EvenOdd, transform, None);

    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let mut sum = [0_u32; 4];
            for dy in 0..scale {
                for dx in 0..scale {
                    let pixel = pixmap.pixel(x * scale + dx, y * scale + dy).unwrap();
                    for (channel, value) in sum.iter_mut().zip([
                        pixel.red(),
                        pixel.green(),
                        pixel.blue(),
                        pixel.alpha(),
                    ]) {
                        *channel += u32::from(value);
                    }
                }
            }
            let samples = scale * scale;
            let alpha = (sum[3] + samples / 2) / samples;
            for channel in &sum[..3] {
                rgba.push(if alpha == 0 {
                    0
                } else {
                    (channel * 255 + sum[3] / 2)
                        .checked_div(sum[3])
                        .unwrap_or(0)
                        .min(255) as u8
                });
            }
            rgba.push(alpha as u8);
        }
    }
    Icon { size, rgba }
}

fn tile_path() -> Path {
    let mut path = PathBuilder::new();
    path.move_to(26.0, 5.0);
    path.line_to(74.0, 5.0);
    path.cubic_to(88.0, 5.0, 95.0, 12.0, 95.0, 26.0);
    path.line_to(95.0, 74.0);
    path.cubic_to(95.0, 88.0, 88.0, 95.0, 74.0, 95.0);
    path.line_to(26.0, 95.0);
    path.cubic_to(12.0, 95.0, 5.0, 88.0, 5.0, 74.0);
    path.line_to(5.0, 26.0);
    path.cubic_to(5.0, 12.0, 12.0, 5.0, 26.0, 5.0);
    path.close();
    path.finish().unwrap()
}

fn crescent_wing() -> Path {
    let mut path = PathBuilder::new();
    path.move_to(49.0, 45.0);
    path.cubic_to(50.0, 30.0, 63.0, 20.0, 79.0, 21.0);
    path.cubic_to(69.0, 26.0, 61.0, 34.0, 58.0, 43.0);
    path.line_to(49.0, 45.0);
    path.close();
    path.finish().unwrap()
}

fn bird() -> Path {
    let mut path = PathBuilder::new();
    // Broad near wing, low swept tail, round head and short pointed beak.
    path.move_to(20.0, 28.0);
    path.cubic_to(21.0, 49.0, 30.0, 65.0, 47.0, 68.0);
    path.line_to(43.0, 80.0);
    path.cubic_to(54.0, 77.0, 61.0, 70.0, 65.0, 64.0);
    path.cubic_to(75.0, 59.0, 78.0, 51.0, 74.0, 44.0);
    path.line_to(82.0, 40.0);
    path.line_to(73.0, 37.0);
    path.cubic_to(68.0, 32.0, 61.0, 33.0, 57.0, 38.0);
    path.cubic_to(53.0, 42.0, 51.0, 47.0, 49.0, 52.0);
    path.cubic_to(41.0, 43.0, 32.0, 34.0, 20.0, 28.0);
    path.close();
    // The small clear eye is an aperture, not a colored detail that disappears
    // when macOS recolors template artwork.
    path.push_circle(66.5, 41.5, 2.2);
    path.finish().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_sizes_have_transparent_corners_and_a_high_contrast_mark() {
        for size in [16, 18, 22, 32, 36, 64, 128, 256, 512, 1024] {
            let icon = app_icon(size);
            assert_eq!(icon.rgba.len(), (size * size * 4) as usize);
            assert_eq!(&icon.rgba[..4], &[0, 0, 0, 0]);
            let pixels = icon.rgba.as_chunks::<4>().0;
            let bright = pixels.iter().filter(|p| p[0] > 220 && p[3] == 255).count();
            let midnight = pixels.iter().filter(|p| p[0] < 50 && p[3] == 255).count();
            assert!(bright > (size * size / 20) as usize, "blank mark at {size}");
            assert!(
                midnight > (size * size / 3) as usize,
                "blank tile at {size}"
            );
            assert!(pixels.iter().any(|p| p[3] > 0 && p[3] < 255));
        }
    }

    #[test]
    fn templates_are_black_alpha_only_with_legible_coverage() {
        for size in [16, 18, 22, 32, 36, 44, 64] {
            let icon = template_icon(size);
            let pixels = icon.rgba.as_chunks::<4>().0;
            assert!(pixels.iter().all(|p| p[..3] == [0, 0, 0]));
            let ink = pixels.iter().filter(|p| p[3] > 192).count();
            assert!(
                ink > (size * size / 10) as usize,
                "blank template at {size}"
            );
            assert!(ink < (size * size / 2) as usize, "opaque tile at {size}");
            assert_eq!(pixels[0][3], 0);
            assert!(pixels.iter().any(|p| p[3] == 255));
            assert!(pixels.iter().any(|p| p[3] > 0 && p[3] < 255));
        }
    }

    #[test]
    fn render_is_deterministic() {
        assert_eq!(app_icon(32).rgba, app_icon(32).rgba);
        assert_ne!(app_icon(32).rgba, template_icon(32).rgba);
    }
}
