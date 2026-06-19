//! OpenDocument viewer support. ODT (text) and ODP (presentation) lower into
//! `dv_flow::Block`s; ODS (spreadsheet) yields rows of strings for the shared
//! grid renderer (`dv_xlsx::Sheet::from_rows`). All three read `content.xml` from
//! the package zip and walk it with quick-xml — viewer-grade (text + structure,
//! not styles).

use std::io::{Cursor, Read};

use dv_flow::{Block, Span};
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
    e.attributes().flatten().find(|a| a.key.as_ref() == key).map(|a| String::from_utf8_lossy(&a.value).into_owned())
}

enum Kind {
    Heading(u8),
    Para,
    Item(u8),
}

/// ODT: headings, paragraphs and list items (text only).
pub fn parse_text(bytes: &[u8]) -> Vec<Block> {
    let xml = match content_xml(bytes) {
        Some(x) => x,
        None => return vec![Block::Para(vec![Span::new("")])],
    };
    let mut reader = Reader::from_str(&xml);
    let mut blocks = Vec::new();
    let mut in_text = false;
    let mut cur: Option<Kind> = None;
    let mut text = String::new();
    let mut list_level: u8 = 0;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"text" => in_text = true,
                b"list" => list_level = list_level.saturating_add(1),
                b"h" if in_text => {
                    let lvl = attr(&e, b"text:outline-level").and_then(|s| s.parse::<u8>().ok()).unwrap_or(1).clamp(1, 6);
                    cur = Some(Kind::Heading(lvl));
                    text.clear();
                }
                b"p" if in_text => {
                    cur = Some(if list_level > 0 { Kind::Item(list_level - 1) } else { Kind::Para });
                    text.clear();
                }
                _ => {}
            },
            Ok(Event::Empty(e)) => {
                if cur.is_some() && matches!(e.local_name().as_ref(), b"tab" | b"s") {
                    text.push(' ');
                }
            }
            Ok(Event::Text(t)) => {
                if cur.is_some() {
                    text.push_str(&t.unescape().unwrap_or_default());
                }
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"list" => list_level = list_level.saturating_sub(1),
                b"h" | b"p" => {
                    if let Some(k) = cur.take() {
                        let s = text.trim().to_string();
                        if !s.is_empty() {
                            blocks.push(match k {
                                Kind::Heading(l) => Block::Heading(l, vec![Span::new(s)]),
                                Kind::Item(level) => Block::ListItem { level, marker: "•".into(), spans: vec![Span::new(s)] },
                                Kind::Para => Block::Para(vec![Span::new(s)]),
                            });
                        }
                    }
                    text.clear();
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

/// ODS: rows of cell strings (first table only) for the grid renderer.
pub fn parse_spreadsheet_rows(bytes: &[u8]) -> Vec<Vec<String>> {
    let xml = match content_xml(bytes) {
        Some(x) => x,
        None => return Vec::new(),
    };
    let mut reader = Reader::from_str(&xml);
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut cell_repeat = 1usize;
    let mut row_repeat = 1usize;
    let mut in_cell = false;
    let mut in_table = false;
    let mut first_table_done = false;

    let handle_start = |e: &BytesStart, in_table: &mut bool, first_table_done: &mut bool, row: &mut Vec<String>, row_repeat: &mut usize, in_cell: &mut bool, cell: &mut String, cell_repeat: &mut usize| {
        match e.local_name().as_ref() {
            b"table" if !*first_table_done => *in_table = true,
            b"table-row" if *in_table => {
                row.clear();
                *row_repeat = attr(e, b"table:number-rows-repeated").and_then(|s| s.parse().ok()).unwrap_or(1).min(2);
            }
            b"table-cell" | b"covered-table-cell" if *in_table => {
                *in_cell = true;
                cell.clear();
                *cell_repeat = attr(e, b"table:number-columns-repeated").and_then(|s| s.parse().ok()).unwrap_or(1).min(64);
            }
            _ => {}
        }
    };

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => handle_start(&e, &mut in_table, &mut first_table_done, &mut row, &mut row_repeat, &mut in_cell, &mut cell, &mut cell_repeat),
            Ok(Event::Empty(e)) => {
                handle_start(&e, &mut in_table, &mut first_table_done, &mut row, &mut row_repeat, &mut in_cell, &mut cell, &mut cell_repeat);
                if in_cell && matches!(e.local_name().as_ref(), b"table-cell" | b"covered-table-cell") {
                    in_cell = false;
                    for _ in 0..cell_repeat.max(1) {
                        row.push(cell.clone());
                    }
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
                    for _ in 0..cell_repeat.max(1) {
                        row.push(cell.clone());
                    }
                }
                b"table-row" if in_table => {
                    while row.last().map(|s| s.trim().is_empty()).unwrap_or(false) {
                        row.pop();
                    }
                    for _ in 0..row_repeat.max(1) {
                        rows.push(row.clone());
                    }
                }
                b"table" if in_table => {
                    in_table = false;
                    first_table_done = true;
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    while rows.last().map(|r| r.iter().all(|s| s.trim().is_empty())).unwrap_or(false) {
        rows.pop();
    }
    rows
}

/// ODP: each draw:page becomes a heading + its text paragraphs, pages separated.
pub fn parse_presentation(bytes: &[u8]) -> Vec<Block> {
    let xml = match content_xml(bytes) {
        Some(x) => x,
        None => return vec![Block::Para(vec![Span::new("")])],
    };
    let mut reader = Reader::from_str(&xml);
    let mut blocks = Vec::new();
    let mut page = 0u32;
    let mut in_p = false;
    let mut text = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"page" => {
                    page += 1;
                    if page > 1 {
                        blocks.push(Block::Rule);
                    }
                    let name = attr(&e, b"draw:name").unwrap_or_else(|| format!("Slide {}", page));
                    blocks.push(Block::Heading(2, vec![Span::new(name)]));
                }
                b"p" => {
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
            Ok(Event::End(e)) => {
                if e.local_name().as_ref() == b"p" {
                    in_p = false;
                    let s = text.trim().to_string();
                    if !s.is_empty() {
                        blocks.push(Block::Para(vec![Span::new(s)]));
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    if blocks.is_empty() {
        blocks.push(Block::Para(vec![Span::new("")]));
    }
    blocks
}
