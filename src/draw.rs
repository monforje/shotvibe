//! Annotations: the shape model shared by the on-screen preview (gpui) and the
//! exported image (tiny-skia + ab_glyph on the CPU), so both look the same.

use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use image::RgbaImage;
use tiny_skia::{FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform};

pub type P = (f32, f32);

pub const PALETTE: [u32; 8] = [
    0xff3b30, 0xff9500, 0xffcc00, 0x34c759, 0x0a84ff, 0xbf5af2, 0xffffff, 0x1c1c1e,
];
pub const WIDTHS: [f32; 5] = [2., 3., 5., 8., 12.];

pub const TEXT_FONT: &str = "DejaVu Sans";
const TEXT_FONT_FILES: [&str; 3] = [
    "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/dejavu/DejaVuSans-Bold.ttf",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Move,
    Pen,
    Line,
    Arrow,
    Rect,
    Marker,
    Text,
}

#[derive(Clone, Debug)]
pub enum ShapeKind {
    Pen(Vec<P>),
    Marker(Vec<P>),
    Line(P, P),
    Arrow(P, P),
    Rect(P, P),
    Text(P, String),
}

#[derive(Clone, Debug)]
pub struct Shape {
    pub kind: ShapeKind,
    pub color: u32,
    pub width: f32,
}

impl Shape {
    /// Marker strokes are wide and translucent.
    pub fn stroke_width(&self) -> f32 {
        match self.kind {
            ShapeKind::Marker(_) => self.width * 3. + 8.,
            _ => self.width,
        }
    }

    pub fn alpha(&self) -> f32 {
        match self.kind {
            ShapeKind::Marker(_) => 0.38,
            _ => 1.,
        }
    }
}

pub fn text_size(width: f32) -> f32 {
    14. + width * 2.5
}

pub fn text_line_height(width: f32) -> f32 {
    (text_size(width) * 1.3).round()
}

/// Where the shaft of an arrow ends and its head polygon.
pub fn arrow_geometry(a: P, b: P, width: f32) -> (P, [P; 3]) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = (dx * dx + dy * dy).sqrt().max(0.001);
    let (ux, uy) = (dx / len, dy / len);
    let head = (width * 3.5 + 8.).min(len);
    let half = head * 0.55;
    let base = (b.0 - ux * head, b.1 - uy * head);
    let left = (base.0 - uy * half, base.1 + ux * half);
    let right = (base.0 + uy * half, base.1 - ux * half);
    // Stop the shaft inside the head so the round cap doesn't poke out.
    let shaft_end = (b.0 - ux * head * 0.7, b.1 - uy * head * 0.7);
    (shaft_end, [b, left, right])
}

pub fn rect_from(a: P, b: P) -> (f32, f32, f32, f32) {
    (
        a.0.min(b.0),
        a.1.min(b.1),
        (a.0 - b.0).abs(),
        (a.1 - b.1).abs(),
    )
}

pub fn load_font() -> Option<FontVec> {
    let mut candidates: Vec<String> = TEXT_FONT_FILES.iter().map(|s| s.to_string()).collect();
    if let Ok(out) = std::process::Command::new("fc-match")
        .args(["-f", "%{file}", "DejaVu Sans:bold"])
        .output()
    {
        candidates.insert(0, String::from_utf8_lossy(&out.stdout).to_string());
    }
    candidates.iter().find_map(|f| {
        std::fs::read(f)
            .ok()
            .and_then(|b| FontVec::try_from_vec(b).ok())
    })
}

/// Crops `base` to `sel` (logical coords; `scale` = image px per logical px)
/// and burns the annotations in.
pub fn render(
    base: &RgbaImage,
    sel: (f32, f32, f32, f32),
    scale: f32,
    shapes: &[Shape],
    font: Option<&FontVec>,
) -> RgbaImage {
    let x0 = ((sel.0 * scale).round() as u32).min(base.width().saturating_sub(1));
    let y0 = ((sel.1 * scale).round() as u32).min(base.height().saturating_sub(1));
    let w = ((sel.2 * scale).round() as u32).clamp(1, base.width() - x0);
    let h = ((sel.3 * scale).round() as u32).clamp(1, base.height() - y0);
    let mut crop = image::imageops::crop_imm(base, x0, y0, w, h).to_image();
    for p in crop.pixels_mut() {
        p.0[3] = 255;
    }

    let size = tiny_skia::IntSize::from_wh(w, h).expect("non-empty crop");
    let mut pixmap = Pixmap::from_vec(crop.into_raw(), size).expect("valid pixmap");
    let ts = Transform::from_row(scale, 0., 0., scale, -(x0 as f32), -(y0 as f32));

    for shape in shapes {
        let (r, g, b) = rgb(shape.color);
        let mut paint = Paint::default();
        paint.set_color_rgba8(r, g, b, (shape.alpha() * 255.) as u8);
        paint.anti_alias = true;
        let stroke = Stroke {
            width: shape.stroke_width(),
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Default::default()
        };
        let stroke_points = |pts: &[P], pixmap: &mut Pixmap| {
            if pts.len() == 1 || pts.iter().all(|p| *p == pts[0]) {
                if let Some(dot) = PathBuilder::from_circle(pts[0].0, pts[0].1, stroke.width / 2.) {
                    pixmap.fill_path(&dot, &paint, FillRule::Winding, ts, None);
                }
                return;
            }
            let mut pb = PathBuilder::new();
            pb.move_to(pts[0].0, pts[0].1);
            for p in &pts[1..] {
                pb.line_to(p.0, p.1);
            }
            if let Some(path) = pb.finish() {
                pixmap.stroke_path(&path, &paint, &stroke, ts, None);
            }
        };
        match &shape.kind {
            ShapeKind::Pen(pts) | ShapeKind::Marker(pts) => stroke_points(pts, &mut pixmap),
            ShapeKind::Line(a, b) => stroke_points(&[*a, *b], &mut pixmap),
            ShapeKind::Arrow(a, b) => {
                let (shaft_end, head) = arrow_geometry(*a, *b, shape.width);
                stroke_points(&[*a, shaft_end], &mut pixmap);
                let mut pb = PathBuilder::new();
                pb.move_to(head[0].0, head[0].1);
                pb.line_to(head[1].0, head[1].1);
                pb.line_to(head[2].0, head[2].1);
                pb.close();
                if let Some(path) = pb.finish() {
                    pixmap.fill_path(&path, &paint, FillRule::Winding, ts, None);
                }
            }
            ShapeKind::Rect(a, b) => {
                let (x, y, w, h) = rect_from(*a, *b);
                if let Some(rect) = tiny_skia::Rect::from_xywh(x, y, w.max(0.5), h.max(0.5)) {
                    let path = PathBuilder::from_rect(rect);
                    let stroke = Stroke {
                        line_join: LineJoin::Miter,
                        ..stroke.clone()
                    };
                    pixmap.stroke_path(&path, &paint, &stroke, ts, None);
                }
            }
            ShapeKind::Text(..) => {}
        }
    }

    let mut out = RgbaImage::from_raw(w, h, pixmap.take()).expect("pixmap size");
    if let Some(font) = font {
        for shape in shapes {
            if let ShapeKind::Text(pos, text) = &shape.kind {
                draw_text(&mut out, font, text, *pos, shape, scale, (x0, y0));
            }
        }
    }
    out
}

fn draw_text(
    img: &mut RgbaImage,
    font: &FontVec,
    text: &str,
    pos: P,
    shape: &Shape,
    scale: f32,
    origin: (u32, u32),
) {
    let px_size = PxScale::from(text_size(shape.width) * scale);
    let scaled = font.as_scaled(px_size);
    let line_h = text_line_height(shape.width) * scale;
    let glyph_h = scaled.ascent() - scaled.descent();
    let mut x = pos.0 * scale - origin.0 as f32;
    let baseline = pos.1 * scale - origin.1 as f32 + (line_h - glyph_h) / 2. + scaled.ascent();
    let (r, g, b) = rgb(shape.color);
    let mut prev = None;
    for c in text.chars() {
        let id = font.glyph_id(c);
        if let Some(p) = prev {
            x += scaled.kern(p, id);
        }
        let glyph = id.with_scale_and_position(px_size, ab_glyph::point(x, baseline));
        if let Some(outline) = font.outline_glyph(glyph) {
            let bb = outline.px_bounds();
            outline.draw(|gx, gy, cov| {
                let (px, py) = (bb.min.x as i32 + gx as i32, bb.min.y as i32 + gy as i32);
                if px < 0 || py < 0 || px >= img.width() as i32 || py >= img.height() as i32 {
                    return;
                }
                let dst = img.get_pixel_mut(px as u32, py as u32);
                let a = cov.clamp(0., 1.);
                for (d, s) in dst.0.iter_mut().zip([r, g, b]) {
                    *d = (s as f32 * a + *d as f32 * (1. - a)).round() as u8;
                }
            });
        }
        x += scaled.h_advance(id);
        prev = Some(id);
    }
}

fn rgb(c: u32) -> (u8, u8, u8) {
    ((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_and_draw() {
        let base = RgbaImage::from_pixel(200, 100, image::Rgba([10, 20, 30, 255]));
        let shapes = vec![Shape {
            kind: ShapeKind::Rect((20., 20.), (60., 40.)),
            color: 0xff0000,
            width: 4.,
        }];
        // Logical 100x50 screen rendered at 2x.
        let out = render(&base, (10., 10., 60., 40.), 2., &shapes, None);
        assert_eq!(out.dimensions(), (120, 80));
        // Rect corner (20,20) logical → (20,20) px in the crop.
        assert_eq!(out.get_pixel(20, 20).0[0], 255);
        assert_eq!(out.get_pixel(110, 75).0, [10, 20, 30, 255]);
    }
}
