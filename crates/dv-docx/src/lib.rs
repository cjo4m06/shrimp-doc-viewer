//! Self-written DOCX flow-layout renderer.
//!
//! Parses `word/document.xml` (paragraphs, runs, run/paragraph properties, page
//! geometry) and runs a small flow-layout engine — greedy line wrapping that
//! breaks at spaces (Latin) or between CJK characters (繁中) — lowering the
//! result into the shared [`dv_ir::DisplayList`].
//!
//! Scope (M4.1): paragraphs + runs, bold (faux) / size / colour, paragraph
//! alignment (left/center/right/justify→left), CJK+Latin wrapping, page width &
//! margins. Not yet: tables, lists/numbering, images, floats, real pagination
//! (rendered as one continuous page), styles.xml inheritance, italic slant.

use std::io::{Cursor, Read};

use dv_ir::{Color, Command, DisplayList, FontId, GlyphRun, Paint, PositionedGlyph};
use dv_text::{shape, FontData};
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use zip::ZipArchive;

#[derive(Clone, Copy, PartialEq)]
enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone)]
struct Run {
    text: String,
    bold: bool,
    size: f32, // px
    color: Color,
}

struct Para {
    runs: Vec<Run>,
    align: Align,
}

struct Document {
    paras: Vec<Para>,
    page_w: f32,
    margin_l: f32,
    margin_r: f32,
    margin_t: f32,
    margin_b: f32,
}

const DEFAULT_SIZE_PX: f32 = 14.67; // 11pt
const TWIP_TO_PX: f32 = 1.0 / 15.0; // 1/1440 inch * 96 dpi

fn get_attr(e: &BytesStart, key: &[u8]) -> Option<String> {
    for a in e.attributes().flatten() {
        if a.key.as_ref() == key {
            return Some(String::from_utf8_lossy(a.value.as_ref()).into_owned());
        }
    }
    None
}

fn parse_color(s: &str) -> Color {
    if s.eq_ignore_ascii_case("auto") || s.len() != 6 {
        return Color::BLACK;
    }
    let r = u8::from_str_radix(&s[0..2], 16).unwrap_or(0);
    let g = u8::from_str_radix(&s[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&s[4..6], 16).unwrap_or(0);
    Color::rgb(r, g, b)
}

fn is_cjk(ch: char) -> bool {
    let c = ch as u32;
    (0x2E80..=0x9FFF).contains(&c)
        || (0xAC00..=0xD7A3).contains(&c)
        || (0xF900..=0xFAFF).contains(&c)
        || (0xFF00..=0xFFEF).contains(&c)
}

fn parse_document(xml: &str) -> Document {
    let mut doc = Document {
        paras: Vec::new(),
        page_w: 794.0,
        margin_l: 96.0,
        margin_r: 96.0,
        margin_t: 96.0,
        margin_b: 96.0,
    };

    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();

    let mut cur_para: Option<Para> = None;
    let mut cur_align = Align::Left;
    let mut cur_run: Option<Run> = None;
    let mut in_t = false;

    let open = |doc: &mut Document,
                    cur_para: &mut Option<Para>,
                    cur_align: &mut Align,
                    cur_run: &mut Option<Run>,
                    in_t: &mut bool,
                    e: &BytesStart| {
        match e.name().as_ref() {
            b"w:p" => {
                *cur_align = Align::Left;
                *cur_para = Some(Para { runs: Vec::new(), align: Align::Left });
            }
            b"w:jc" => {
                if cur_para.is_some() {
                    *cur_align = match get_attr(e, b"w:val").as_deref() {
                        Some("center") => Align::Center,
                        Some("right") | Some("end") => Align::Right,
                        _ => Align::Left,
                    };
                }
            }
            b"w:r" => {
                *cur_run = Some(Run { text: String::new(), bold: false, size: DEFAULT_SIZE_PX, color: Color::BLACK });
            }
            b"w:b" => {
                if let Some(r) = cur_run.as_mut() {
                    r.bold = get_attr(e, b"w:val").as_deref() != Some("false")
                        && get_attr(e, b"w:val").as_deref() != Some("0");
                }
            }
            b"w:sz" => {
                if let (Some(r), Some(v)) = (cur_run.as_mut(), get_attr(e, b"w:val").and_then(|s| s.parse::<f32>().ok())) {
                    r.size = v * 2.0 / 3.0; // half-points -> px
                }
            }
            b"w:color" => {
                if let (Some(r), Some(v)) = (cur_run.as_mut(), get_attr(e, b"w:val")) {
                    r.color = parse_color(&v);
                }
            }
            b"w:t" => *in_t = true,
            b"w:pgSz" => {
                if let Some(w) = get_attr(e, b"w:w").and_then(|s| s.parse::<f32>().ok()) {
                    doc.page_w = w * TWIP_TO_PX;
                }
            }
            b"w:pgMar" => {
                if let Some(v) = get_attr(e, b"w:left").and_then(|s| s.parse::<f32>().ok()) {
                    doc.margin_l = v * TWIP_TO_PX;
                }
                if let Some(v) = get_attr(e, b"w:right").and_then(|s| s.parse::<f32>().ok()) {
                    doc.margin_r = v * TWIP_TO_PX;
                }
                if let Some(v) = get_attr(e, b"w:top").and_then(|s| s.parse::<f32>().ok()) {
                    doc.margin_t = v * TWIP_TO_PX;
                }
                if let Some(v) = get_attr(e, b"w:bottom").and_then(|s| s.parse::<f32>().ok()) {
                    doc.margin_b = v * TWIP_TO_PX;
                }
            }
            _ => {}
        }
    };

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                open(&mut doc, &mut cur_para, &mut cur_align, &mut cur_run, &mut in_t, &e);
            }
            Ok(Event::Text(t)) => {
                if in_t {
                    if let Some(r) = cur_run.as_mut() {
                        r.text.push_str(&t.unescape().unwrap_or_default());
                    }
                }
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"w:t" => in_t = false,
                b"w:r" => {
                    if let (Some(p), Some(r)) = (cur_para.as_mut(), cur_run.take()) {
                        if !r.text.is_empty() {
                            p.runs.push(r);
                        }
                    }
                }
                b"w:p" => {
                    if let Some(mut p) = cur_para.take() {
                        p.align = cur_align;
                        doc.paras.push(p);
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    doc
}

/// One laid-out glyph with the style needed to paint it.
struct Item {
    gid: u32,
    advance: f32,
    x_off: f32,
    size: f32,
    color: Color,
    bold: bool,
    break_after: bool,
    is_space: bool,
}

fn shape_para(font: &FontData, para: &Para) -> Vec<Item> {
    let mut items = Vec::new();
    for run in &para.runs {
        let shaped = shape(font, &run.text, run.size);
        let scale = run.size / shaped.units_per_em.max(1.0);
        for g in &shaped.glyphs {
            let ch = run.text.get(g.cluster as usize..).and_then(|s| s.chars().next()).unwrap_or(' ');
            let is_space = ch.is_whitespace();
            items.push(Item {
                gid: g.glyph_id,
                advance: g.x_advance * scale,
                x_off: g.x_offset * scale,
                size: run.size,
                color: run.color,
                bold: run.bold,
                break_after: is_space || is_cjk(ch),
                is_space,
            });
        }
    }
    items
}

fn wrap(items: Vec<Item>, content_w: f32) -> Vec<Vec<Item>> {
    let mut lines: Vec<Vec<Item>> = Vec::new();
    let mut cur: Vec<Item> = Vec::new();
    let mut cur_w = 0.0f32;

    let last_break = |line: &[Item]| line.iter().rposition(|it| it.break_after);

    for it in items {
        if !cur.is_empty() && cur_w + it.advance > content_w {
            if let Some(bi) = last_break(&cur) {
                let remainder = cur.split_off(bi + 1);
                lines.push(std::mem::take(&mut cur));
                cur = remainder;
            } else {
                lines.push(std::mem::take(&mut cur));
            }
            cur_w = cur.iter().map(|i| i.advance).sum();
        }
        cur_w += it.advance;
        cur.push(it);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

fn line_width(line: &[Item]) -> f32 {
    // Width excluding trailing spaces.
    let end = line.iter().rposition(|i| !i.is_space).map(|i| i + 1).unwrap_or(0);
    line[..end].iter().map(|i| i.advance).sum()
}

/// Lay out and render a DOCX into a continuous page. `font` is used to measure
/// and (under [`FontId(0)`]) to paint.
pub fn render_document(bytes: &[u8], font: &FontData) -> DisplayList {
    let doc = match read_document_xml(bytes) {
        Some(xml) => parse_document(&xml),
        None => return DisplayList::new(794.0, 200.0),
    };

    let content_w = (doc.page_w - doc.margin_l - doc.margin_r).max(32.0);
    let mut commands: Vec<Command> = Vec::new();
    let mut y = doc.margin_t;

    for para in &doc.paras {
        let items = shape_para(font, para);
        if items.is_empty() {
            y += DEFAULT_SIZE_PX * 1.4;
            continue;
        }
        let lines = wrap(items, content_w);
        for line in &lines {
            let max_size = line.iter().map(|i| i.size).fold(DEFAULT_SIZE_PX, f32::max);
            let line_h = max_size * 1.4;
            let baseline = y + max_size * 0.92;
            let lw = line_width(line);
            let x_start = match para.align {
                Align::Left => doc.margin_l,
                Align::Center => doc.margin_l + (content_w - lw) / 2.0,
                Align::Right => doc.margin_l + (content_w - lw),
            };

            // Emit glyphs, grouped into runs of equal (size, color, bold).
            let mut x = x_start;
            let mut i = 0;
            while i < line.len() {
                let (size, color, bold) = (line[i].size, line[i].color, line[i].bold);
                let mut glyphs = Vec::new();
                while i < line.len() && line[i].size == size && line[i].color == color && line[i].bold == bold {
                    let it = &line[i];
                    glyphs.push(PositionedGlyph { id: it.gid, x: x + it.x_off, y: baseline });
                    x += it.advance;
                    i += 1;
                }
                commands.push(Command::Glyphs(GlyphRun { font: FontId(0), size, paint: Paint::Solid(color), bold, glyphs }));
            }
            y += line_h;
        }
        y += max_para_size(para) * 0.45; // paragraph spacing
    }

    let total_h = y + doc.margin_b;
    let mut dl = DisplayList::new(doc.page_w, total_h);
    dl.commands = commands;
    dl
}

fn max_para_size(para: &Para) -> f32 {
    para.runs.iter().map(|r| r.size).fold(DEFAULT_SIZE_PX, f32::max)
}

fn read_document_xml(bytes: &[u8]) -> Option<String> {
    let mut zip = ZipArchive::new(Cursor::new(bytes.to_vec())).ok()?;
    let mut f = zip.by_name("word/document.xml").ok()?;
    let mut s = String::new();
    f.read_to_string(&mut s).ok()?;
    Some(s)
}
