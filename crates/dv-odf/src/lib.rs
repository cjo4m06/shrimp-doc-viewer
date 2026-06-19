//! OpenDocument viewer support. ODT (text) and ODP (presentation) lower into
//! `dv_flow::Block`s; ODS (spreadsheet) yields named sheets of cell strings for the
//! shared grid renderer. Reads `content.xml` from the package zip with quick-xml.
//! Viewer-grade: text + structure + inline character styling (from automatic-styles).

use std::collections::HashMap;
use std::io::{Cursor, Read};

use dv_flow::{Block, Span};
use dv_ir::Color;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use zip::ZipArchive;

fn content_xml(bytes: &[u8]) -> Option<String> {
    let mut zip = ZipArchive::new(Cursor::new(bytes.to_vec())).ok()?;
    let mut f = zip.by_name("content.xml").ok()?;
    let mut s = String::new();
    f.read_to_string(&mut s).ok()?;
    Some(s)
}

fn attr(e: &BytesStart, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .map(|a| String::from_utf8_lossy(&a.value).into_owned())
}

fn parse_hex_color(s: &str) -> Option<Color> {
    let h = s.trim_start_matches('#');
    if h.len() == 6 {
        let r = u8::from_str_radix(&h[0..2], 16).ok()?;
        let g = u8::from_str_radix(&h[2..4], 16).ok()?;
        let b = u8::from_str_radix(&h[4..6], 16).ok()?;
        Some(Color { r, g, b, a: 255 })
    } else {
        None
    }
}

#[derive(Clone, Copy, Default)]
struct SpanStyle {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    color: Option<Color>,
    size: Option<f32>,
}

/// Build the automatic-style map (style name -> character style) by scanning
/// `<style:style>` / `<style:text-properties>` that precede the body.
// The `if cur_name.is_some()` body reads clearer than a match guard over its 30 lines.
#[allow(clippy::collapsible_match)]
fn parse_styles(xml: &str) -> HashMap<String, SpanStyle> {
    let mut reader = Reader::from_str(xml);
    let mut map = HashMap::new();
    let mut cur_name: Option<String> = None;
    let mut cur = SpanStyle::default();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match e.local_name().as_ref() {
                b"style" => {
                    cur_name = attr(&e, b"style:name");
                    cur = SpanStyle::default();
                }
                b"text-properties" => {
                    if cur_name.is_some() {
                        if attr(&e, b"fo:font-weight").as_deref() == Some("bold") {
                            cur.bold = true;
                        }
                        if attr(&e, b"fo:font-style").as_deref() == Some("italic") {
                            cur.italic = true;
                        }
                        if attr(&e, b"style:text-underline-style")
                            .map(|v| v != "none")
                            .unwrap_or(false)
                        {
                            cur.underline = true;
                        }
                        if attr(&e, b"style:text-line-through-style")
                            .map(|v| v != "none")
                            .unwrap_or(false)
                        {
                            cur.strike = true;
                        }
                        if let Some(c) = attr(&e, b"fo:color").and_then(|v| parse_hex_color(&v)) {
                            cur.color = Some(c);
                        }
                        if let Some(sz) = attr(&e, b"fo:font-size") {
                            if let Some(pt) =
                                sz.strip_suffix("pt").and_then(|s| s.parse::<f32>().ok())
                            {
                                cur.size = Some(pt * 96.0 / 72.0);
                            }
                        }
                        if let Some(n) = cur_name.clone() {
                            map.insert(n, cur);
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::End(e)) if e.local_name().as_ref() == b"style" => cur_name = None,
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    map
}

enum Kind {
    Heading(u8),
    Para,
    Item(u8, String),
}

/// ODT: headings, paragraphs and list items with inline character styling.
pub fn parse_text(bytes: &[u8]) -> Vec<Block> {
    let xml = match content_xml(bytes) {
        Some(x) => x,
        None => return vec![Block::Para(vec![Span::new("")])],
    };
    let styles = parse_styles(&xml);
    let mut reader = Reader::from_str(&xml);
    let mut blocks = Vec::new();
    let mut in_text = false;
    let mut cur: Option<Kind> = None;
    let mut spans: Vec<Span> = Vec::new();
    let mut style_stack: Vec<SpanStyle> = Vec::new();
    let mut list_level: u8 = 0;
    // ordered-list counters per level (None = bullet level)
    let mut counters: Vec<Option<u32>> = Vec::new();

    let push = |spans: &mut Vec<Span>, st: Option<&SpanStyle>, text: &str| {
        if text.is_empty() {
            return;
        }
        let s = st.copied().unwrap_or_default();
        spans.push(Span {
            text: text.to_string(),
            bold: s.bold,
            italic: s.italic,
            underline: s.underline,
            strike: s.strike,
            color: s.color,
            size: s.size,
            ..Default::default()
        });
    };

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"text" => in_text = true,
                b"list" => {
                    list_level = list_level.saturating_add(1);
                    // ordered if any level style is number; default bullet
                    let ordered = false;
                    counters.push(if ordered { Some(0) } else { None });
                }
                b"list-item" => {
                    if let Some(Some(c)) = counters.last_mut() {
                        *c += 1;
                    }
                }
                b"h" if in_text => {
                    let lvl = attr(&e, b"text:outline-level")
                        .and_then(|s| s.parse::<u8>().ok())
                        .unwrap_or(1)
                        .clamp(1, 6);
                    cur = Some(Kind::Heading(lvl));
                    spans.clear();
                }
                b"p" if in_text => {
                    cur = Some(if list_level > 0 {
                        let lvl = list_level - 1;
                        let marker = match counters.last() {
                            Some(Some(n)) => format!("{}.", n),
                            _ => "•".to_string(),
                        };
                        Kind::Item(lvl, marker)
                    } else {
                        Kind::Para
                    });
                    spans.clear();
                }
                b"span" if cur.is_some() => {
                    let st = attr(&e, b"text:style-name")
                        .and_then(|n| styles.get(&n).copied())
                        .unwrap_or_default();
                    style_stack.push(st);
                }
                _ => {}
            },
            Ok(Event::Empty(e)) if cur.is_some() => match e.local_name().as_ref() {
                b"tab" => push(&mut spans, style_stack.last(), "    "),
                b"line-break" => push(&mut spans, style_stack.last(), " "),
                b"s" => {
                    let n = attr(&e, b"text:c")
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(1)
                        .min(256);
                    push(&mut spans, style_stack.last(), &" ".repeat(n));
                }
                _ => {}
            },
            Ok(Event::Text(t)) => {
                if cur.is_some() {
                    push(
                        &mut spans,
                        style_stack.last(),
                        &t.unescape().unwrap_or_default(),
                    );
                }
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"span" => {
                    style_stack.pop();
                }
                b"list" => {
                    list_level = list_level.saturating_sub(1);
                    counters.pop();
                }
                b"h" | b"p" => {
                    if let Some(k) = cur.take() {
                        let body = std::mem::take(&mut spans);
                        let has = body.iter().any(|s| !s.text.trim().is_empty());
                        if has {
                            blocks.push(match k {
                                Kind::Heading(l) => Block::Heading(l, body),
                                Kind::Item(level, marker) => Block::ListItem {
                                    level,
                                    marker,
                                    spans: body,
                                },
                                Kind::Para => Block::Para(body),
                            });
                        }
                    }
                }
                b"text" => in_text = false,
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    if blocks.is_empty() {
        blocks.push(Block::Para(vec![Span::new("")]));
    }
    blocks
}

/// ODS: every sheet as `(name, rows)` for the grid renderer.
pub fn parse_spreadsheet(bytes: &[u8]) -> Vec<(String, Vec<Vec<String>>)> {
    let xml = match content_xml(bytes) {
        Some(x) => x,
        None => return Vec::new(),
    };
    let mut reader = Reader::from_str(&xml);
    let mut sheets: Vec<(String, Vec<Vec<String>>)> = Vec::new();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut name = String::new();
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut cell_val: Option<String> = None; // office:value / date / string-value
    let mut cell_repeat = 1usize;
    let mut row_repeat = 1usize;
    let mut in_cell = false;
    let mut in_table = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                match e.local_name().as_ref() {
                    b"table" => {
                        in_table = true;
                        rows = Vec::new();
                        name = attr(&e, b"table:name")
                            .unwrap_or_else(|| format!("Sheet{}", sheets.len() + 1));
                    }
                    b"table-row" if in_table => {
                        row = Vec::new();
                        row_repeat = attr(&e, b"table:number-rows-repeated")
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(1)
                            .min(1000);
                    }
                    b"table-cell" | b"covered-table-cell" if in_table => {
                        in_cell = true;
                        cell.clear();
                        cell_repeat = attr(&e, b"table:number-columns-repeated")
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(1)
                            .min(1024);
                        // numeric/date/bool cells carry the value in an attribute
                        cell_val = attr(&e, b"office:value")
                            .or_else(|| attr(&e, b"office:date-value"))
                            .or_else(|| attr(&e, b"office:string-value"))
                            .or_else(|| attr(&e, b"office:boolean-value"));
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                if in_cell {
                    cell.push_str(&t.unescape().unwrap_or_default());
                }
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"table-cell" | b"covered-table-cell" if in_table => {
                    in_cell = false;
                    let text = if cell.trim().is_empty() {
                        cell_val.take().unwrap_or_default()
                    } else {
                        cell.clone()
                    };
                    // a huge repeat of an EMPTY trailing cell shouldn't materialize 1024 cells
                    let rep = if text.trim().is_empty() {
                        1
                    } else {
                        cell_repeat.max(1)
                    };
                    for _ in 0..rep {
                        row.push(text.clone());
                    }
                    cell_val = None;
                }
                b"table-row" if in_table => {
                    while row.last().map(|s| s.trim().is_empty()).unwrap_or(false) {
                        row.pop();
                    }
                    let rep = if row.is_empty() { 1 } else { row_repeat.max(1) };
                    for _ in 0..rep {
                        rows.push(row.clone());
                    }
                }
                b"table" if in_table => {
                    in_table = false;
                    while rows
                        .last()
                        .map(|r| r.iter().all(|s| s.trim().is_empty()))
                        .unwrap_or(false)
                    {
                        rows.pop();
                    }
                    sheets.push((std::mem::take(&mut name), std::mem::take(&mut rows)));
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    sheets
}

/// Back-compat: first sheet's rows only.
pub fn parse_spreadsheet_rows(bytes: &[u8]) -> Vec<Vec<String>> {
    parse_spreadsheet(bytes)
        .into_iter()
        .next()
        .map(|(_, r)| r)
        .unwrap_or_default()
}

/// ODP: each draw:page -> a heading + its text paragraphs; presentation notes skipped.
pub fn parse_presentation(bytes: &[u8]) -> Vec<Block> {
    let xml = match content_xml(bytes) {
        Some(x) => x,
        None => return vec![Block::Para(vec![Span::new("")])],
    };
    let mut reader = Reader::from_str(&xml);
    let mut blocks = Vec::new();
    let mut page = 0u32;
    let mut in_p = false;
    let mut in_notes = false;
    let mut text = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"notes" => in_notes = true,
                b"page" if !in_notes => {
                    page += 1;
                    if page > 1 {
                        blocks.push(Block::Rule);
                    }
                    let name = attr(&e, b"draw:name").unwrap_or_else(|| format!("Slide {}", page));
                    blocks.push(Block::Heading(2, vec![Span::new(name)]));
                }
                b"p" if !in_notes => {
                    in_p = true;
                    text.clear();
                }
                _ => {}
            },
            Ok(Event::Text(t)) => {
                if in_p {
                    text.push_str(&t.unescape().unwrap_or_default());
                }
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"notes" => in_notes = false,
                b"p" if in_p => {
                    in_p = false;
                    let s = text.trim().to_string();
                    if !s.is_empty() {
                        blocks.push(Block::Para(vec![Span::new(s)]));
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    if blocks.is_empty() {
        blocks.push(Block::Para(vec![Span::new("")]));
    }
    blocks
}
