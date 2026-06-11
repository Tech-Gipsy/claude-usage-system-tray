use tiny_skia::{Color, LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};

pub const GREEN: (u8, u8, u8) = (74, 222, 128);
pub const AMBER: (u8, u8, u8) = (251, 191, 36);
pub const RED: (u8, u8, u8) = (239, 68, 68);
pub const GRAY: (u8, u8, u8) = (148, 163, 184);

/// Color thresholds (percent). Mirrored by barClass() in src/main.ts — keep in sync.
pub const AMBER_THRESHOLD: f32 = 60.0;
pub const RED_THRESHOLD: f32 = 85.0;

/// Returns the ring color for a given percentage.
/// pct is expected in 0..=100; callers should clamp (render_ring does).
pub fn ring_color(pct: Option<f32>) -> (u8, u8, u8) {
    match pct {
        None => GRAY,
        Some(p) if p >= RED_THRESHOLD => RED,
        Some(p) if p >= AMBER_THRESHOLD => AMBER,
        Some(_) => GREEN,
    }
}

/// 32x32 RGBA ring: faint full-circle track + colored arc for pct (clockwise from 12 o'clock).
pub fn render_ring(pct: Option<f32>) -> (Vec<u8>, u32, u32) {
    let pct = pct.map(|p| p.clamp(0.0, 100.0));

    const SIZE: u32 = 32;
    let mut pixmap = Pixmap::new(SIZE, SIZE).unwrap();
    let cx = SIZE as f32 / 2.0;
    let cy = SIZE as f32 / 2.0;
    let r = SIZE as f32 / 2.0 - 4.0;

    let arc = |from_frac: f32, to_frac: f32| -> Option<tiny_skia::Path> {
        let mut pb = PathBuilder::new();
        let steps = 64;
        let mut started = false;
        for i in 0..=steps {
            let f = from_frac + (to_frac - from_frac) * i as f32 / steps as f32;
            let angle = f * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
            let (x, y) = (cx + r * angle.cos(), cy + r * angle.sin());
            if started {
                pb.line_to(x, y);
            } else {
                pb.move_to(x, y);
                started = true;
            }
        }
        pb.finish()
    };

    let mut stroke = Stroke {
        width: 4.0,
        line_cap: LineCap::Round,
        ..Default::default()
    };
    let mut paint = Paint::default();
    paint.anti_alias = true;

    // track
    paint.set_color(Color::from_rgba8(120, 120, 130, 110));
    if let Some(track) = arc(0.0, 1.0) {
        pixmap.stroke_path(&track, &paint, &stroke, Transform::identity(), None);
    }

    // progress arc
    let frac = pct.map(|p| (p / 100.0).clamp(0.0, 1.0)).unwrap_or(0.0);
    if frac > 0.005 {
        let (cr, cg, cb) = ring_color(pct);
        paint.set_color(Color::from_rgba8(cr, cg, cb, 255));
        stroke.width = 5.0;
        if let Some(p) = arc(0.0, frac) {
            pixmap.stroke_path(&p, &paint, &stroke, Transform::identity(), None);
        }
    }

    (pixmap.take(), SIZE, SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_colors() {
        assert_eq!(ring_color(Some(10.0)), GREEN);
        assert_eq!(ring_color(Some(59.9)), GREEN);
        assert_eq!(ring_color(Some(60.0)), AMBER);
        assert_eq!(ring_color(Some(85.0)), RED);
        assert_eq!(ring_color(Some(100.0)), RED);
        assert_eq!(ring_color(None), GRAY);
    }

    #[test]
    fn renders_32px_rgba_with_visible_pixels() {
        let (rgba, w, h) = render_ring(Some(62.0));
        assert_eq!((w, h), (32, 32));
        assert_eq!(rgba.len(), 32 * 32 * 4);
        assert!(rgba.chunks(4).any(|p| p[3] > 0));
    }

    #[test]
    fn zero_percent_still_draws_track() {
        let (rgba, _, _) = render_ring(Some(0.0));
        assert!(rgba.chunks(4).any(|p| p[3] > 0));
    }

    #[test]
    fn none_renders_track_only_gray() {
        let (rgba, _, _) = render_ring(None);
        // no fully-opaque colored arc pixels — only the faint track (alpha < 255)
        assert!(rgba.chunks(4).all(|p| p[3] < 255));
    }

    #[test]
    fn arc_pixel_at_three_oclock_is_amber_for_62_pct() {
        let (rgba, w, _) = render_ring(Some(62.0));
        // 62% sweeps past 3 o'clock (25%); sample the rightmost ring point (x=cx+r, y=cy)
        let (x, y) = (16 + 12, 16usize);
        let idx = (y * w as usize + x) * 4;
        let px = &rgba[idx..idx + 4];
        assert_eq!(px[3], 255);
        // premultiplied RGBA with a=255 equals straight color: AMBER (251,191,36)
        assert!(px[0] > 200 && px[1] > 140 && px[2] < 100, "got {:?}", px);
    }
}
