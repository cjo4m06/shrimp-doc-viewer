//! OOXML geometry parser — the layout layer calamine does not expose: column
//! widths, row heights, and merged-cell ranges, parsed from `xl/worksheets/*.xml`
//! with quick-xml. Values still come from calamine; this adds the structure that
//! makes a sheet look like a sheet. Styles (fills/fonts/borders/number formats)
//! are layered on in a later step.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use dv_ir::Color;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use zip::ZipArchive;

type Zip = ZipArchive<Cursor<Vec<u8>>>;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum HAlign {
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, Default)]
pub struct FontStyle {
    pub bold: bool,
    pub size: Option<f32>,
    pub color: Option<Color>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BorderEdges {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

/// A resolved cell format (one entry of `cellXfs`).
#[derive(Clone, Debug, Default)]
pub struct Xf {
    pub fmt_code: String,
    pub font: FontStyle,
    pub fill: Option<Color>,
    pub border: BorderEdges,
    pub h_align: Option<HAlign>,
}

#[derive(Default)]
pub struct Styles {
    pub xfs: Vec<Xf>,
}

/// Everything parsed for one sheet.
pub struct Parsed {
    pub geom: Geometry,
    pub styles: Styles,
}

#[derive(Default)]
pub struct Geometry {
    /// Explicit column-width ranges as (min_col_1based, max_col_1based, px).
    pub col_ranges: Vec<(u32, u32, f32)>,
    pub default_col_px: f32,
    /// Explicit row heights keyed by 1-based row index, in px.
    pub row_px: HashMap<u32, f32>,
    pub default_row_px: f32,
    /// Merged regions as (row0, col0, row1, col1), 0-based inclusive.
    pub merges: Vec<(u32, u32, u32, u32)>,
    /// Per-cell style index into [`Styles::xfs`], keyed by (row0, col0).
    pub cell_xf: HashMap<(u32, u32), u32>,
    /// Extents (0-based exclusive counts) discovered from dimension/rows/cells.
    pub n_rows: u32,
    pub n_cols: u32,
}

impl Geometry {
    pub fn col_width(&self, col0: u32) -> f32 {
        let c = col0 + 1;
        for &(min, max, px) in &self.col_ranges {
            if c >= min && c <= max {
                return px;
            }
        }
        self.default_col_px
    }
    pub fn row_height(&self, row0: u32) -> f32 {
        *self.row_px.get(&(row0 + 1)).unwrap_or(&self.default_row_px)
    }
}

fn get_attr(e: &BytesStart, key: &[u8]) -> Option<String> {
    for a in e.attributes().flatten() {
        if a.key.as_ref() == key {
            return Some(String::from_utf8_lossy(a.value.as_ref()).into_owned());
        }
    }
    None
}

fn read_entry(zip: &mut Zip, name: &str) -> Option<String> {
    let mut f = zip.by_name(name).ok()?;
    let mut s = String::new();
    f.read_to_string(&mut s).ok()?;
    Some(s)
}

/// "A1" -> (row0, col0). "12" or "A" tolerated.
fn parse_ref(s: &str) -> (u32, u32) {
    let b = s.as_bytes();
    let mut i = 0;
    let mut col = 0u32;
    while i < b.len() && b[i].is_ascii_alphabetic() {
        col = col * 26 + (b[i].to_ascii_uppercase() - b'A' + 1) as u32;
        i += 1;
    }
    let row: u32 = s[i..].parse().unwrap_or(1);
    (row.saturating_sub(1), col.saturating_sub(1))
}

fn parse_range(s: &str) -> (u32, u32, u32, u32) {
    let mut it = s.split(':');
    let a = parse_ref(it.next().unwrap_or("A1"));
    let b = it.next().map(parse_ref).unwrap_or(a);
    (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1))
}

/// Ordered worksheet rIds from xl/workbook.xml.
fn workbook_rids(xml: &str) -> Vec<String> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if e.name().as_ref() == b"sheet" => {
                if let Some(rid) = get_attr(&e, b"r:id") {
                    out.push(rid);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

/// rId -> Target from xl/_rels/workbook.xml.rels.
fn rels_map(xml: &str) -> HashMap<String, String> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut map = HashMap::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if e.name().as_ref() == b"Relationship" => {
                if let (Some(id), Some(t)) = (get_attr(&e, b"Id"), get_attr(&e, b"Target")) {
                    map.insert(id, t);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    map
}

fn sheet_path(zip: &mut Zip, index: usize) -> Option<String> {
    let wb = read_entry(zip, "xl/workbook.xml")?;
    let rels = read_entry(zip, "xl/_rels/workbook.xml.rels")?;
    let rid = workbook_rids(&wb).into_iter().nth(index)?;
    let target = rels_map(&rels).remove(&rid)?;
    Some(if let Some(stripped) = target.strip_prefix('/') {
        stripped.to_string()
    } else {
        format!("xl/{}", target)
    })
}

fn parse_worksheet(xml: &str, default_col_px: f32, default_row_px: f32) -> Geometry {
    let mut g = Geometry { default_col_px, default_row_px, ..Default::default() };
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut max_row = 0u32;
    let mut max_col = 0u32;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match e.name().as_ref() {
                b"dimension" => {
                    if let Some(r) = get_attr(&e, b"ref") {
                        let (_, _, r1, c1) = parse_range(&r);
                        max_row = max_row.max(r1 + 1);
                        max_col = max_col.max(c1 + 1);
                    }
                }
                b"col" => {
                    let min = get_attr(&e, b"min").and_then(|s| s.parse().ok()).unwrap_or(1);
                    let max = get_attr(&e, b"max").and_then(|s| s.parse().ok()).unwrap_or(min);
                    if let Some(w) = get_attr(&e, b"width").and_then(|s| s.parse::<f32>().ok()) {
                        // Excel char width -> px (Calibri 11 approximation).
                        g.col_ranges.push((min, max, w * 7.0 + 5.0));
                    }
                }
                b"row" => {
                    if let Some(r) = get_attr(&e, b"r").and_then(|s| s.parse::<u32>().ok()) {
                        max_row = max_row.max(r);
                        if let Some(ht) = get_attr(&e, b"ht").and_then(|s| s.parse::<f32>().ok()) {
                            g.row_px.insert(r, ht * 96.0 / 72.0);
                        }
                    }
                }
                b"c" => {
                    if let Some(rf) = get_attr(&e, b"r") {
                        let (r, c) = parse_ref(&rf);
                        max_row = max_row.max(r + 1);
                        max_col = max_col.max(c + 1);
                        if let Some(s) = get_attr(&e, b"s").and_then(|s| s.parse::<u32>().ok()) {
                            g.cell_xf.insert((r, c), s);
                        }
                    }
                }
                b"mergeCell" => {
                    if let Some(r) = get_attr(&e, b"ref") {
                        g.merges.push(parse_range(&r));
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    g.n_rows = max_row;
    g.n_cols = max_col;
    g
}

// --- styles.xml -----------------------------------------------------------

fn theme_color(idx: u32) -> Color {
    // Approximate default Office theme palette (tint applied separately).
    match idx {
        0 => Color::WHITE,                  // lt1 / background1
        1 => Color::BLACK,                  // dk1 / text1
        2 => Color::rgb(0xEE, 0xEC, 0xE1),
        3 => Color::rgb(0x1F, 0x49, 0x7D),
        4 => Color::rgb(0x4F, 0x81, 0xBD),
        5 => Color::rgb(0xC0, 0x50, 0x4D),
        6 => Color::rgb(0x9B, 0xBB, 0x59),
        7 => Color::rgb(0x80, 0x64, 0xA2),
        8 => Color::rgb(0x4B, 0xAC, 0xC6),
        9 => Color::rgb(0xF7, 0x96, 0x46),
        _ => Color::rgb(0x80, 0x80, 0x80),
    }
}

fn apply_tint(c: Color, tint: f64) -> Color {
    if tint == 0.0 {
        return c;
    }
    let f = |v: u8| -> u8 {
        let v = v as f64;
        let nv = if tint < 0.0 { v * (1.0 + tint) } else { v * (1.0 - tint) + 255.0 * tint };
        nv.round().clamp(0.0, 255.0) as u8
    };
    Color::rgba(f(c.r), f(c.g), f(c.b), c.a)
}

fn parse_color(e: &BytesStart) -> Option<Color> {
    let tint = get_attr(e, b"tint").and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
    if let Some(rgb) = get_attr(e, b"rgb") {
        let hex = rgb.trim();
        let h = if hex.len() == 8 { &hex[2..] } else { hex };
        if h.len() == 6 {
            if let (Ok(r), Ok(g), Ok(b)) = (
                u8::from_str_radix(&h[0..2], 16),
                u8::from_str_radix(&h[2..4], 16),
                u8::from_str_radix(&h[4..6], 16),
            ) {
                return Some(apply_tint(Color::rgb(r, g, b), tint));
            }
        }
    }
    if let Some(t) = get_attr(e, b"theme").and_then(|s| s.parse::<u32>().ok()) {
        return Some(apply_tint(theme_color(t), tint));
    }
    None
}

fn builtin_format(id: u32) -> &'static str {
    match id {
        1 => "0",
        2 => "0.00",
        3 => "#,##0",
        4 => "#,##0.00",
        9 => "0%",
        10 => "0.00%",
        11 => "0.00E+00",
        14 => "m/d/yyyy",
        15 => "d-mmm-yy",
        16 => "d-mmm",
        17 => "mmm-yy",
        22 => "m/d/yyyy h:mm",
        37 | 38 => "#,##0",
        39 | 40 => "#,##0.00",
        44 => "$#,##0.00",
        49 => "@",
        _ => "General",
    }
}

type XfRaw = (u32, usize, usize, usize, Option<HAlign>);

#[derive(PartialEq, Clone, Copy)]
enum Sec {
    None,
    Fonts,
    Fills,
    Borders,
    CellXfs,
}

#[derive(Default)]
struct StylesState {
    num_fmts: HashMap<u32, String>,
    fonts: Vec<FontStyle>,
    fills: Vec<Option<Color>>,
    borders: Vec<BorderEdges>,
    xf_raw: Vec<XfRaw>,
    cur_font: FontStyle,
    cur_fill: Option<Color>,
    cur_fill_solid: bool,
    cur_border: BorderEdges,
    cur_xf: Option<XfRaw>,
}

fn h_align(s: &str) -> Option<HAlign> {
    match s {
        "left" => Some(HAlign::Left),
        "center" | "centerContinuous" => Some(HAlign::Center),
        "right" => Some(HAlign::Right),
        _ => None,
    }
}

fn styles_open(st: &mut StylesState, sec: &mut Sec, e: &BytesStart, empty: bool) {
    match e.name().as_ref() {
        b"numFmt" => {
            if let (Some(id), Some(code)) = (
                get_attr(e, b"numFmtId").and_then(|s| s.parse::<u32>().ok()),
                get_attr(e, b"formatCode"),
            ) {
                st.num_fmts.insert(id, code);
            }
        }
        b"fonts" => *sec = Sec::Fonts,
        b"fills" => *sec = Sec::Fills,
        b"borders" => *sec = Sec::Borders,
        b"cellXfs" => *sec = Sec::CellXfs,
        b"font" if *sec == Sec::Fonts => {
            st.cur_font = FontStyle::default();
            if empty {
                st.fonts.push(FontStyle::default());
            }
        }
        b"b" if *sec == Sec::Fonts => {
            st.cur_font.bold = get_attr(e, b"val").as_deref() != Some("0");
        }
        b"sz" if *sec == Sec::Fonts => {
            st.cur_font.size = get_attr(e, b"val").and_then(|s| s.parse().ok());
        }
        b"color" if *sec == Sec::Fonts => {
            if let Some(c) = parse_color(e) {
                st.cur_font.color = Some(c);
            }
        }
        b"fill" if *sec == Sec::Fills => {
            st.cur_fill = None;
            st.cur_fill_solid = false;
            if empty {
                st.fills.push(None);
            }
        }
        b"patternFill" if *sec == Sec::Fills => {
            st.cur_fill_solid = get_attr(e, b"patternType").as_deref() == Some("solid");
        }
        b"fgColor" if *sec == Sec::Fills && st.cur_fill_solid => {
            st.cur_fill = parse_color(e);
        }
        b"border" if *sec == Sec::Borders => {
            st.cur_border = BorderEdges::default();
            if empty {
                st.borders.push(BorderEdges::default());
            }
        }
        edge @ (b"left" | b"right" | b"top" | b"bottom") if *sec == Sec::Borders => {
            let has = get_attr(e, b"style").map(|s| s != "none").unwrap_or(false);
            match edge {
                b"left" => st.cur_border.left = has,
                b"right" => st.cur_border.right = has,
                b"top" => st.cur_border.top = has,
                _ => st.cur_border.bottom = has,
            }
        }
        b"xf" if *sec == Sec::CellXfs => {
            let raw: XfRaw = (
                get_attr(e, b"numFmtId").and_then(|s| s.parse().ok()).unwrap_or(0),
                get_attr(e, b"fontId").and_then(|s| s.parse().ok()).unwrap_or(0),
                get_attr(e, b"fillId").and_then(|s| s.parse().ok()).unwrap_or(0),
                get_attr(e, b"borderId").and_then(|s| s.parse().ok()).unwrap_or(0),
                None,
            );
            if empty {
                st.xf_raw.push(raw);
            } else {
                st.cur_xf = Some(raw);
            }
        }
        b"alignment" if *sec == Sec::CellXfs => {
            if let Some(xf) = st.cur_xf.as_mut() {
                xf.4 = get_attr(e, b"horizontal").and_then(|s| h_align(&s));
            }
        }
        _ => {}
    }
}

fn styles_close(st: &mut StylesState, sec: &mut Sec, name: &[u8]) {
    match name {
        b"fonts" | b"fills" | b"borders" | b"cellXfs" => *sec = Sec::None,
        b"font" if *sec == Sec::Fonts => st.fonts.push(st.cur_font.clone()),
        b"fill" if *sec == Sec::Fills => st.fills.push(st.cur_fill.take()),
        b"border" if *sec == Sec::Borders => st.borders.push(st.cur_border),
        b"xf" if *sec == Sec::CellXfs => {
            if let Some(raw) = st.cur_xf.take() {
                st.xf_raw.push(raw);
            }
        }
        _ => {}
    }
}

fn parse_styles(xml: &str) -> Styles {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut st = StylesState::default();
    let mut sec = Sec::None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => styles_open(&mut st, &mut sec, &e, false),
            Ok(Event::Empty(e)) => styles_open(&mut st, &mut sec, &e, true),
            Ok(Event::End(e)) => styles_close(&mut st, &mut sec, e.name().as_ref()),
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    let xfs = st
        .xf_raw
        .into_iter()
        .map(|(num_fmt_id, font_id, fill_id, border_id, h_align)| Xf {
            fmt_code: st
                .num_fmts
                .get(&num_fmt_id)
                .cloned()
                .unwrap_or_else(|| builtin_format(num_fmt_id).to_string()),
            font: st.fonts.get(font_id).cloned().unwrap_or_default(),
            fill: st.fills.get(fill_id).copied().flatten(),
            border: st.borders.get(border_id).copied().unwrap_or_default(),
            h_align,
        })
        .collect();

    Styles { xfs }
}

/// Parse geometry + styles for one sheet. Returns sensible defaults on failure.
pub fn parse(bytes: &[u8], sheet_index: usize, default_col_px: f32, default_row_px: f32) -> Parsed {
    let fallback = || Geometry { default_col_px, default_row_px, ..Default::default() };
    let mut zip = match ZipArchive::new(Cursor::new(bytes.to_vec())) {
        Ok(z) => z,
        Err(_) => return Parsed { geom: fallback(), styles: Styles::default() },
    };
    let styles = read_entry(&mut zip, "xl/styles.xml").map(|s| parse_styles(&s)).unwrap_or_default();
    let geom = match sheet_path(&mut zip, sheet_index).and_then(|p| read_entry(&mut zip, &p)) {
        Some(xml) => parse_worksheet(&xml, default_col_px, default_row_px),
        None => fallback(),
    };
    Parsed { geom, styles }
}
