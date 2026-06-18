//! Native smoke test: render a .pptx slide to `pptx.png`.
//!
//!   cargo run -p dv-pptx --example pptx_demo -- <file.pptx> <font> [slide_index]

use dv_ir::FontId;
use dv_render::{render_to_pixmap, FontRegistry};
use dv_text::FontData;

fn main() {
    let mut args = std::env::args().skip(1);
    let pptx_path = args.next().expect("usage: pptx_demo <file.pptx> <font> [slide]");
    let font_path = args.next().expect("usage: pptx_demo <file.pptx> <font> [slide]");
    let idx: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);

    let pptx = std::fs::read(&pptx_path).expect("read pptx");
    let font_bytes = std::fs::read(&font_path).expect("read font");

    let deck = dv_pptx::Deck::parse(&pptx);
    let font = FontData::new(font_bytes.clone());
    let dl = deck.render_slide(idx, &font, 1.0);

    let mut registry = FontRegistry::new();
    registry.insert(FontId(0), FontData::new(font_bytes));

    let pixmap = render_to_pixmap(&dl, &registry);
    pixmap.save_png("pptx.png").expect("save pptx.png");
    println!("rendered slide {}/{} -> pptx.png ({}x{})", idx + 1, deck.slide_count(), pixmap.width(), pixmap.height());
}
