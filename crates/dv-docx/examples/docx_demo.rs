//! Native smoke test: render a .docx to `docx.png`.
//!
//!   cargo run -p dv-docx --example docx_demo -- <file.docx> <font.ttf|otf>

use dv_ir::FontId;
use dv_render::{render_to_pixmap, FontRegistry};
use dv_text::FontData;

fn main() {
    let mut args = std::env::args().skip(1);
    let docx_path = args.next().expect("usage: docx_demo <file.docx> <font>");
    let font_path = args.next().expect("usage: docx_demo <file.docx> <font>");

    let docx = std::fs::read(&docx_path).expect("read docx");
    let font_bytes = std::fs::read(&font_path).expect("read font");

    let font = FontData::new(font_bytes.clone());
    let dl = dv_docx::render_document(&docx, &font);

    let mut registry = FontRegistry::new();
    registry.insert(FontId(0), FontData::new(font_bytes));

    let pixmap = render_to_pixmap(&dl, &registry);
    pixmap.save_png("docx.png").expect("save docx.png");
    println!("rendered docx -> docx.png ({}x{})", pixmap.width(), pixmap.height());
}
