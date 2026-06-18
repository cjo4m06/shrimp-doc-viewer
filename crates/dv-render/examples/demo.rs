//! Native pipeline smoke test: shape text + paint a vector scene to `demo.png`.
//!
//! Usage:
//!   cargo run -p dv-render --example demo -- <font.ttf|otf> ["text to render"]
//!
//! This exercises the whole geba (shape → outline → tiny-skia raster → pixels)
//! without a browser. Eyeball `demo.png` to confirm glyphs (incl. 繁體中文,
//! given a CJK font) actually render.

use dv_ir::{Color, Command, DisplayList, FillRule, FontId, GlyphRun, Paint, PathData, PositionedGlyph, Transform};
use dv_render::{render_to_pixmap, FontRegistry};
use dv_text::{shape, FontData};

fn main() {
    let mut args = std::env::args().skip(1);
    let font_path = args.next().unwrap_or_else(|| {
        eprintln!("usage: demo <font.ttf|otf> [text]");
        std::process::exit(2);
    });
    let text = args.next().unwrap_or_else(|| "Hello, 你好,繁體中文! 0123".to_string());

    let font_bytes = std::fs::read(&font_path).expect("read font file");

    let width = 900.0_f32;
    let height = 260.0_f32;
    let mut dl = DisplayList::new(width, height);

    // --- vector scene: prove path fills + transforms ---
    let mut rect = PathData::new();
    rect.move_to(30.0, 30.0);
    rect.line_to(250.0, 30.0);
    rect.line_to(250.0, 90.0);
    rect.line_to(30.0, 90.0);
    rect.close();
    dl.push(Command::FillPath {
        path: rect,
        paint: Paint::Solid(Color::rgb(74, 144, 226)),
        fill_rule: FillRule::NonZero,
        transform: Transform::IDENTITY,
    });

    let mut tri = PathData::new();
    tri.move_to(300.0, 90.0);
    tri.line_to(360.0, 30.0);
    tri.line_to(420.0, 90.0);
    tri.close();
    dl.push(Command::FillPath {
        path: tri,
        paint: Paint::Solid(Color::rgb(226, 96, 74)),
        fill_rule: FillRule::NonZero,
        transform: Transform::IDENTITY,
    });

    // --- text: shape, lay out at a baseline, paint as positioned glyphs ---
    let font = FontData::new(font_bytes.clone());
    let size = 56.0_f32;
    let shaped = shape(&font, &text, size);
    let scale = size / shaped.units_per_em;

    let mut pen_x = 40.0_f32;
    let baseline = 200.0_f32;
    let mut glyphs = Vec::new();
    for g in &shaped.glyphs {
        glyphs.push(PositionedGlyph {
            id: g.glyph_id,
            x: pen_x + g.x_offset * scale,
            y: baseline - g.y_offset * scale,
        });
        pen_x += g.x_advance * scale;
    }
    dl.push(Command::Glyphs(GlyphRun {
        font: FontId(0),
        size,
        paint: Paint::Solid(Color::BLACK),
        glyphs,
    }));

    let mut registry = FontRegistry::new();
    registry.insert(FontId(0), FontData::new(font_bytes));

    let pixmap = render_to_pixmap(&dl, &registry);
    pixmap.save_png("demo.png").expect("save demo.png");
    println!(
        "rendered {} glyph(s) of {:?} -> demo.png ({}x{})",
        shaped.glyphs.len(),
        text,
        pixmap.width(),
        pixmap.height()
    );
}
