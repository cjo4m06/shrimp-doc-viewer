//! The doc-viewer WASM core, exposed to JS via wasm-bindgen.
//!
//! M1 surface is intentionally tiny: one demo entry that shapes + paints text
//! through the shared geba and returns straight RGBA bytes for the JS layer to
//! blit onto a canvas. Format frontends (PDF via PDFium in M2, then Office)
//! attach here behind the same module without changing the JS packaging.

use std::collections::HashMap;

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
        bold: false,
        glyphs,
    }));

    let mut registry = FontRegistry::new();
    registry.insert(FontId(0), FontData::new(font_bytes));

    render(&dl, &registry).data
}

/// A rendered raster (straight RGBA) handed to JS for `ImageData`/canvas.
#[wasm_bindgen]
pub struct RenderedImage {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

#[wasm_bindgen]
impl RenderedImage {
    #[wasm_bindgen(getter)]
    pub fn width(&self) -> u32 {
        self.width
    }
    #[wasm_bindgen(getter)]
    pub fn height(&self) -> u32 {
        self.height
    }
    /// Move the RGBA bytes out to JS (`width*height*4`). Call once.
    #[wasm_bindgen(js_name = takeData)]
    pub fn take_data(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.data)
    }
}

/// Render one sheet of an XLSX workbook to RGBA via the shared geba.
/// `max_rows`/`max_cols` cap the rendered range (0 = use defaults).
#[wasm_bindgen]
pub fn render_xlsx(
    xlsx: Vec<u8>,
    font_bytes: Vec<u8>,
    sheet_index: usize,
    max_rows: usize,
    max_cols: usize,
) -> RenderedImage {
    let mut opts = dv_xlsx::Options::default();
    if max_rows > 0 {
        opts.max_rows = max_rows;
    }
    if max_cols > 0 {
        opts.max_cols = max_cols;
    }

    let measure_font = FontData::new(font_bytes.clone());
    let dl = dv_xlsx::render_sheet(&xlsx, sheet_index, &measure_font, &opts);

    let mut registry = FontRegistry::new();
    registry.insert(FontId(0), FontData::new(font_bytes));

    let rgba = render(&dl, &registry);
    RenderedImage { width: rgba.width, height: rgba.height, data: rgba.data }
}

/// Sheet names of an XLSX workbook, in order.
#[wasm_bindgen]
pub fn xlsx_sheet_names(xlsx: &[u8]) -> Vec<String> {
    dv_xlsx::sheet_names(xlsx)
}

/// A parsed XLSX workbook kept alive for virtualized, zoomable viewport renders.
/// Sheets are parsed lazily and cached.
#[wasm_bindgen]
pub struct XlsxBook {
    bytes: Vec<u8>,
    font: Vec<u8>,
    names: Vec<String>,
    opts: dv_xlsx::Options,
    cache: HashMap<usize, dv_xlsx::Sheet>,
}

#[wasm_bindgen]
impl XlsxBook {
    #[wasm_bindgen(constructor)]
    pub fn new(bytes: Vec<u8>, font: Vec<u8>) -> XlsxBook {
        let names = dv_xlsx::sheet_names(&bytes);
        XlsxBook { bytes, font, names, opts: dv_xlsx::Options::default(), cache: HashMap::new() }
    }

    #[wasm_bindgen(js_name = sheetNames)]
    pub fn sheet_names(&self) -> Vec<String> {
        self.names.clone()
    }

    fn sheet(&mut self, idx: usize) -> &dv_xlsx::Sheet {
        let (bytes, opts) = (&self.bytes, &self.opts);
        self.cache.entry(idx).or_insert_with(|| dv_xlsx::Sheet::parse(bytes, idx, opts))
    }

    /// `[total_w, total_h, header_w, header_h]` in base (zoom=1) px.
    #[wasm_bindgen(js_name = sheetGeometry)]
    pub fn sheet_geometry(&mut self, idx: usize) -> Vec<f32> {
        let s = self.sheet(idx);
        vec![s.total_w(), s.total_h(), s.header_w(), s.header_h()]
    }

    /// Render the viewport at data-px `(scroll_x, scroll_y)` into a
    /// `dev_w`×`dev_h` surface scaled by `scale` (= zoom × dpr).
    #[wasm_bindgen(js_name = renderViewport)]
    pub fn render_viewport(&mut self, idx: usize, scroll_x: f32, scroll_y: f32, dev_w: f32, dev_h: f32, scale: f32) -> RenderedImage {
        let font_bytes = self.font.clone();
        let font = FontData::new(font_bytes.clone());
        let dl = self.sheet(idx).render_viewport(&font, scroll_x, scroll_y, dev_w, dev_h, scale);
        let mut registry = FontRegistry::new();
        registry.insert(FontId(0), FontData::new(font_bytes));
        let rgba = render(&dl, &registry);
        RenderedImage { width: rgba.width, height: rgba.height, data: rgba.data }
    }
}

/// A parsed PPTX deck kept alive for per-slide, scalable renders.
#[wasm_bindgen]
pub struct PptxDeck {
    deck: dv_pptx::Deck,
    font: Vec<u8>,
}

#[wasm_bindgen]
impl PptxDeck {
    #[wasm_bindgen(constructor)]
    pub fn new(bytes: Vec<u8>, font: Vec<u8>) -> PptxDeck {
        PptxDeck { deck: dv_pptx::Deck::parse(&bytes), font }
    }

    #[wasm_bindgen(js_name = slideCount)]
    pub fn slide_count(&self) -> usize {
        self.deck.slide_count()
    }

    /// `[width, height]` in base (zoom=1) px.
    #[wasm_bindgen(js_name = slideSize)]
    pub fn slide_size(&self) -> Vec<f32> {
        vec![self.deck.width(), self.deck.height()]
    }

    #[wasm_bindgen(js_name = renderSlide)]
    pub fn render_slide(&self, idx: usize, scale: f32) -> RenderedImage {
        let font = FontData::new(self.font.clone());
        let dl = self.deck.render_slide(idx, &font, scale);
        let mut registry = FontRegistry::new();
        registry.insert(FontId(0), FontData::new(self.font.clone()));
        let rgba = render(&dl, &registry);
        RenderedImage { width: rgba.width, height: rgba.height, data: rgba.data }
    }
}

/// Render a DOCX document to RGBA via the shared geba (one continuous page).
#[wasm_bindgen]
pub fn render_docx(docx: Vec<u8>, font_bytes: Vec<u8>) -> RenderedImage {
    let measure_font = FontData::new(font_bytes.clone());
    let dl = dv_docx::render_document(&docx, &measure_font);

    let mut registry = FontRegistry::new();
    registry.insert(FontId(0), FontData::new(font_bytes));

    let rgba = render(&dl, &registry);
    RenderedImage { width: rgba.width, height: rgba.height, data: rgba.data }
}

/// The semantic version of the WASM core, surfaced to JS for diagnostics.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
