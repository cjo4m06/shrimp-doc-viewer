//! Native smoke test: render one .docx page to `docx.png`.
//!
//!   cargo run -p dv-docx --example docx_demo -- <file.docx> <font.ttf|otf> [page]

use dv_ir::FontId;
use dv_render::{render_to_pixmap, FontRegistry};
use dv_text::FontData;

fn main() {
    let mut args = std::env::args().skip(1);
    let docx_path = args.next().expect("usage: docx_demo <file.docx> <font> [page]");
    let font_path = args.next().expect("usage: docx_demo <file.docx> <font> [page]");
    let page: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);

    let docx = std::fs::read(&docx_path).expect("read docx");
    let font_bytes = std::fs::read(&font_path).expect("read font");

    let font = FontData::new(font_bytes.clone());
    let doc = dv_docx::DocxDoc::parse(&docx, &font);
    let n = doc.page_count();
    let dl = doc.render_page(page.min(n.saturating_sub(1)), 2.0);

    let mut registry = FontRegistry::new();
    registry.insert(FontId(0), FontData::new(font_bytes));

    let pixmap = render_to_pixmap(&dl, &registry);
    pixmap.save_png("docx.png").expect("save docx.png");
    println!("rendered docx page {}/{} -> docx.png ({}x{})", page + 1, n, pixmap.width(), pixmap.height());
}
