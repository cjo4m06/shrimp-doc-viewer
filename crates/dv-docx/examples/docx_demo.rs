//! Native smoke test: render one .docx page to `docx.png`.
//!
//!   cargo run -p dv-docx --example docx_demo -- <file.docx> <font.ttf|otf> [page] ["名稱=字型路徑" ...]
//!
//! Extra `名稱=路徑` args register caller fonts (e.g. "標楷體=/path/Kaiti.ttf"),
//! mirroring the JS `mount({ fonts: { ... } })` font map.

use dv_ir::FontId;
use dv_render::{render_to_pixmap, FontRegistry};
use dv_text::FontData;

fn main() {
    let mut args = std::env::args().skip(1);
    let docx_path = args.next().expect("usage: docx_demo <file.docx> <font> [page] [name=path ...]");
    let font_path = args.next().expect("usage: docx_demo <file.docx> <font> [page] [name=path ...]");
    let page: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);

    let docx = std::fs::read(&docx_path).expect("read docx");
    let font = FontData::new(std::fs::read(&font_path).expect("read font"));

    // remaining args: caller-provided named fonts ("name=path")
    let extra: Vec<(String, FontData)> = args
        .filter_map(|a| {
            let (name, path) = a.split_once('=')?;
            Some((name.to_string(), FontData::new(std::fs::read(path).ok()?)))
        })
        .collect();

    let doc = dv_docx::DocxDoc::parse_with_fonts(&docx, font, extra);
    let n = doc.page_count();
    let dl = doc.render_page(page.min(n.saturating_sub(1)), 2.0);

    // Build a registry whose FontIds match the FontIds in the emitted glyph runs.
    let mut registry = FontRegistry::new();
    for (i, fd) in doc.fonts().data().iter().enumerate() {
        registry.insert(FontId(i as u32), fd.clone());
    }

    let pixmap = render_to_pixmap(&dl, &registry);
    pixmap.save_png("docx.png").expect("save docx.png");
    println!("rendered docx page {}/{} -> docx.png ({}x{}) [{} fonts]", page + 1, n, pixmap.width(), pixmap.height(), doc.fonts().data().len());
}
