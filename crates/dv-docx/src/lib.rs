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
    page_h: f32,
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
        page_w: 816.0,  // US Letter default (12240 twips)
        page_h: 1056.0, // 15840 twips
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
                if let Some(h) = get_attr(e, b"w:h").and_then(|s| s.parse::<f32>().ok()) {
                    doc.page_h = h * TWIP_TO_PX;
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

/// One glyph placed on a line, page-relative x (at zoom 1), with paint style.
#[derive(Clone, Copy)]
struct PlacedGlyph {
    id: u32,
    x: f32,
    size: f32,
    color: Color,
    bold: bool,
}

/// A laid-out line at zoom 1. `top` is the cumulative content-y of its top edge.
struct Line {
    placed: Vec<PlacedGlyph>,
    top: f32,
    line_h: f32,
    advance: f32, // line_h + trailing paragraph spacing
    ascent: f32,
}

/// Shape + wrap all paragraphs into a flat list of laid-out lines (zoom 1).
fn layout_lines(doc: &Document, font: &FontData) -> Vec<Line> {
    let content_w = (doc.page_w - doc.margin_l - doc.margin_r).max(32.0);
    let mut lines = Vec::new();
    let mut top = 0.0f32;

    for para in &doc.paras {
        let items = shape_para(font, para);
        if items.is_empty() {
            let line_h = DEFAULT_SIZE_PX * 1.4;
            lines.push(Line { placed: Vec::new(), top, line_h, advance: line_h, ascent: DEFAULT_SIZE_PX * 0.92 });
            top += line_h;
            continue;
        }
        let wrapped = wrap(items, content_w);
        let n = wrapped.len();
        let para_space = max_para_size(para) * 0.45;
        for (li, line) in wrapped.iter().enumerate() {
            let max_size = line.iter().map(|i| i.size).fold(DEFAULT_SIZE_PX, f32::max);
            let line_h = max_size * 1.4;
            let lw = line_width(line);
            let mut x = match para.align {
                Align::Left => doc.margin_l,
                Align::Center => doc.margin_l + (content_w - lw) / 2.0,
                Align::Right => doc.margin_l + (content_w - lw),
            };
            let mut placed = Vec::with_capacity(line.len());
            for it in line {
                placed.push(PlacedGlyph { id: it.gid, x: x + it.x_off, size: it.size, color: it.color, bold: it.bold });
                x += it.advance;
            }
            let advance = line_h + if li + 1 == n { para_space } else { 0.0 };
            lines.push(Line { placed, top, line_h, advance, ascent: max_size * 0.92 });
            top += advance;
        }
    }
    lines
}

/// Emit one line's glyphs (grouped by run style) at a device `baseline`/`scale`.
fn emit_line(dl: &mut DisplayList, line: &Line, baseline: f32, scale: f32) {
    let mut i = 0;
    while i < line.placed.len() {
        let (size, color, bold) = (line.placed[i].size, line.placed[i].color, line.placed[i].bold);
        let mut glyphs = Vec::new();
        while i < line.placed.len() && line.placed[i].size == size && line.placed[i].color == color && line.placed[i].bold == bold {
            glyphs.push(PositionedGlyph { id: line.placed[i].id, x: line.placed[i].x * scale, y: baseline });
            i += 1;
        }
        dl.push(Command::Glyphs(GlyphRun { font: FontId(0), size: size * scale, paint: Paint::Solid(color), bold, glyphs }));
    }
}

/// Lay out and render a DOCX into a single continuous page (no pagination).
pub fn render_document(bytes: &[u8], font: &FontData) -> DisplayList {
    let doc = match read_document_xml(bytes) {
        Some(xml) => parse_document(&xml),
        None => return DisplayList::new(816.0, 200.0),
    };
    let lines = layout_lines(&doc, font);
    let total_h = doc.margin_t + lines.last().map(|l| l.top + l.advance).unwrap_or(0.0) + doc.margin_b;
    let mut dl = DisplayList::new(doc.page_w, total_h);
    for line in &lines {
        emit_line(&mut dl, line, doc.margin_t + line.top + line.ascent, 1.0);
    }
    dl
}

struct Page {
    start: usize,
    end: usize,
    top_y: f32,
}

/// A paginated DOCX, ready for per-page virtualized, zoomable rendering.
pub struct DocxDoc {
    lines: Vec<Line>,
    pages: Vec<Page>,
    page_w: f32,
    page_h: f32,
    margin_t: f32,
}

impl DocxDoc {
    pub fn parse(bytes: &[u8], font: &FontData) -> DocxDoc {
        let doc = read_document_xml(bytes).map(|x| parse_document(&x)).unwrap_or_else(|| Document {
            paras: Vec::new(),
            page_w: 816.0,
            page_h: 1056.0,
            margin_l: 96.0,
            margin_r: 96.0,
            margin_t: 96.0,
            margin_b: 96.0,
        });
        let lines = layout_lines(&doc, font);
        let cap = (doc.page_h - doc.margin_t - doc.margin_b).max(32.0);

        let mut pages = Vec::new();
        let mut start = 0;
        let mut used = 0.0f32;
        let mut page_top = 0.0f32;
        for (i, line) in lines.iter().enumerate() {
            if used > 0.0 && used + line.line_h > cap {
                pages.push(Page { start, end: i, top_y: page_top });
                start = i;
                used = 0.0;
                page_top = line.top;
            }
            used += line.advance;
        }
        pages.push(Page { start, end: lines.len(), top_y: page_top });

        DocxDoc { lines, pages, page_w: doc.page_w, page_h: doc.page_h, margin_t: doc.margin_t }
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
    pub fn page_size(&self) -> (f32, f32) {
        (self.page_w, self.page_h)
    }

    /// Render page `idx` into a device-px display list at `scale` (= zoom × dpr).
    pub fn render_page(&self, idx: usize, scale: f32) -> DisplayList {
        let mut dl = DisplayList::new((self.page_w * scale).max(1.0), (self.page_h * scale).max(1.0));
        let Some(page) = self.pages.get(idx) else { return dl };
        for li in page.start..page.end {
            let line = &self.lines[li];
            let local_top = self.margin_t + (line.top - page.top_y);
            emit_line(&mut dl, line, (local_top + line.ascent) * scale, scale);
        }
        dl
    }
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
