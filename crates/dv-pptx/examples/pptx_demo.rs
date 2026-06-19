//! Native smoke test: render a .pptx slide to `pptx.png`.
//!
//!   cargo run -p dv-pptx --example pptx_demo -- <file.pptx> <font> [slide_index] ["名稱=字型路徑" ...]
//!
//! Extra `名稱=路徑` args register caller fonts (e.g. "標楷體=/path/Kaiti.ttf"),
//! mirroring the JS `renderPptxInto({ fonts: { ... } })` font map.

use dv_ir::FontId;
use dv_render::{render_to_pixmap, FontRegistry};
use dv_text::{FontData, Fonts};

fn main() {
    let mut args = std::env::args().skip(1);
    let pptx_path = args.next().expect("usage: pptx_demo <file.pptx> <font> [slide] [name=path ...]");
    let font_path = args.next().expect("usage: pptx_demo <file.pptx> <font> [slide] [name=path ...]");
    let idx: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);

    let pptx = std::fs::read(&pptx_path).expect("read pptx");
    let font = FontData::new(std::fs::read(&font_path).expect("read font"));
    let extra: Vec<(String, FontData)> = args
        .filter_map(|a| {
            let (name, path) = a.split_once('=')?;
            Some((name.to_string(), FontData::new(std::fs::read(path).ok()?)))
        })
        .collect();
    let fonts = Fonts::new(font, extra);

    let deck = dv_pptx::Deck::parse(&pptx);
    let dl = deck.render_slide(idx, &fonts, 1.0);

    let mut registry = FontRegistry::new();
    for (i, fd) in fonts.data().iter().enumerate() {
        registry.insert(FontId(i as u32), fd.clone());
    }

    let pixmap = render_to_pixmap(&dl, &registry);
    pixmap.save_png("pptx.png").expect("save pptx.png");
    println!("rendered slide {}/{} -> pptx.png ({}x{}) [{} fonts]", idx + 1, deck.slide_count(), pixmap.width(), pixmap.height(), fonts.data().len());
}
