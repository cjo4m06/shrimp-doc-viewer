//! Self-written XLSX grid renderer.
//!
//! M3.1 scope: parse cell *values* with `calamine` and lower a sheet into the
//! shared [`dv_ir::DisplayList`] as a spreadsheet grid — column-letter / row-number
//! headers, grid lines, and per-cell text (shaped through the shared text stack,
//! truncated to the cell width, numbers right-aligned). Real column widths / row
//! heights, merged cells, styles, and number formats come in later steps; this
//! is the first format that actually exercises the Rust geba.

use calamine::{Data, Reader, Xlsx};
use std::io::Cursor;

use dv_ir::{Color, Command, DisplayList, FillRule, FontId, GlyphRun, Paint, PathData, PositionedGlyph, Transform};
use dv_text::{shape, FontData};

#[derive(Clone, Copy)]
pub struct Options {
    pub max_rows: usize,
    pub max_cols: usize,
    pub col_width: f32,
    pub row_height: f32,
    pub header_w: f32,
    pub font_size: f32,
}

impl Default for Options {
    fn default() -> Self {
        Self { max_rows: 200, max_cols: 40, col_width: 90.0, row_height: 22.0, header_w: 44.0, font_size: 13.0 }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Align {
    Left,
    Right,
    Center,
}

const GRID: Color = Color::rgb(0xC8, 0xC8, 0xC8);
const HEADER_FILL: Color = Color::rgb(0xF0, 0xF1, 0xF3);
const HEADER_TEXT: Color = Color::rgb(0x44, 0x47, 0x4C);
const CELL_TEXT: Color = Color::BLACK;

/// Sheet names in workbook order (empty if the bytes aren't a readable xlsx).
pub fn sheet_names(bytes: &[u8]) -> Vec<String> {
    match Xlsx::new(Cursor::new(bytes.to_vec())) {
        Ok(wb) => wb.sheet_names().to_vec(),
        Err(_) => Vec::new(),
    }
}

/// Render one sheet into a display list. `font` is used both to measure and (by
/// the caller's registry, under [`FontId(0)`]) to paint cell text.
pub fn render_sheet(bytes: &[u8], sheet_index: usize, font: &FontData, opts: &Options) -> DisplayList {
    let mut wb = match Xlsx::new(Cursor::new(bytes.to_vec())) {
        Ok(w) => w,
        Err(_) => return DisplayList::new(opts.header_w + opts.col_width, opts.row_height * 2.0),
    };
    let names = wb.sheet_names().to_vec();
    let name = match names.get(sheet_index) {
        Some(n) => n.clone(),
        None => return DisplayList::new(opts.header_w + opts.col_width, opts.row_height * 2.0),
    };
    let range = match wb.worksheet_range(&name) {
        Ok(r) => r,
        Err(_) => return DisplayList::new(opts.header_w + opts.col_width, opts.row_height * 2.0),
    };

    let (used_rows, used_cols) = range.get_size();
    let nrows = used_rows.min(opts.max_rows);
    let ncols = used_cols.min(opts.max_cols);

    let hw = opts.header_w;
    let hh = opts.row_height; // header row height
    let cw = opts.col_width;
    let rh = opts.row_height;
    let total_w = hw + ncols as f32 * cw;
    let total_h = hh + nrows as f32 * rh;

    let mut dl = DisplayList::new(total_w, total_h);

    // Header bands (top = column letters, left = row numbers).
    dl.push(fill_rect(0.0, 0.0, total_w, hh, HEADER_FILL));
    dl.push(fill_rect(0.0, 0.0, hw, total_h, HEADER_FILL));

    // Grid lines: verticals then horizontals across the whole area.
    for c in 0..=ncols {
        let x = hw + c as f32 * cw;
        dl.push(vline(x, 0.0, total_h));
    }
    dl.push(vline(0.0, 0.0, total_h)); // left edge
    for r in 0..=nrows {
        let y = hh + r as f32 * rh;
        dl.push(hline(0.0, total_w, y));
    }
    dl.push(hline(0.0, total_w, 0.0)); // top edge

    // Header labels.
    for c in 0..ncols {
        let label = col_letter(c);
        let x = hw + c as f32 * cw;
        push_text(&mut dl, font, &label, opts.font_size, x, x + cw, 0.0, hh, Align::Center, HEADER_TEXT);
    }
    for r in 0..nrows {
        let label = (r + 1).to_string();
        let y = hh + r as f32 * rh;
        push_text(&mut dl, font, &label, opts.font_size, 0.0, hw, y, rh, Align::Center, HEADER_TEXT);
    }

    // Cell values.
    for r in 0..nrows {
        for c in 0..ncols {
            let cell = match range.get((r, c)) {
                Some(d) => d,
                None => continue,
            };
            let (text, align) = match format_cell(cell) {
                Some(v) => v,
                None => continue,
            };
            let x = hw + c as f32 * cw;
            let y = hh + r as f32 * rh;
            push_text(&mut dl, font, &text, opts.font_size, x, x + cw, y, rh, align, CELL_TEXT);
        }
    }

    dl
}

fn format_cell(cell: &Data) -> Option<(String, Align)> {
    match cell {
        Data::Empty => None,
        Data::String(s) if s.is_empty() => None,
        Data::String(s) => Some((s.clone(), Align::Left)),
        Data::Float(f) => Some((format_number(*f), Align::Right)),
        Data::Int(i) => Some((i.to_string(), Align::Right)),
        Data::Bool(b) => Some(((if *b { "TRUE" } else { "FALSE" }).to_string(), Align::Center)),
        Data::DateTime(dt) => Some((format_number(dt.as_f64()), Align::Right)),
        Data::DateTimeIso(s) => Some((s.clone(), Align::Right)),
        Data::DurationIso(s) => Some((s.clone(), Align::Right)),
        Data::Error(e) => Some((format!("{:?}", e), Align::Center)),
    }
}

fn format_number(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        format!("{}", f)
    }
}

fn col_letter(mut c: usize) -> String {
    let mut s = String::new();
    c += 1;
    while c > 0 {
        let rem = (c - 1) % 26;
        s.insert(0, (b'A' + rem as u8) as char);
        c = (c - 1) / 26;
    }
    s
}

fn fill_rect(x: f32, y: f32, w: f32, h: f32, color: Color) -> Command {
    let mut p = PathData::new();
    p.move_to(x, y);
    p.line_to(x + w, y);
    p.line_to(x + w, y + h);
    p.line_to(x, y + h);
    p.close();
    Command::FillPath { path: p, paint: Paint::Solid(color), fill_rule: FillRule::NonZero, transform: Transform::IDENTITY }
}

fn vline(x: f32, y0: f32, y1: f32) -> Command {
    let mut p = PathData::new();
    p.move_to(x, y0);
    p.line_to(x, y1);
    Command::StrokePath { path: p, paint: Paint::Solid(GRID), width: 1.0, transform: Transform::IDENTITY }
}

fn hline(x0: f32, x1: f32, y: f32) -> Command {
    let mut p = PathData::new();
    p.move_to(x0, y);
    p.line_to(x1, y);
    Command::StrokePath { path: p, paint: Paint::Solid(GRID), width: 1.0, transform: Transform::IDENTITY }
}

#[allow(clippy::too_many_arguments)]
fn push_text(
    dl: &mut DisplayList,
    font: &FontData,
    text: &str,
    size: f32,
    left: f32,
    right: f32,
    top: f32,
    height: f32,
    align: Align,
    color: Color,
) {
    let shaped = shape(font, text, size);
    let scale = size / shaped.units_per_em.max(1.0);
    let total: f32 = shaped.glyphs.iter().map(|g| g.x_advance * scale).sum();
    let pad = 4.0;
    let avail = (right - left - 2.0 * pad).max(0.0);
    let start_x = match align {
        Align::Left => left + pad,
        Align::Right if total <= avail => right - pad - total,
        Align::Center if total <= avail => left + (right - left - total) / 2.0,
        _ => left + pad,
    };
    let baseline = top + height * 0.5 + size * 0.34;
    let max_x = right - pad;

    let mut x = start_x;
    let mut glyphs = Vec::new();
    for g in &shaped.glyphs {
        let adv = g.x_advance * scale;
        if !glyphs.is_empty() && x + adv > max_x + 0.5 {
            break; // truncate at the cell edge (keep at least one glyph)
        }
        glyphs.push(PositionedGlyph { id: g.glyph_id, x: x + g.x_offset * scale, y: baseline - g.y_offset * scale });
        x += adv;
    }
    if !glyphs.is_empty() {
        dl.push(Command::Glyphs(GlyphRun { font: FontId(0), size, paint: Paint::Solid(color), glyphs }));
    }
}
