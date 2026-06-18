//! Self-written XLSX grid renderer.
//!
//! Values come from `calamine`; the visual layer — column widths, row heights,
//! merged cells, cell styles (fills, fonts, borders, alignment) and number
//! formats — is parsed from the OOXML in [`model`] and lowered, with the values,
//! into the shared [`dv_ir::DisplayList`]. The first format rendered entirely by
//! our own code over the Rust geba.

mod model;

use std::collections::{HashMap, HashSet};
use std::io::Cursor;

use calamine::{Data, Reader, Xlsx};

use dv_ir::{Color, Command, DisplayList, FillRule, FontId, GlyphRun, Paint, PathData, PositionedGlyph, Transform};
use dv_text::{shape, FontData};
use model::{HAlign, Xf};

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

enum CellVal {
    Text(String),
    Num(f64),
    Bool(bool),
    Err(String),
}

const GRID: Color = Color::rgb(0xC8, 0xC8, 0xC8);
const BORDER: Color = Color::rgb(0x3C, 0x3F, 0x44);
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
    let parsed = model::parse(bytes, sheet_index, opts.col_width, opts.row_height);
    let geom = &parsed.geom;
    let styles = &parsed.styles;

    let mut ext_rows = geom.n_rows;
    let mut ext_cols = geom.n_cols;
    for &(r, c) in values.keys() {
        ext_rows = ext_rows.max(r + 1);
        ext_cols = ext_cols.max(c + 1);
    }
    let nrows = (ext_rows as usize).min(opts.max_rows).max(1);
    let ncols = (ext_cols as usize).min(opts.max_cols).max(1);

    let hw = opts.header_w;
    let hh = opts.row_height;

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

    // Merge bookkeeping.
    let mut covered: HashSet<(u32, u32)> = HashSet::new();
    let mut anchor_span: HashMap<(u32, u32), (u32, u32)> = HashMap::new();
    for &(r0, c0, mut r1, mut c1) in &geom.merges {
        if r0 as usize >= nrows || c0 as usize >= ncols {
            continue;
        }
        r1 = (r1 as usize).min(nrows - 1) as u32;
        c1 = (c1 as usize).min(ncols - 1) as u32;
        for r in r0..=r1 {
            for c in c0..=c1 {
                covered.insert((r, c));
            }
        }
        covered.remove(&(r0, c0));
        anchor_span.insert((r0, c0), (r1, c1));
    }

    let xf_of = |r: u32, c: u32| -> Option<&Xf> {
        geom.cell_xf.get(&(r, c)).and_then(|i| styles.xfs.get(*i as usize))
    };
    let rect_of = |r: u32, c: u32| -> (f32, f32, f32, f32) {
        let (er, ec) = anchor_span.get(&(r, c)).copied().unwrap_or((r, c));
        (col_x[c as usize], col_x[ec as usize + 1], row_y[r as usize], row_y[er as usize + 1])
    };

    // 1) Header bands.
    dl.push(fill_rect(0.0, 0.0, total_w, hh, HEADER_FILL));
    dl.push(fill_rect(0.0, 0.0, hw, total_h, HEADER_FILL));

    // 2) Grid lines.
    dl.push(vline(0.0, 0.0, total_h));
    for c in 0..=ncols {
        dl.push(vline(col_x[c], 0.0, total_h));
    }
    dl.push(hline(0.0, total_w, 0.0));
    for r in 0..=nrows {
        dl.push(hline(0.0, total_w, row_y[r]));
    }

    // 3) Merged regions: cover interior grid lines with the anchor fill (or white).
    for (&(r0, c0), _) in anchor_span.iter() {
        let (x0, x1, y0, y1) = rect_of(r0, c0);
        let fill = xf_of(r0, c0).and_then(|xf| xf.fill).unwrap_or(Color::WHITE);
        dl.push(fill_rect(x0, y0, x1 - x0, y1 - y0, fill));
    }

    // 4) Cell fills (non-merged styled cells).
    for (&(r, c), _) in geom.cell_xf.iter() {
        if covered.contains(&(r, c)) || anchor_span.contains_key(&(r, c)) || r as usize >= nrows || c as usize >= ncols {
            continue;
        }
        if let Some(fill) = xf_of(r, c).and_then(|xf| xf.fill) {
            let (x0, x1, y0, y1) = rect_of(r, c);
            dl.push(fill_rect(x0, y0, x1 - x0, y1 - y0, fill));
        }
    }

    // 5) Borders: explicit cell borders, then merge outlines on top.
    for (&(r, c), _) in geom.cell_xf.iter() {
        if covered.contains(&(r, c)) || r as usize >= nrows || c as usize >= ncols {
            continue;
        }
        if let Some(xf) = xf_of(r, c) {
            let b = xf.border;
            if b.left || b.right || b.top || b.bottom {
                let (x0, x1, y0, y1) = rect_of(r, c);
                if b.top {
                    dl.push(seg(x0, y0, x1, y0));
                }
                if b.bottom {
                    dl.push(seg(x0, y1, x1, y1));
                }
                if b.left {
                    dl.push(seg(x0, y0, x0, y1));
                }
                if b.right {
                    dl.push(seg(x1, y0, x1, y1));
                }
            }
        }
    }
    for (&(r0, c0), _) in anchor_span.iter() {
        let (x0, x1, y0, y1) = rect_of(r0, c0);
        dl.push(rect_stroke(x0, y0, x1 - x0, y1 - y0, MERGE_BORDER));
    }

    // 6) Header labels.
    for c in 0..ncols {
        push_text(&mut dl, font, &col_letter(c), opts.font_size, col_x[c], col_x[c + 1], 0.0, hh, Align::Center, HEADER_TEXT, false);
    }
    for r in 0..nrows {
        push_text(&mut dl, font, &(r + 1).to_string(), opts.font_size, 0.0, hw, row_y[r], row_y[r + 1] - row_y[r], Align::Center, HEADER_TEXT, false);
    }

    // 7) Cell text.
    for (&(r, c), val) in values.iter() {
        if covered.contains(&(r, c)) || r as usize >= nrows || c as usize >= ncols {
            continue;
        }
        let xf = xf_of(r, c);
        let text = display_text(val, xf);
        if text.is_empty() {
            continue;
        }
        let align = xf
            .and_then(|x| x.h_align)
            .map(map_align)
            .unwrap_or_else(|| default_align(val));
        let color = xf.and_then(|x| x.font.color).unwrap_or(CELL_TEXT);
        let bold = xf.map(|x| x.font.bold).unwrap_or(false);
        let (x0, x1, y0, y1) = rect_of(r, c);
        push_text(&mut dl, font, &text, opts.font_size, x0, x1, y0, y1 - y0, align, color, bold);
    }

    dl
}

fn map_align(h: HAlign) -> Align {
    match h {
        HAlign::Left => Align::Left,
        HAlign::Center => Align::Center,
        HAlign::Right => Align::Right,
    }
}

fn default_align(v: &CellVal) -> Align {
    match v {
        CellVal::Text(_) => Align::Left,
        CellVal::Num(_) => Align::Right,
        CellVal::Bool(_) | CellVal::Err(_) => Align::Center,
    }
}

fn display_text(v: &CellVal, xf: Option<&Xf>) -> String {
    match v {
        CellVal::Text(s) => s.clone(),
        CellVal::Num(n) => {
            let code = xf.map(|x| x.fmt_code.as_str()).unwrap_or("General");
            format_value(*n, code)
        }
        CellVal::Bool(b) => (if *b { "TRUE" } else { "FALSE" }).to_string(),
        CellVal::Err(e) => e.clone(),
    }
}

fn read_values(bytes: &[u8], sheet_index: usize) -> HashMap<(u32, u32), CellVal> {
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
        let v = match cell {
            Data::Empty => continue,
            Data::String(s) if s.is_empty() => continue,
            Data::String(s) => CellVal::Text(s.clone()),
            Data::Float(f) => CellVal::Num(*f),
            Data::Int(i) => CellVal::Num(*i as f64),
            Data::Bool(b) => CellVal::Bool(*b),
            Data::DateTime(dt) => CellVal::Num(dt.as_f64()),
            Data::DateTimeIso(s) => CellVal::Text(s.clone()),
            Data::DurationIso(s) => CellVal::Text(s.clone()),
            Data::Error(e) => CellVal::Err(format!("{:?}", e)),
        };
        map.insert((sr + rr as u32, sc + rc as u32), v);
    }
    map
}

// --- number format engine (a pragmatic subset) ----------------------------

fn format_value(v: f64, code: &str) -> String {
    let code = code.trim();
    if code.is_empty() || code.eq_ignore_ascii_case("general") || code == "@" {
        return general_number(v);
    }
    let sect = code.split(';').next().unwrap_or(code).trim();
    if looks_like_date(sect) {
        return format_date(v, sect);
    }
    let percent = sect.contains('%');
    let val = if percent { v * 100.0 } else { v };
    let decimals = decimals_in(sect);
    let thousands = sect.contains("#,#") || sect.contains("0,0");
    let mut out = format_fixed(val, decimals, thousands);
    if sect.contains('$') {
        out = format!("${}", out);
    }
    if percent {
        out.push('%');
    }
    out
}

fn general_number(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        format!("{}", f)
    }
}

fn decimals_in(sect: &str) -> usize {
    match sect.split_once('.') {
        Some((_, frac)) => frac.chars().take_while(|c| matches!(c, '0' | '#')).count(),
        None => 0,
    }
}

fn format_fixed(val: f64, decimals: usize, thousands: bool) -> String {
    let neg = val < 0.0;
    let s = format!("{:.*}", decimals, val.abs());
    let (int_part, frac_part) = match s.split_once('.') {
        Some((i, f)) => (i.to_string(), Some(f.to_string())),
        None => (s, None),
    };
    let int_part = if thousands { group_thousands(&int_part) } else { int_part };
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    out.push_str(&int_part);
    if let Some(f) = frac_part {
        out.push('.');
        out.push_str(&f);
    }
    out
}

fn group_thousands(s: &str) -> String {
    let len = s.chars().count();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn looks_like_date(sect: &str) -> bool {
    let lower = sect.to_ascii_lowercase();
    lower.contains('y') || lower.contains('d') || lower.contains("mmm")
}

fn format_date(v: f64, pattern: &str) -> String {
    let (y, m, d) = ymd_from_excel_serial(v.trunc() as i64);
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        let lc = ch.to_ascii_lowercase();
        if lc == 'y' || lc == 'm' || lc == 'd' {
            let mut n = 0;
            while i < chars.len() && chars[i].to_ascii_lowercase() == lc {
                n += 1;
                i += 1;
            }
            match lc {
                'y' => out.push_str(&if n >= 4 { format!("{:04}", y) } else { format!("{:02}", y.rem_euclid(100)) }),
                'm' => out.push_str(&if n >= 2 { format!("{:02}", m) } else { format!("{}", m) }),
                _ => out.push_str(&if n >= 2 { format!("{:02}", d) } else { format!("{}", d) }),
            }
        } else if ch == '"' {
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                out.push(chars[i]);
                i += 1;
            }
            if i < chars.len() {
                i += 1;
            }
        } else if ch == '[' {
            while i < chars.len() && chars[i] != ']' {
                i += 1;
            }
            if i < chars.len() {
                i += 1;
            }
        } else if lc == 'h' || lc == 's' || ch == '\\' {
            i += 1; // time tokens / escapes not handled
        } else {
            out.push(ch);
            i += 1;
        }
    }
    out
}

fn ymd_from_excel_serial(serial: i64) -> (i64, i64, i64) {
    // Excel 1900 system: serial 0 == 1899-12-30; Unix epoch == serial 25569.
    civil_from_days(serial - 25569)
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    // Howard Hinnant's algorithm; z = days since 1970-01-01.
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (y + if m <= 2 { 1 } else { 0 }, m, d)
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

fn seg(x0: f32, y0: f32, x1: f32, y1: f32) -> Command {
    let mut p = PathData::new();
    p.move_to(x0, y0);
    p.line_to(x1, y1);
    Command::StrokePath { path: p, paint: Paint::Solid(BORDER), width: 1.0, transform: Transform::IDENTITY }
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
    bold: bool,
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
        dl.push(Command::Glyphs(GlyphRun { font: FontId(0), size, paint: Paint::Solid(color), bold, glyphs }));
    }
}
