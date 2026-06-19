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
    RenderedImage {
        width: rgba.width,
        height: rgba.height,
        data: rgba.data,
    }
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
        XlsxBook {
            bytes,
            font,
            names,
            opts: dv_xlsx::Options::default(),
            cache: HashMap::new(),
        }
    }

    /// Build a single-sheet grid book from CSV / TSV / semicolon-delimited bytes,
    /// reusing the xlsx viewport renderer + viewer.
    #[wasm_bindgen(js_name = fromCsv)]
    pub fn from_csv(bytes: Vec<u8>, font: Vec<u8>) -> XlsxBook {
        let opts = dv_xlsx::Options::default();
        let sheet = dv_xlsx::Sheet::from_csv(&bytes, &opts);
        let mut cache = HashMap::new();
        cache.insert(0usize, sheet);
        XlsxBook {
            bytes: Vec::new(),
            font,
            names: vec!["CSV".to_string()],
            opts,
            cache,
        }
    }

    /// Build a grid book from an ODS spreadsheet — every sheet becomes a tab.
    #[wasm_bindgen(js_name = fromOds)]
    pub fn from_ods(bytes: Vec<u8>, font: Vec<u8>) -> XlsxBook {
        let opts = dv_xlsx::Options::default();
        let mut cache = HashMap::new();
        let mut names = Vec::new();
        for (i, (name, rows)) in dv_odf::parse_spreadsheet(&bytes).into_iter().enumerate() {
            names.push(if name.is_empty() {
                format!("Sheet{}", i + 1)
            } else {
                name
            });
            cache.insert(i, dv_xlsx::Sheet::from_rows(rows, &opts));
        }
        if names.is_empty() {
            names.push("Sheet1".to_string());
            cache.insert(0, dv_xlsx::Sheet::from_rows(Vec::new(), &opts));
        }
        XlsxBook {
            bytes: Vec::new(),
            font,
            names,
            opts,
            cache,
        }
    }

    #[wasm_bindgen(js_name = sheetNames)]
    pub fn sheet_names(&self) -> Vec<String> {
        self.names.clone()
    }

    fn sheet(&mut self, idx: usize) -> &dv_xlsx::Sheet {
        let (bytes, opts) = (&self.bytes, &self.opts);
        self.cache
            .entry(idx)
            .or_insert_with(|| dv_xlsx::Sheet::parse(bytes, idx, opts))
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
    pub fn render_viewport(
        &mut self,
        idx: usize,
        scroll_x: f32,
        scroll_y: f32,
        dev_w: f32,
        dev_h: f32,
        scale: f32,
    ) -> RenderedImage {
        let font_bytes = self.font.clone();
        let font = FontData::new(font_bytes.clone());
        let dl = self
            .sheet(idx)
            .render_viewport(&font, scroll_x, scroll_y, dev_w, dev_h, scale);
        let mut registry = FontRegistry::new();
        registry.insert(FontId(0), FontData::new(font_bytes));
        let rgba = render(&dl, &registry);
        RenderedImage {
            width: rgba.width,
            height: rgba.height,
            data: rgba.data,
        }
    }
}

/// A parsed PPTX deck kept alive for per-slide, scalable renders.
#[wasm_bindgen]
pub struct PptxDeck {
    deck: dv_pptx::Deck,
    fonts: dv_text::Fonts,
    registry: FontRegistry,
}

#[wasm_bindgen]
impl PptxDeck {
    /// `font` is the default/fallback face; `extra` is an Array of [name, Uint8Array]
    /// caller fonts for families the deck declares but doesn't embed (e.g. 標楷體).
    /// (PowerPoint-embedded fonts are MicroType-Express-compressed EOT, not loadable.)
    #[wasm_bindgen(constructor)]
    pub fn new(bytes: Vec<u8>, font: Vec<u8>, extra: js_sys::Array) -> PptxDeck {
        use wasm_bindgen::JsCast;
        let mut extra_fonts: Vec<(String, FontData)> = Vec::new();
        for v in extra.iter() {
            if let Ok(pair) = v.dyn_into::<js_sys::Array>() {
                let name = pair.get(0).as_string().unwrap_or_default();
                if let Ok(arr) = pair.get(1).dyn_into::<js_sys::Uint8Array>() {
                    if !name.is_empty() {
                        extra_fonts.push((name, FontData::new(arr.to_vec())));
                    }
                }
            }
        }
        let fonts = dv_text::Fonts::new(FontData::new(font), extra_fonts);
        let mut registry = FontRegistry::new();
        for (i, fd) in fonts.data().iter().enumerate() {
            registry.insert(FontId(i as u32), fd.clone());
        }
        PptxDeck {
            deck: dv_pptx::Deck::parse(&bytes),
            fonts,
            registry,
        }
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
        let dl = self.deck.render_slide(idx, &self.fonts, scale);
        let rgba = render(&dl, &self.registry);
        RenderedImage {
            width: rgba.width,
            height: rgba.height,
            data: rgba.data,
        }
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
    RenderedImage {
        width: rgba.width,
        height: rgba.height,
        data: rgba.data,
    }
}

/// A paginated DOCX kept alive for per-page virtualized, zoomable rendering.
#[wasm_bindgen]
pub struct DocxDoc {
    doc: dv_docx::DocxDoc,
    registry: FontRegistry,
}

#[wasm_bindgen]
impl DocxDoc {
    /// `font` is the default/fallback face. `extra` is an Array of [name, Uint8Array]
    /// pairs: caller-provided fonts for families the document declares but does not
    /// embed (e.g. ["標楷體", <bytes>]). Embedded fonts in the file are loaded too.
    #[wasm_bindgen(constructor)]
    pub fn new(bytes: Vec<u8>, font: Vec<u8>, extra: js_sys::Array) -> DocxDoc {
        use wasm_bindgen::JsCast;
        let mut extra_fonts: Vec<(String, FontData)> = Vec::new();
        for v in extra.iter() {
            if let Ok(pair) = v.dyn_into::<js_sys::Array>() {
                let name = pair.get(0).as_string().unwrap_or_default();
                if let Ok(arr) = pair.get(1).dyn_into::<js_sys::Uint8Array>() {
                    if !name.is_empty() {
                        extra_fonts.push((name, FontData::new(arr.to_vec())));
                    }
                }
            }
        }
        let doc = dv_docx::DocxDoc::parse_with_fonts(&bytes, FontData::new(font), extra_fonts);
        let mut registry = FontRegistry::new();
        for (i, fd) in doc.fonts().data().iter().enumerate() {
            registry.insert(FontId(i as u32), fd.clone());
        }
        DocxDoc { doc, registry }
    }

    #[wasm_bindgen(js_name = pageCount)]
    pub fn page_count(&self) -> usize {
        self.doc.page_count()
    }

    /// `[width, height]` in base (zoom=1) px.
    #[wasm_bindgen(js_name = pageSize)]
    pub fn page_size(&self) -> Vec<f32> {
        let (w, h) = self.doc.page_size();
        vec![w, h]
    }

    #[wasm_bindgen(js_name = renderPage)]
    pub fn render_page(&self, idx: usize, scale: f32) -> RenderedImage {
        let dl = self.doc.render_page(idx, scale);
        let rgba = render(&dl, &self.registry);
        RenderedImage {
            width: rgba.width,
            height: rgba.height,
            data: rgba.data,
        }
    }
}

/// Parse the JS `[name, Uint8Array]` caller-font pairs into named FontData.
fn parse_extra_fonts(extra: js_sys::Array) -> Vec<(String, FontData)> {
    use wasm_bindgen::JsCast;
    let mut out = Vec::new();
    for v in extra.iter() {
        if let Ok(pair) = v.dyn_into::<js_sys::Array>() {
            let name = pair.get(0).as_string().unwrap_or_default();
            if let Ok(arr) = pair.get(1).dyn_into::<js_sys::Uint8Array>() {
                if !name.is_empty() {
                    out.push((name, FontData::new(arr.to_vec())));
                }
            }
        }
    }
    out
}

/// A paginated rich-text flow document (Markdown / plain text / RTF / ODT / ODP),
/// rendered page-by-page through the same viewer as DOCX.
#[wasm_bindgen]
pub struct FlowDoc {
    doc: dv_flow::FlowDoc,
    registry: FontRegistry,
}

fn build_flow(blocks: Vec<dv_flow::Block>, font: Vec<u8>, extra: js_sys::Array) -> FlowDoc {
    let fonts = dv_text::Fonts::new(FontData::new(font), parse_extra_fonts(extra));
    let doc = dv_flow::FlowDoc::new(&blocks, fonts);
    let mut registry = FontRegistry::new();
    for (i, fd) in doc.fonts().data().iter().enumerate() {
        registry.insert(FontId(i as u32), fd.clone());
    }
    FlowDoc { doc, registry }
}

#[wasm_bindgen]
impl FlowDoc {
    #[wasm_bindgen(js_name = fromMarkdown)]
    pub fn from_markdown(bytes: Vec<u8>, font: Vec<u8>, extra: js_sys::Array) -> FlowDoc {
        build_flow(
            dv_md::parse_markdown(&String::from_utf8_lossy(&bytes)),
            font,
            extra,
        )
    }
    #[wasm_bindgen(js_name = fromText)]
    pub fn from_text(bytes: Vec<u8>, font: Vec<u8>, extra: js_sys::Array) -> FlowDoc {
        build_flow(
            dv_md::parse_text(&String::from_utf8_lossy(&bytes)),
            font,
            extra,
        )
    }
    #[wasm_bindgen(js_name = fromRtf)]
    pub fn from_rtf(bytes: Vec<u8>, font: Vec<u8>, extra: js_sys::Array) -> FlowDoc {
        build_flow(dv_rtf::parse(&bytes), font, extra)
    }
    #[wasm_bindgen(js_name = fromOdt)]
    pub fn from_odt(bytes: Vec<u8>, font: Vec<u8>, extra: js_sys::Array) -> FlowDoc {
        build_flow(dv_odf::parse_text(&bytes), font, extra)
    }
    #[wasm_bindgen(js_name = fromOdp)]
    pub fn from_odp(bytes: Vec<u8>, font: Vec<u8>, extra: js_sys::Array) -> FlowDoc {
        build_flow(dv_odf::parse_presentation(&bytes), font, extra)
    }

    #[wasm_bindgen(js_name = pageCount)]
    pub fn page_count(&self) -> usize {
        self.doc.page_count()
    }
    #[wasm_bindgen(js_name = pageSize)]
    pub fn page_size(&self) -> Vec<f32> {
        let (w, h) = self.doc.page_size();
        vec![w, h]
    }
    #[wasm_bindgen(js_name = renderPage)]
    pub fn render_page(&self, idx: usize, scale: f32) -> RenderedImage {
        let rgba = render(&self.doc.render_page(idx, scale), &self.registry);
        RenderedImage {
            width: rgba.width,
            height: rgba.height,
            data: rgba.data,
        }
    }
}

/// The semantic version of the WASM core, surfaced to JS for diagnostics.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
