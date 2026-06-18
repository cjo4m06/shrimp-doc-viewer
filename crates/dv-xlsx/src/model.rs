//! OOXML geometry parser — the layout layer calamine does not expose: column
//! widths, row heights, and merged-cell ranges, parsed from `xl/worksheets/*.xml`
//! with quick-xml. Values still come from calamine; this adds the structure that
//! makes a sheet look like a sheet. Styles (fills/fonts/borders/number formats)
//! are layered on in a later step.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use zip::ZipArchive;

type Zip = ZipArchive<Cursor<Vec<u8>>>;

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

/// Parse geometry for one sheet. Returns sensible defaults on any failure.
pub fn parse_geometry(bytes: &[u8], sheet_index: usize, default_col_px: f32, default_row_px: f32) -> Geometry {
    let mut zip = match ZipArchive::new(Cursor::new(bytes.to_vec())) {
        Ok(z) => z,
        Err(_) => return Geometry { default_col_px, default_row_px, ..Default::default() },
    };
    let path = match sheet_path(&mut zip, sheet_index) {
        Some(p) => p,
        None => return Geometry { default_col_px, default_row_px, ..Default::default() },
    };
    match read_entry(&mut zip, &path) {
        Some(xml) => parse_worksheet(&xml, default_col_px, default_row_px),
        None => Geometry { default_col_px, default_row_px, ..Default::default() },
    }
}
