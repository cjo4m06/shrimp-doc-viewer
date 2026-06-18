//! The doc-viewer WASM core, exposed to JS via wasm-bindgen.
//!
//! M1 surface is intentionally tiny: one demo entry that shapes + paints text
//! through the shared geba and returns straight RGBA bytes for the JS layer to
//! blit onto a canvas. Format frontends (PDF via PDFium in M2, then Office)
//! attach here behind the same module without changing the JS packaging.

use wasm_bindgen::prelude::*;

use dv_ir::{Color, Command, DisplayList, FontId, GlyphRun, Paint, PositionedGlyph};
use dv_render::{render, FontRegistry};
use dv_text::{shape, FontData};

#[wasm_bindgen(start)]
pub fn start() {
    // Surface Rust panics as readable JS console errors during development.
    console_error_panic_hook::set_once();
}

/// Shape `text` in the given font and paint it onto a `width`x`height` white
/// canvas, returning straight (un-premultiplied) RGBA bytes (`width*height*4`).
///
/// This is the M1 geba smoke test; it proves shape → outline → tiny-skia raster
/// works in the browser, including 繁體中文. Real documents go through `mount()`
/// in the JS layer once frontends land.
#[wasm_bindgen]
pub fn render_text_demo(
    width: u32,
    height: u32,
    font_bytes: Vec<u8>,
    text: &str,
    size: f32,
    x: f32,
    baseline: f32,
) -> Vec<u8> {
    let mut dl = DisplayList::new(width as f32, height as f32);

    let font = FontData::new(font_bytes.clone());
    let shaped = shape(&font, text, size);
    let scale = size / shaped.units_per_em.max(1.0);

    let mut pen_x = x;
    let mut glyphs = Vec::with_capacity(shaped.glyphs.len());
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

    render(&dl, &registry).data
}

/// The semantic version of the WASM core, surfaced to JS for diagnostics.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
