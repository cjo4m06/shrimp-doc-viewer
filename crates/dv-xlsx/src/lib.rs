//! Self-written XLSX grid renderer.
//!
//! Values come from `calamine`; the layout/structure layer (column widths, row
//! heights, merged cells) is parsed from the OOXML in [`model`] and lowered,
//! together with the values, into the shared [`dv_ir::DisplayList`]. This is the
//! first format rendered entirely by our own code over the Rust geba.
//!
//! Done: headers, grid lines, real column widths & row heights, merged cells,
//! per-cell text (shaped + truncated, numbers right-aligned). Next: cell styles
//! (fills/fonts/borders) and a number-format engine.

mod model;

use std::collections::{HashMap, HashSet};
use std::io::Cursor;

use calamine::{Data, Reader, Xlsx};

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
        Self { max_rows: 500, max_cols: 64, col_width: 64.0, row_height: 20.0, header_w: 44.0, font_size: 13.0 }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Align {
    Left,
    Right,
    Center,
}

const GRID: Color = Color::rgb(0xC8, 0xC8, 0xC8);
const MERGE_BORDER: Color = Color::rgb(0xB0, 0xB4, 0xBA);
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

/// Render one sheet into a display list. `font` is used to measure and (under
/// [`FontId(0)`] in the caller's registry) to paint cell text.
pub fn render_sheet(bytes: &[u8], sheet_index: usize, font: &FontData, opts: &Options) -> DisplayList {
    let values = read_values(bytes, sheet_index);
    let geom = model::parse_geometry(bytes, sheet_index, opts.col_width, opts.row_height);

    // Extents: the larger of the declared dimension and the cells we actually have.
    let mut ext_rows = geom.n_rows;
    let mut ext_cols = geom.n_cols;
    for &(r, c) in values.keys() {
        ext_rows = ext_rows.max(r + 1);
        ext_cols = ext_cols.max(c + 1);
    }
    let nrows = (ext_rows as usize).min(opts.max_rows);
    let ncols = (ext_cols as usize).min(opts.max_cols).max(1);
    let nrows = nrows.max(1);

    let hw = opts.header_w;
    let hh = opts.row_height;

    // Prefix sums of column/row boundaries (header band first).
    let mut col_x = vec![hw];
    for c in 0..ncols {
        col_x.push(col_x[c] + geom.col_width(c as u32));
    }
    let mut row_y = vec![hh];
    for r in 0..nrows {
        row_y.push(row_y[r] + geom.row_height(r as u32));
    }
    let total_w = col_x[ncols];
    let total_h = row_y[nrows];

    let mut dl = DisplayList::new(total_w, total_h);

    // Header bands.
    dl.push(fill_rect(0.0, 0.0, total_w, hh, HEADER_FILL));
    dl.push(fill_rect(0.0, 0.0, hw, total_h, HEADER_FILL));

    // Grid lines at every boundary.
    dl.push(vline(0.0, 0.0, total_h));
    for c in 0..=ncols {
        dl.push(vline(col_x[c], 0.0, total_h));
    }
    dl.push(hline(0.0, total_w, 0.0));
    for r in 0..=nrows {
        dl.push(hline(0.0, total_w, row_y[r]));
    }

    // Merged cells: cover interior grid lines, redraw the outer border, mark covered.
    let mut covered: HashSet<(u32, u32)> = HashSet::new();
    let mut merge_anchor: HashMap<(u32, u32), (u32, u32)> = HashMap::new(); // top-left -> (r1,c1)
    for &(r0, c0, r1, c1) in &geom.merges {
        if (r0 as usize) >= nrows || (c0 as usize) >= ncols {
            continue;
        }
        let r1 = (r1 as usize).min(nrows - 1) as u32;
        let c1 = (c1 as usize).min(ncols - 1) as u32;
        let x0 = col_x[c0 as usize];
        let y0 = row_y[r0 as usize];
        let x1 = col_x[c1 as usize + 1];
        let y1 = row_y[r1 as usize + 1];
        dl.push(fill_rect(x0, y0, x1 - x0, y1 - y0, Color::WHITE));
        dl.push(rect_stroke(x0, y0, x1 - x0, y1 - y0, MERGE_BORDER));
        for r in r0..=r1 {
            for c in c0..=c1 {
                covered.insert((r, c));
            }
        }
        covered.remove(&(r0, c0));
        merge_anchor.insert((r0, c0), (r1, c1));
    }

    // Header labels.
    for c in 0..ncols {
        push_text(&mut dl, font, &col_letter(c), opts.font_size, col_x[c], col_x[c + 1], 0.0, hh, Align::Center, HEADER_TEXT);
    }
    for r in 0..nrows {
        push_text(&mut dl, font, &(r + 1).to_string(), opts.font_size, 0.0, hw, row_y[r], row_y[r + 1] - row_y[r], Align::Center, HEADER_TEXT);
    }

    // Cell values.
    for r in 0..nrows as u32 {
        for c in 0..ncols as u32 {
            if covered.contains(&(r, c)) {
                continue;
            }
            let (text, align) = match values.get(&(r, c)) {
                Some(v) => v.clone(),
                None => continue,
            };
            let (left, right, top, height) = match merge_anchor.get(&(r, c)) {
                Some(&(r1, c1)) => (col_x[c as usize], col_x[c1 as usize + 1], row_y[r as usize], row_y[r1 as usize + 1] - row_y[r as usize]),
                None => (col_x[c as usize], col_x[c as usize + 1], row_y[r as usize], row_y[r as usize + 1] - row_y[r as usize]),
            };
            push_text(&mut dl, font, &text, opts.font_size, left, right, top, height, align, CELL_TEXT);
        }
    }

    dl
}

fn read_values(bytes: &[u8], sheet_index: usize) -> HashMap<(u32, u32), (String, Align)> {
    let mut map = HashMap::new();
    let mut wb = match Xlsx::new(Cursor::new(bytes.to_vec())) {
        Ok(w) => w,
        Err(_) => return map,
    };
    let names = wb.sheet_names().to_vec();
    let name = match names.get(sheet_index) {
        Some(n) => n.clone(),
        None => return map,
    };
    let range = match wb.worksheet_range(&name) {
        Ok(r) => r,
        Err(_) => return map,
    };
    let (sr, sc) = range.start().unwrap_or((0, 0));
    for (rr, rc, cell) in range.cells() {
        if let Some(v) = format_cell(cell) {
            map.insert((sr + rr as u32, sc + rc as u32), v);
        }
    }
    map
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

fn rect_stroke(x: f32, y: f32, w: f32, h: f32, color: Color) -> Command {
    let mut p = PathData::new();
    p.move_to(x, y);
    p.line_to(x + w, y);
    p.line_to(x + w, y + h);
    p.line_to(x, y + h);
    p.close();
    Command::StrokePath { path: p, paint: Paint::Solid(color), width: 1.0, transform: Transform::IDENTITY }
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
            break;
        }
        glyphs.push(PositionedGlyph { id: g.glyph_id, x: x + g.x_offset * scale, y: baseline - g.y_offset * scale });
        x += adv;
    }
    if !glyphs.is_empty() {
        dl.push(Command::Glyphs(GlyphRun { font: FontId(0), size, paint: Paint::Solid(color), glyphs }));
    }
}
