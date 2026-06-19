//! tiny-skia CPU raster backend.
//!
//! Consumes a [`dv_ir::DisplayList`] plus a [`FontRegistry`] and paints to a
//! `tiny_skia::Pixmap`. tiny-skia gives Skia-grade analytic anti-aliasing in
//! pure Rust (~200-300KB in wasm, no GPU dependency), which is what makes small
//! 繁體中文 glyph edges look right. The display-list consumer is intentionally
//! self-contained so a `vello`/WebGPU backend can be added behind a capability
//! check later without touching any frontend.

use std::collections::HashMap;

use dv_ir::{Command, DisplayList, FillRule, FontId, Paint, PathData, PathVerb, Transform};
use dv_text::FontData;
use tiny_skia::{
    Color as SkColor, ColorU8, FillRule as SkFillRule, FilterQuality, LineCap, LineJoin, Mask,
    Paint as SkPaint, PathBuilder, Pixmap, PixmapPaint, Stroke, Transform as SkTransform,
};

/// Maps [`FontId`]s used in a display list to actual font bytes. Frontends fill
/// this in; the backend resolves glyph outlines through it.
#[derive(Default)]
pub struct FontRegistry {
    fonts: HashMap<u32, FontData>,
}

impl FontRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, id: FontId, font: FontData) {
        self.fonts.insert(id.0, font);
    }

    pub fn get(&self, id: FontId) -> Option<&FontData> {
        self.fonts.get(&id.0)
    }
}

fn sk_transform(t: Transform) -> SkTransform {
    SkTransform::from_row(t.sx, t.ky, t.kx, t.sy, t.tx, t.ty)
}

fn sk_color(c: dv_ir::Color) -> SkColor {
    SkColor::from_rgba8(c.r, c.g, c.b, c.a)
}

fn sk_fill_rule(r: FillRule) -> SkFillRule {
    match r {
        FillRule::NonZero => SkFillRule::Winding,
        FillRule::EvenOdd => SkFillRule::EvenOdd,
    }
}

fn build_path(p: &PathData) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    for verb in &p.verbs {
        match *verb {
            PathVerb::MoveTo(x, y) => pb.move_to(x, y),
            PathVerb::LineTo(x, y) => pb.line_to(x, y),
            PathVerb::QuadTo(cx, cy, x, y) => pb.quad_to(cx, cy, x, y),
            PathVerb::CubicTo(a, b, c, d, x, y) => pb.cubic_to(a, b, c, d, x, y),
            PathVerb::Close => pb.close(),
        }
    }
    pb.finish()
}

/// Paint a display list onto a fresh white `Pixmap`.
pub fn render_to_pixmap(dl: &DisplayList, fonts: &FontRegistry) -> Pixmap {
    let w = dl.width.ceil().max(1.0) as u32;
    let h = dl.height.ceil().max(1.0) as u32;
    let mut pixmap = Pixmap::new(w, h).expect("non-zero pixmap dimensions");
    pixmap.fill(SkColor::WHITE);

    // Cache outlines within a single render — CJK pages repeat glyphs heavily —
    // and open each font once (rebuilding the FontRef per glyph is the hot path).
    let mut glyph_cache: HashMap<(u32, u32), Option<tiny_skia::Path>> = HashMap::new();
    let mut sources: HashMap<u32, Option<dv_text::GlyphSource>> = HashMap::new();

    for command in &dl.commands {
        match command {
            Command::FillPath {
                path,
                paint,
                fill_rule,
                transform,
            } => {
                if let Some(p) = build_path(path) {
                    let mut sp = SkPaint {
                        anti_alias: true,
                        ..Default::default()
                    };
                    let Paint::Solid(c) = *paint;
                    sp.set_color(sk_color(c));
                    pixmap.fill_path(
                        &p,
                        &sp,
                        sk_fill_rule(*fill_rule),
                        sk_transform(*transform),
                        None,
                    );
                }
            }
            Command::StrokePath {
                path,
                paint,
                width,
                transform,
            } => {
                if let Some(p) = build_path(path) {
                    let mut sp = SkPaint {
                        anti_alias: true,
                        ..Default::default()
                    };
                    let Paint::Solid(c) = *paint;
                    sp.set_color(sk_color(c));
                    let stroke = Stroke {
                        width: *width,
                        ..Stroke::default()
                    };
                    pixmap.stroke_path(&p, &sp, &stroke, sk_transform(*transform), None);
                }
            }
            Command::Glyphs(run) => {
                let Some(font) = fonts.get(run.font) else {
                    continue;
                };
                let upem = font.units_per_em().max(1.0);
                let scale = run.size / upem;
                let mut sp = SkPaint {
                    anti_alias: true,
                    ..Default::default()
                };
                let Paint::Solid(c) = run.paint;
                sp.set_color(sk_color(c));

                // Faux-bold: stroke the outline on top of the fill to thicken it.
                // The path is in font units and the per-glyph transform scales by
                // `scale`, so express the ~0.07·em px target in font units. (Heavier
                // than a hairline so CJK bold reads as bold, approximating 標楷體 bold.)
                let bold_stroke = if run.bold {
                    Some(Stroke {
                        width: run.size * 0.07 / scale,
                        line_cap: LineCap::Round,
                        line_join: LineJoin::Round,
                        ..Stroke::default()
                    })
                } else {
                    None
                };

                let src = sources
                    .entry(run.font.0)
                    .or_insert_with(|| font.glyph_source());
                for g in &run.glyphs {
                    let entry = glyph_cache.entry((run.font.0, g.id)).or_insert_with(|| {
                        let outline = src.as_ref().map(|s| s.outline(g.id)).unwrap_or_default();
                        build_path(&outline)
                    });
                    let Some(p) = entry else { continue };
                    // Outline is font-unit, y-up. Scale to px and flip y onto the
                    // baseline at (g.x, g.y).
                    let t = SkTransform::from_row(scale, 0.0, 0.0, -scale, g.x, g.y);
                    pixmap.fill_path(p, &sp, SkFillRule::Winding, t, None);
                    if let Some(stroke) = &bold_stroke {
                        pixmap.stroke_path(p, &sp, stroke, t, None);
                    }
                }
            }
            Command::Image {
                rgba,
                src_w,
                src_h,
                x,
                y,
                w,
                h,
                clip,
            } => {
                if *src_w == 0 || *src_h == 0 || rgba.len() < (src_w * src_h * 4) as usize {
                    continue;
                }
                let Some(mut src) = Pixmap::new(*src_w, *src_h) else {
                    continue;
                };
                {
                    // tiny-skia stores premultiplied RGBA; convert from straight.
                    let dst = src.pixels_mut();
                    for (i, px) in dst.iter_mut().enumerate() {
                        let o = i * 4;
                        *px = ColorU8::from_rgba(rgba[o], rgba[o + 1], rgba[o + 2], rgba[o + 3])
                            .premultiply();
                    }
                }
                let t =
                    SkTransform::from_row(*w / *src_w as f32, 0.0, 0.0, *h / *src_h as f32, *x, *y);
                let paint = PixmapPaint {
                    quality: FilterQuality::Bilinear,
                    ..PixmapPaint::default()
                };
                // Optional clip (device-space path -> coverage mask), e.g. ellipse crop.
                let mask = clip.as_ref().and_then(build_path).and_then(|p| {
                    let mut m = Mask::new(pixmap.width(), pixmap.height())?;
                    m.fill_path(&p, SkFillRule::Winding, true, SkTransform::identity());
                    Some(m)
                });
                pixmap.draw_pixmap(0, 0, src.as_ref(), &paint, t, mask.as_ref());
            }
        }
    }

    pixmap
}

/// Rendered pixels as straight (un-premultiplied) RGBA, ready for Canvas
/// `ImageData` / `createImageBitmap`.
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// Render and return straight RGBA bytes.
pub fn render(dl: &DisplayList, fonts: &FontRegistry) -> Rgba {
    let pixmap = render_to_pixmap(dl, fonts);
    let width = pixmap.width();
    let height = pixmap.height();
    let mut data = Vec::with_capacity((width * height * 4) as usize);
    for px in pixmap.pixels() {
        let c = px.demultiply();
        data.push(c.red());
        data.push(c.green());
        data.push(c.blue());
        data.push(c.alpha());
    }
    Rgba {
        width,
        height,
        data,
    }
}
