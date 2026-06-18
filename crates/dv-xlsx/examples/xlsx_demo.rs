//! Native smoke test: render an .xlsx sheet to `xlsx.png`.
//!
//!   cargo run -p dv-xlsx --example xlsx_demo -- <file.xlsx> <font.ttf|otf>

use dv_ir::FontId;
use dv_render::{render_to_pixmap, FontRegistry};
use dv_text::FontData;
use dv_xlsx::{render_sheet, Options};

fn main() {
    let mut args = std::env::args().skip(1);
    let xlsx_path = args.next().expect("usage: xlsx_demo <file.xlsx> <font>");
    let font_path = args.next().expect("usage: xlsx_demo <file.xlsx> <font>");

    let xlsx = std::fs::read(&xlsx_path).expect("read xlsx");
    let font_bytes = std::fs::read(&font_path).expect("read font");

    let font = FontData::new(font_bytes.clone());
    let dl = render_sheet(&xlsx, 0, &font, &Options::default());

    let mut registry = FontRegistry::new();
    registry.insert(FontId(0), FontData::new(font_bytes));

    let pixmap = render_to_pixmap(&dl, &registry);
    pixmap.save_png("xlsx.png").expect("save xlsx.png");
    println!("rendered xlsx -> xlsx.png ({}x{})", pixmap.width(), pixmap.height());
}
