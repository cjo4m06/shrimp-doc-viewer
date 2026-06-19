//! Self-written XLSX grid renderer.
//!
//! Values come from `calamine`; the visual layer — column widths, row heights,
//! merged cells, cell styles (fills, fonts, borders, alignment) and number
//! formats — is parsed from the OOXML in [`model`]. A sheet is parsed once into
//! an owned [`Sheet`] and rendered by *viewport*: only the cells intersecting a
//! scroll window are lowered into the shared [`dv_ir::DisplayList`], with frozen
//! column/row headers painted on top. This is what lets large grids scroll and
//! zoom without rendering everything.

mod model;

use std::collections::{HashMap, HashSet};
use std::io::Cursor;

use calamine::{Data, Reader, Xlsx};

use dv_ir::{Color, Command, DisplayList, FillRule, FontId, GlyphRun, Paint, PathData, PositionedGlyph, Transform};
use dv_text::{shape, FontData};
use model::{HAlign, Xf};

#[derive(Clone, Copy)]
pub struct Options {
    /// Caps for the prefix-sum arrays (guards pathological dimensions).
    pub max_rows: usize,
    pub max_cols: usize,
    pub col_width: f32,
    pub row_height: f32,
    pub header_w: f32,
    pub font_size: f32,
}

impl Default for Options {
    fn default() -> Self {
        Self { max_rows: 100_000, max_cols: 1024, col_width: 64.0, row_height: 20.0, header_w: 44.0, font_size: 13.0 }
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

/// A worksheet parsed into an owned model, ready for repeated viewport renders.
pub struct Sheet {
    pub name: String,
    values: HashMap<(u32, u32), CellVal>,
    cell_xf: HashMap<(u32, u32), u32>,
    xfs: Vec<Xf>,
    anchor_span: HashMap<(u32, u32), (u32, u32)>,
    covered: HashSet<(u32, u32)>,
    /// Column boundaries in data px (len n_cols+1, col_x[0] = 0).
    col_x: Vec<f32>,
    /// Row boundaries in data px (len n_rows+1, row_y[0] = 0).
    row_y: Vec<f32>,
    n_rows: u32,
    n_cols: u32,
    header_w: f32,
    header_h: f32,
    font_size: f32,
}

/// Sheet names in workbook order (empty if the bytes aren't a readable xlsx).
pub fn sheet_names(bytes: &[u8]) -> Vec<String> {
    match Xlsx::new(Cursor::new(bytes.to_vec())) {
        Ok(wb) => wb.sheet_names().to_vec(),
        Err(_) => Vec::new(),
    }
}

/// Parse delimited text (CSV / TSV / semicolon) per RFC 4180: the delimiter is
/// auto-detected from the first non-empty line; quoted fields may contain the
/// delimiter, newlines and `""`-escaped quotes; CRLF and LF both end a record.
fn parse_delimited(bytes: &[u8]) -> Vec<Vec<String>> {
    const MAX_ROWS: usize = 1_000_000;
    const MAX_COLS: usize = 4096;
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    if text.starts_with('\u{feff}') {
        text.remove(0); // strip UTF-8 BOM
    }
    // Choose the delimiter from the first several non-comment lines, preferring the
    // candidate that yields the most consistent field count (not just the first line).
    let sample: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#')).take(10).collect();
    let score = |d: char| -> (usize, usize) {
        use std::collections::HashMap as M;
        let mut counts: M<usize, usize> = M::new();
        let mut with = 0usize;
        for l in &sample {
            let n = l.matches(d).count();
            if n >= 1 {
                with += 1;
            }
            *counts.entry(n).or_insert(0) += 1;
        }
        let consistency = counts.values().copied().max().unwrap_or(0);
        (with, consistency)
    };
    let delim = [',', '\t', ';']
        .into_iter()
        .filter(|&d| score(d).0 > 0)
        .max_by_key(|&d| score(d))
        .unwrap_or(',');

    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        if rows.len() >= MAX_ROWS {
            break;
        }
        if quoted {
            if c == '"' {
                if it.peek() == Some(&'"') {
                    field.push('"');
                    it.next();
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
        } else if c == '"' {
            quoted = true;
        } else if c == delim {
            if row.len() < MAX_COLS {
                row.push(std::mem::take(&mut field));
            } else {
                field.clear();
            }
        } else if c == '\n' || c == '\r' {
            if c == '\r' && it.peek() == Some(&'\n') {
                it.next();
            }
            if row.len() < MAX_COLS {
                row.push(std::mem::take(&mut field));
            }
            field.clear();
            rows.push(std::mem::take(&mut row));
        } else {
            field.push(c);
        }
    }
    if (!field.is_empty() || !row.is_empty()) && rows.len() < MAX_ROWS {
        if row.len() < MAX_COLS {
            row.push(field);
        }
        rows.push(row);
    }
    rows
}

impl Sheet {
    /// Parse one sheet into an owned model.
    pub fn parse(bytes: &[u8], sheet_index: usize, opts: &Options) -> Sheet {
        let name = sheet_names(bytes).into_iter().nth(sheet_index).unwrap_or_default();
        let values = read_values(bytes, sheet_index);
        let parsed = model::parse(bytes, sheet_index, opts.col_width, opts.row_height);
        let geom = &parsed.geom;

        let mut ext_rows = geom.n_rows;
        let mut ext_cols = geom.n_cols;
        for &(r, c) in values.keys() {
            ext_rows = ext_rows.max(r + 1);
            ext_cols = ext_cols.max(c + 1);
        }
        let n_rows = (ext_rows as usize).min(opts.max_rows).max(1) as u32;
        let n_cols = (ext_cols as usize).min(opts.max_cols).max(1) as u32;

        let mut col_x = Vec::with_capacity(n_cols as usize + 1);
        col_x.push(0.0);
        for c in 0..n_cols {
            col_x.push(col_x[c as usize] + geom.col_width(c));
        }
        let mut row_y = Vec::with_capacity(n_rows as usize + 1);
        row_y.push(0.0);
        for r in 0..n_rows {
            row_y.push(row_y[r as usize] + geom.row_height(r));
        }

        let mut covered = HashSet::new();
        let mut anchor_span = HashMap::new();
        for &(r0, c0, r1, c1) in &geom.merges {
            if r0 >= n_rows || c0 >= n_cols {
                continue;
            }
            let r1 = r1.min(n_rows - 1);
            let c1 = c1.min(n_cols - 1);
            for r in r0..=r1 {
                for c in c0..=c1 {
                    covered.insert((r, c));
                }
            }
            covered.remove(&(r0, c0));
            anchor_span.insert((r0, c0), (r1, c1));
        }

        Sheet {
            name,
            values,
            cell_xf: parsed.geom.cell_xf,
            xfs: parsed.styles.xfs,
            anchor_span,
            covered,
            col_x,
            row_y,
            n_rows,
            n_cols,
            header_w: opts.header_w,
            header_h: opts.row_height,
            font_size: opts.font_size,
        }
    }

    /// Build a grid sheet from CSV / TSV / semicolon-delimited bytes. No styles or
    /// merges; cells that parse as numbers right-align, columns auto-size to content.
    /// Reuses the same viewport renderer as xlsx.
    pub fn from_csv(bytes: &[u8], opts: &Options) -> Sheet {
        Sheet::from_rows(parse_delimited(bytes), opts)
    }

    /// Build a grid sheet from already-parsed rows of strings (CSV, ODS, …).
    pub fn from_rows(rows: Vec<Vec<String>>, opts: &Options) -> Sheet {
        // `.min(.max(1))` (not clamp) so a caller-set max of 0 can't panic (lo>hi).
        let n_rows = rows.len().max(1).min(opts.max_rows.max(1)) as u32;
        let n_cols = rows.iter().map(Vec::len).max().unwrap_or(1).max(1).min(opts.max_cols.max(1)) as u32;

        let mut values = HashMap::new();
        let mut col_chars = vec![0usize; n_cols as usize];
        for (r, row) in rows.iter().enumerate().take(n_rows as usize) {
            for (c, cell) in row.iter().enumerate().take(n_cols as usize) {
                let t = cell.trim();
                col_chars[c] = col_chars[c].max(t.chars().count());
                if t.is_empty() {
                    continue;
                }
                // Treat as a number only when it cleanly parses and isn't an
                // identifier-like leading-zero integer (e.g. "007", phone numbers).
                let numeric = t
                    .parse::<f64>()
                    .ok()
                    .filter(|n| n.is_finite())
                    .filter(|_| t == "0" || t.contains('.') || !t.starts_with('0'));
                let v = match numeric {
                    Some(n) => CellVal::Num(n),
                    // cap pathological cell length (a cell can't display 1M chars;
                    // shaping the full string every frame would hang) — char-safe slice.
                    None if t.chars().count() > 2000 => CellVal::Text(t.chars().take(2000).collect()),
                    None => CellVal::Text(t.to_string()),
                };
                values.insert((r as u32, c as u32), v);
            }
        }

        let mut col_x = Vec::with_capacity(n_cols as usize + 1);
        col_x.push(0.0);
        for c in 0..n_cols as usize {
            let w = (col_chars[c] as f32 * opts.font_size * 0.62 + 12.0).clamp(opts.col_width, 480.0);
            col_x.push(col_x[c] + w);
        }
        let mut row_y = Vec::with_capacity(n_rows as usize + 1);
        row_y.push(0.0);
        for r in 0..n_rows as usize {
            row_y.push(row_y[r] + opts.row_height);
        }

        Sheet {
            name: "CSV".to_string(),
            values,
            cell_xf: HashMap::new(),
            xfs: Vec::new(),
            anchor_span: HashMap::new(),
            covered: HashSet::new(),
            col_x,
            row_y,
            n_rows,
            n_cols,
            header_w: opts.header_w,
            header_h: opts.row_height,
            font_size: opts.font_size,
        }
    }

    pub fn total_w(&self) -> f32 {
        self.col_x[self.n_cols as usize]
    }
    pub fn total_h(&self) -> f32 {
        self.row_y[self.n_rows as usize]
    }
    pub fn header_w(&self) -> f32 {
        self.header_w
    }
    pub fn header_h(&self) -> f32 {
        self.header_h
    }

    fn xf_of(&self, r: u32, c: u32) -> Option<&Xf> {
        self.cell_xf.get(&(r, c)).and_then(|i| self.xfs.get(*i as usize))
    }

    /// Render the viewport at `(scroll_x, scroll_y)` (data px) into a
    /// `dev_w`×`dev_h` device-pixel surface, scaled by `scale` (= zoom × dpr).
    /// Column/row headers are frozen (painted last, over scrolled content).
    pub fn render_viewport(&self, font: &FontData, scroll_x: f32, scroll_y: f32, dev_w: f32, dev_h: f32, scale: f32) -> DisplayList {
        let mut dl = DisplayList::new(dev_w.max(1.0), dev_h.max(1.0));
        let hw = self.header_w * scale;
        let hh = self.header_h * scale;
        let content_w = (dev_w / scale - self.header_w).max(0.0);
        let content_h = (dev_h / scale - self.header_h).max(0.0);

        let (c0, c1) = visible_range(&self.col_x, scroll_x, content_w, self.n_cols as usize);
        let (r0, r1) = visible_range(&self.row_y, scroll_y, content_h, self.n_rows as usize);

        let dx = |cx: f32| hw + (cx - scroll_x) * scale;
        let dy = |cy: f32| hh + (cy - scroll_y) * scale;

        // 1) Grid lines (data area).
        for c in c0..=c1.min(self.n_cols as usize) {
            dl.push(vline(dx(self.col_x[c]), hh, dev_h, GRID));
        }
        for r in r0..=r1.min(self.n_rows as usize) {
            dl.push(hline(hw, dev_w, dy(self.row_y[r]), GRID));
        }

        // 2) Cell fills (cover grid lines where present).
        for r in r0..r1 {
            for c in c0..c1 {
                let (r, c) = (r as u32, c as u32);
                if self.covered.contains(&(r, c)) || self.anchor_span.contains_key(&(r, c)) {
                    continue;
                }
                if let Some(fill) = self.xf_of(r, c).and_then(|x| x.fill) {
                    let (x0, x1, y0, y1) = (dx(self.col_x[c as usize]), dx(self.col_x[c as usize + 1]), dy(self.row_y[r as usize]), dy(self.row_y[r as usize + 1]));
                    dl.push(fill_rect(x0, y0, x1 - x0, y1 - y0, fill));
                }
            }
        }

        // 3) Merges that intersect the viewport: fill (anchor colour/white) + border + text.
        for (&(ar, ac), &(er, ec)) in self.anchor_span.iter() {
            if (ar as usize) >= r1 || (er as usize) < r0 || (ac as usize) >= c1 || (ec as usize) < c0 {
                continue;
            }
            let (x0, x1, y0, y1) = (dx(self.col_x[ac as usize]), dx(self.col_x[ec as usize + 1]), dy(self.row_y[ar as usize]), dy(self.row_y[er as usize + 1]));
            let fill = self.xf_of(ar, ac).and_then(|x| x.fill).unwrap_or(Color::WHITE);
            dl.push(fill_rect(x0, y0, x1 - x0, y1 - y0, fill));
            dl.push(rect_stroke(x0, y0, x1 - x0, y1 - y0, MERGE_BORDER));
            if let Some(v) = self.values.get(&(ar, ac)) {
                self.push_cell_text(&mut dl, font, v, ar, ac, x0, x1, y0, y1, scale);
            }
        }

        // 4) Explicit cell borders.
        for r in r0..r1 {
            for c in c0..c1 {
                let (r, c) = (r as u32, c as u32);
                if self.covered.contains(&(r, c)) {
                    continue;
                }
                if let Some(b) = self.xf_of(r, c).map(|x| x.border) {
                    if b.left || b.right || b.top || b.bottom {
                        let (x0, x1, y0, y1) = (dx(self.col_x[c as usize]), dx(self.col_x[c as usize + 1]), dy(self.row_y[r as usize]), dy(self.row_y[r as usize + 1]));
                        if b.top {
                            dl.push(seg(x0, y0, x1, y0, BORDER));
                        }
                        if b.bottom {
                            dl.push(seg(x0, y1, x1, y1, BORDER));
                        }
                        if b.left {
                            dl.push(seg(x0, y0, x0, y1, BORDER));
                        }
                        if b.right {
                            dl.push(seg(x1, y0, x1, y1, BORDER));
                        }
                    }
                }
            }
        }

        // 5) Cell text (non-merged).
        for r in r0..r1 {
            for c in c0..c1 {
                let (r, c) = (r as u32, c as u32);
                if self.covered.contains(&(r, c)) || self.anchor_span.contains_key(&(r, c)) {
                    continue;
                }
                if let Some(v) = self.values.get(&(r, c)) {
                    let (x0, x1, y0, y1) = (dx(self.col_x[c as usize]), dx(self.col_x[c as usize + 1]), dy(self.row_y[r as usize]), dy(self.row_y[r as usize + 1]));
                    self.push_cell_text(&mut dl, font, v, r, c, x0, x1, y0, y1, scale);
                }
            }
        }

        // 6) Frozen headers (painted last, over any scrolled-under content).
        dl.push(fill_rect(0.0, 0.0, dev_w, hh, HEADER_FILL));
        dl.push(fill_rect(0.0, 0.0, hw, dev_h, HEADER_FILL));
        dl.push(vline(0.0, 0.0, dev_h, GRID));
        dl.push(vline(hw, 0.0, dev_h, GRID));
        dl.push(hline(0.0, dev_w, 0.0, GRID));
        dl.push(hline(0.0, dev_w, hh, GRID));
        let hsize = self.font_size * scale;
        for c in c0..c1 {
            let (x0, x1) = (dx(self.col_x[c]), dx(self.col_x[c + 1]));
            dl.push(vline(x1, 0.0, hh, GRID));
            push_text(&mut dl, font, &col_letter(c), hsize, x0, x1, 0.0, hh, Align::Center, HEADER_TEXT, false);
        }
        for r in r0..r1 {
            let (y0, y1) = (dy(self.row_y[r]), dy(self.row_y[r + 1]));
            dl.push(hline(0.0, hw, y1, GRID));
            push_text(&mut dl, font, &(r + 1).to_string(), hsize, 0.0, hw, y0, y1 - y0, Align::Center, HEADER_TEXT, false);
        }

        dl
    }

    #[allow(clippy::too_many_arguments)]
    fn push_cell_text(&self, dl: &mut DisplayList, font: &FontData, v: &CellVal, r: u32, c: u32, x0: f32, x1: f32, y0: f32, y1: f32, scale: f32) {
        let xf = self.xf_of(r, c);
        let text = display_text(v, xf);
        if text.is_empty() {
            return;
        }
        let align = xf.and_then(|x| x.h_align).map(map_align).unwrap_or_else(|| default_align(v));
        let color = xf.and_then(|x| x.font.color).unwrap_or(CELL_TEXT);
        let bold = xf.map(|x| x.font.bold).unwrap_or(false);
        push_text(dl, font, &text, self.font_size * scale, x0, x1, y0, y1 - y0, align, color, bold);
    }
}

/// Visible cell index range `[i0, i1)` for a prefix-sum boundary array.
fn visible_range(prefix: &[f32], start: f32, span: f32, n: usize) -> (usize, usize) {
    if n == 0 {
        return (0, 0);
    }
    let end = start + span;
    let i0 = prefix.partition_point(|&x| x <= start).saturating_sub(1).min(n - 1);
    let i1 = prefix.partition_point(|&x| x < end).min(n);
    (i0, i1.max(i0 + 1))
}

/// Convenience: render an entire sheet (no scrolling) at 1×. Used by the native
/// demo and the non-virtualized `render_xlsx` path.
pub fn render_sheet(bytes: &[u8], sheet_index: usize, font: &FontData, opts: &Options) -> DisplayList {
    let sheet = Sheet::parse(bytes, sheet_index, opts);
    let dev_w = (sheet.header_w + sheet.total_w()).ceil();
    let dev_h = (sheet.header_h + sheet.total_h()).ceil();
    sheet.render_viewport(font, 0.0, 0.0, dev_w, dev_h, 1.0)
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
        CellVal::Num(n) => format_value(*n, xf.map(|x| x.fmt_code.as_str()).unwrap_or("General")),
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
            i += 1;
        } else {
            out.push(ch);
            i += 1;
        }
    }
    out
}

fn ymd_from_excel_serial(serial: i64) -> (i64, i64, i64) {
    civil_from_days(serial - 25569)
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
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

fn seg(x0: f32, y0: f32, x1: f32, y1: f32, color: Color) -> Command {
    let mut p = PathData::new();
    p.move_to(x0, y0);
    p.line_to(x1, y1);
    Command::StrokePath { path: p, paint: Paint::Solid(color), width: 1.0, transform: Transform::IDENTITY }
}

fn vline(x: f32, y0: f32, y1: f32, color: Color) -> Command {
    seg(x, y0, x, y1, color)
}

fn hline(x0: f32, x1: f32, y: f32, color: Color) -> Command {
    seg(x0, y, x1, y, color)
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
