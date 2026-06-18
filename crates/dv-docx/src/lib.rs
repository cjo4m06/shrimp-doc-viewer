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

use std::collections::{HashMap, HashSet};
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

/// Partial run properties — `None` means "inherit". Used for direct overrides,
/// style definitions, and docDefaults so style inheritance can be resolved.
#[derive(Clone, Default)]
struct RPr {
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    strike: Option<bool>,
    size: Option<f32>,       // px
    vert_align: Option<i8>,  // 1=superscript, -1=subscript, 0=baseline
    color: Option<Color>,
    highlight: Option<Color>,
}

/// Partial paragraph properties.
#[derive(Clone, Default)]
struct PPr {
    align: Option<Align>,
}

fn overlay_rpr(base: &mut RPr, top: &RPr) {
    if top.bold.is_some() {
        base.bold = top.bold;
    }
    if top.italic.is_some() {
        base.italic = top.italic;
    }
    if top.underline.is_some() {
        base.underline = top.underline;
    }
    if top.strike.is_some() {
        base.strike = top.strike;
    }
    if top.size.is_some() {
        base.size = top.size;
    }
    if top.vert_align.is_some() {
        base.vert_align = top.vert_align;
    }
    if top.color.is_some() {
        base.color = top.color;
    }
    if top.highlight.is_some() {
        base.highlight = top.highlight;
    }
}

#[derive(Clone)]
struct Run {
    text: String, // may contain '\t' (tab) and '\n' (line break) control chars
    direct: RPr,
    r_style: Option<String>,
    // resolved by resolve_document():
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    size: f32,
    vert_align: i8,
    color: Color,
    highlight: Option<Color>,
}

#[derive(Clone)]
struct ParaBorder {
    top: bool,
    bottom: bool,
    color: Color,
    size: f32, // px
}

impl Default for ParaBorder {
    fn default() -> Self {
        ParaBorder { top: false, bottom: false, color: Color::BLACK, size: 1.0 }
    }
}

struct Para {
    runs: Vec<Run>,
    direct: PPr,
    p_style: Option<String>,
    // list/numbering (from w:numPr) + direct indents (twips→px):
    num_id: Option<u32>,
    num_ilvl: u32,
    d_ind_left: Option<f32>,
    d_ind_hanging: Option<f32>,
    // inline image (w:drawing): relationship id + extent (px)
    image_rid: Option<String>,
    image_w: f32,
    image_h: f32,
    // spacing (px): before/after; line height multiple (0 = auto)
    spc_before: f32,
    spc_after: f32,
    line_mult: f32,    // w:spacing line/240 when lineRule=auto (0 = unset)
    line_exact: f32,   // exact/atLeast line height px (0 = unset)
    pbdr: ParaBorder,
    shd: Option<Color>,
    tab_stops: Vec<(f32, Align)>, // (position px, alignment)
    page_break_before: bool,
    // resolved:
    align: Align,
    marker: Option<String>, // bullet/number prefix
    indent: f32,            // body left indent (px)
    hanging: f32,           // first-line marker hang (px)
    image: Option<dv_image::DecodedImage>,
}

impl Default for Para {
    fn default() -> Self {
        Para {
            runs: Vec::new(),
            direct: PPr::default(),
            p_style: None,
            num_id: None,
            num_ilvl: 0,
            d_ind_left: None,
            d_ind_hanging: None,
            image_rid: None,
            image_w: 0.0,
            image_h: 0.0,
            spc_before: 0.0,
            spc_after: 0.0,
            line_mult: 0.0,
            line_exact: 0.0,
            pbdr: ParaBorder::default(),
            shd: None,
            tab_stops: Vec::new(),
            page_break_before: false,
            align: Align::Left,
            marker: None,
            indent: 0.0,
            hanging: 0.0,
            image: None,
        }
    }
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
const EMU_TO_PX: f32 = 1.0 / 9525.0; // 914400 EMU/inch ÷ 96 dpi

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

/// Named `w:highlight` colour -> RGB (`none` -> None).
fn highlight_color(name: &str) -> Option<Color> {
    let c = match name {
        "black" => (0, 0, 0),
        "blue" => (0, 0, 255),
        "cyan" => (0, 255, 255),
        "green" => (0, 255, 0),
        "magenta" => (255, 0, 255),
        "red" => (255, 0, 0),
        "yellow" => (255, 255, 0),
        "white" => (255, 255, 255),
        "darkBlue" => (0, 0, 139),
        "darkCyan" => (0, 139, 139),
        "darkGreen" => (0, 100, 0),
        "darkMagenta" => (139, 0, 139),
        "darkRed" => (139, 0, 0),
        "darkYellow" => (128, 128, 0),
        "darkGray" => (169, 169, 169),
        "lightGray" => (211, 211, 211),
        _ => return None,
    };
    Some(Color::rgb(c.0, c.1, c.2))
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
    let mut cur_run: Option<Run> = None;
    let mut in_t = false;
    let mut in_tabs = false;
    let mut in_pbdr = false;

    let open = |doc: &mut Document,
                cur_para: &mut Option<Para>,
                cur_run: &mut Option<Run>,
                in_t: &mut bool,
                in_tabs: &mut bool,
                in_pbdr: &mut bool,
                e: &BytesStart| {
        match e.name().as_ref() {
            b"w:p" => {
                *cur_para = Some(Para::default());
            }
            b"w:numId" => {
                if let (Some(p), Some(v)) = (cur_para.as_mut(), get_attr(e, b"w:val").and_then(|s| s.parse::<u32>().ok())) {
                    p.num_id = Some(v);
                }
            }
            b"w:ilvl" => {
                if let (Some(p), Some(v)) = (cur_para.as_mut(), get_attr(e, b"w:val").and_then(|s| s.parse::<u32>().ok())) {
                    p.num_ilvl = v;
                }
            }
            b"w:ind" => {
                if let Some(p) = cur_para.as_mut() {
                    if cur_run.is_none() {
                        p.d_ind_left = get_attr(e, b"w:left").and_then(|s| s.parse::<f32>().ok()).map(|v| v * TWIP_TO_PX);
                        p.d_ind_hanging = get_attr(e, b"w:hanging").and_then(|s| s.parse::<f32>().ok()).map(|v| v * TWIP_TO_PX);
                    }
                }
            }
            b"w:pStyle" => {
                // Only the paragraph-level pStyle (in pPr, before any run).
                if cur_run.is_none() {
                    if let (Some(p), Some(v)) = (cur_para.as_mut(), get_attr(e, b"w:val")) {
                        p.p_style = Some(v);
                    }
                }
            }
            b"w:jc" => {
                if let Some(p) = cur_para.as_mut() {
                    p.direct.align = Some(match get_attr(e, b"w:val").as_deref() {
                        Some("center") => Align::Center,
                        Some("right") | Some("end") => Align::Right,
                        _ => Align::Left,
                    });
                }
            }
            b"w:r" => {
                *cur_run = Some(Run {
                    text: String::new(),
                    direct: RPr::default(),
                    r_style: None,
                    bold: false,
                    italic: false,
                    underline: false,
                    strike: false,
                    size: DEFAULT_SIZE_PX,
                    vert_align: 0,
                    color: Color::BLACK,
                    highlight: None,
                });
            }
            b"w:rStyle" => {
                if let (Some(r), Some(v)) = (cur_run.as_mut(), get_attr(e, b"w:val")) {
                    r.r_style = Some(v);
                }
            }
            b"w:b" => {
                if let Some(r) = cur_run.as_mut() {
                    let v = get_attr(e, b"w:val");
                    r.direct.bold = Some(!matches!(v.as_deref(), Some("false") | Some("0") | Some("off")));
                }
            }
            b"w:sz" => {
                if let (Some(r), Some(v)) = (cur_run.as_mut(), get_attr(e, b"w:val").and_then(|s| s.parse::<f32>().ok())) {
                    r.direct.size = Some(v * 2.0 / 3.0); // half-points -> px
                }
            }
            b"w:color" => {
                if let (Some(r), Some(v)) = (cur_run.as_mut(), get_attr(e, b"w:val")) {
                    r.direct.color = Some(parse_color(&v));
                }
            }
            b"w:i" => {
                if let Some(r) = cur_run.as_mut() {
                    r.direct.italic = Some(!matches!(get_attr(e, b"w:val").as_deref(), Some("false") | Some("0") | Some("off")));
                }
            }
            b"w:u" => {
                if let Some(r) = cur_run.as_mut() {
                    r.direct.underline = Some(get_attr(e, b"w:val").as_deref() != Some("none"));
                }
            }
            b"w:strike" => {
                if let Some(r) = cur_run.as_mut() {
                    r.direct.strike = Some(!matches!(get_attr(e, b"w:val").as_deref(), Some("false") | Some("0") | Some("off")));
                }
            }
            b"w:vertAlign" => {
                if let Some(r) = cur_run.as_mut() {
                    r.direct.vert_align = Some(match get_attr(e, b"w:val").as_deref() {
                        Some("superscript") => 1,
                        Some("subscript") => -1,
                        _ => 0,
                    });
                }
            }
            b"w:highlight" => {
                if let (Some(r), Some(v)) = (cur_run.as_mut(), get_attr(e, b"w:val")) {
                    r.direct.highlight = highlight_color(&v);
                }
            }
            b"w:shd" => {
                let fill = get_attr(e, b"w:fill");
                let col = fill.as_deref().filter(|f| !f.eq_ignore_ascii_case("auto") && *f != "FFFFFF").map(parse_color);
                if let Some(r) = cur_run.as_mut() {
                    if r.direct.highlight.is_none() {
                        r.direct.highlight = col;
                    }
                } else if let Some(p) = cur_para.as_mut() {
                    p.shd = col;
                }
            }
            b"w:spacing" => {
                if let Some(p) = cur_para.as_mut() {
                    if cur_run.is_none() {
                        if let Some(v) = get_attr(e, b"w:before").and_then(|s| s.parse::<f32>().ok()) {
                            p.spc_before = v * TWIP_TO_PX;
                        }
                        if let Some(v) = get_attr(e, b"w:after").and_then(|s| s.parse::<f32>().ok()) {
                            p.spc_after = v * TWIP_TO_PX;
                        }
                        if let Some(line) = get_attr(e, b"w:line").and_then(|s| s.parse::<f32>().ok()) {
                            match get_attr(e, b"w:lineRule").as_deref() {
                                Some("exact") | Some("atLeast") => p.line_exact = line * TWIP_TO_PX,
                                _ => p.line_mult = line / 240.0,
                            }
                        }
                    }
                }
            }
            b"w:pBdr" => *in_pbdr = true,
            b"w:top" | b"w:bottom" => {
                if *in_pbdr {
                    if let Some(p) = cur_para.as_mut() {
                        let on = !matches!(get_attr(e, b"w:val").as_deref(), Some("none") | Some("nil") | None);
                        if e.name().as_ref() == b"w:top" {
                            p.pbdr.top = on;
                        } else {
                            p.pbdr.bottom = on;
                        }
                        if let Some(sz) = get_attr(e, b"w:sz").and_then(|s| s.parse::<f32>().ok()) {
                            p.pbdr.size = (sz / 8.0 * 4.0 / 3.0).max(0.75); // eighths of a point -> px
                        }
                        if let Some(c) = get_attr(e, b"w:color") {
                            p.pbdr.color = parse_color(&c);
                        }
                    }
                }
            }
            b"w:tabs" => *in_tabs = true,
            b"w:tab" => {
                if *in_tabs {
                    if let (Some(p), Some(pos)) = (cur_para.as_mut(), get_attr(e, b"w:pos").and_then(|s| s.parse::<f32>().ok())) {
                        let al = match get_attr(e, b"w:val").as_deref() {
                            Some("center") => Align::Center,
                            Some("right") | Some("end") => Align::Right,
                            _ => Align::Left,
                        };
                        p.tab_stops.push((pos * TWIP_TO_PX, al));
                    }
                } else if let Some(r) = cur_run.as_mut() {
                    r.text.push('\t');
                }
            }
            b"w:br" => {
                if let Some(r) = cur_run.as_mut() {
                    r.text.push('\n');
                }
            }
            b"w:pageBreakBefore" => {
                if let Some(p) = cur_para.as_mut() {
                    p.page_break_before = !matches!(get_attr(e, b"w:val").as_deref(), Some("false") | Some("0") | Some("off"));
                }
            }
            b"w:t" => *in_t = true,
            b"wp:extent" => {
                if let Some(p) = cur_para.as_mut() {
                    if let Some(cx) = get_attr(e, b"cx").and_then(|s| s.parse::<f32>().ok()) {
                        p.image_w = cx * EMU_TO_PX;
                    }
                    if let Some(cy) = get_attr(e, b"cy").and_then(|s| s.parse::<f32>().ok()) {
                        p.image_h = cy * EMU_TO_PX;
                    }
                }
            }
            b"a:blip" => {
                if let (Some(p), Some(rid)) = (cur_para.as_mut(), get_attr(e, b"r:embed")) {
                    p.image_rid = Some(rid);
                }
            }
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
                open(&mut doc, &mut cur_para, &mut cur_run, &mut in_t, &mut in_tabs, &mut in_pbdr, &e);
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
                b"w:tabs" => in_tabs = false,
                b"w:pBdr" => in_pbdr = false,
                b"w:r" => {
                    if let (Some(p), Some(r)) = (cur_para.as_mut(), cur_run.take()) {
                        if !r.text.is_empty() {
                            p.runs.push(r);
                        }
                    }
                }
                b"w:p" => {
                    if let Some(p) = cur_para.take() {
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
#[derive(Clone, Copy, PartialEq)]
enum IKind {
    Glyph,
    Tab,
    Break,
}

struct Item {
    kind: IKind,
    gid: u32,
    advance: f32,
    x_off: f32,
    size: f32,
    color: Color,
    bold: bool,
    underline: bool,
    strike: bool,
    highlight: Option<Color>,
    vshift: f32, // baseline shift px (negative = up, for super/subscript)
    break_after: bool,
    is_space: bool,
}

fn shape_para(font: &FontData, para: &Para) -> Vec<Item> {
    let mut items = Vec::new();
    for run in &para.runs {
        let (sz, vshift) = match run.vert_align {
            1 => (run.size * 0.65, -run.size * 0.33),
            -1 => (run.size * 0.65, run.size * 0.12),
            _ => (run.size, 0.0),
        };
        // Split on tab/break control chars; shape the text segments.
        let mut seg = String::new();
        let flush = |seg: &mut String, items: &mut Vec<Item>| {
            if seg.is_empty() {
                return;
            }
            let shaped = shape(font, seg, sz);
            let scale = sz / shaped.units_per_em.max(1.0);
            for g in &shaped.glyphs {
                let ch = seg.get(g.cluster as usize..).and_then(|s| s.chars().next()).unwrap_or(' ');
                let is_space = ch.is_whitespace();
                items.push(Item {
                    kind: IKind::Glyph,
                    gid: g.glyph_id,
                    advance: g.x_advance * scale,
                    x_off: g.x_offset * scale,
                    size: sz,
                    color: run.color,
                    bold: run.bold,
                    underline: run.underline,
                    strike: run.strike,
                    highlight: run.highlight,
                    vshift,
                    break_after: is_space || is_cjk(ch),
                    is_space,
                });
            }
            seg.clear();
        };
        for ch in run.text.chars() {
            match ch {
                '\t' => {
                    flush(&mut seg, &mut items);
                    items.push(ctrl_item(IKind::Tab, run));
                }
                '\n' => {
                    flush(&mut seg, &mut items);
                    items.push(ctrl_item(IKind::Break, run));
                }
                _ => seg.push(ch),
            }
        }
        flush(&mut seg, &mut items);
    }
    items
}

fn ctrl_item(kind: IKind, run: &Run) -> Item {
    Item {
        kind,
        gid: 0,
        advance: 0.0,
        x_off: 0.0,
        size: run.size,
        color: run.color,
        bold: false,
        underline: false,
        strike: false,
        highlight: None,
        vshift: 0.0,
        break_after: true,
        is_space: true,
    }
}

fn wrap(items: Vec<Item>, content_w: f32) -> Vec<Vec<Item>> {
    let mut lines: Vec<Vec<Item>> = Vec::new();
    let mut cur: Vec<Item> = Vec::new();
    let mut cur_w = 0.0f32;

    let last_break = |line: &[Item]| line.iter().rposition(|it| it.break_after);

    for it in items {
        if it.kind == IKind::Break {
            // explicit line break (w:br): end this line, drop the marker
            lines.push(std::mem::take(&mut cur));
            cur_w = 0.0;
            continue;
        }
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

// --- numbering.xml (lists) -------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum NumFmt {
    Decimal,
    DecimalZero,
    LowerLetter,
    UpperLetter,
    LowerRoman,
    UpperRoman,
    Bullet,
    None_,
}

#[derive(Clone)]
struct Lvl {
    fmt: NumFmt,
    text: String,
    start: i32,
    ind_left: Option<f32>,
    ind_hanging: Option<f32>,
}

#[derive(Default)]
struct Numbering {
    num_to_abstract: HashMap<u32, u32>,
    abstracts: HashMap<u32, HashMap<u32, Lvl>>, // abstractNumId -> ilvl -> Lvl
}

fn num_fmt(s: &str) -> NumFmt {
    match s {
        "decimalZero" => NumFmt::DecimalZero,
        "lowerLetter" => NumFmt::LowerLetter,
        "upperLetter" => NumFmt::UpperLetter,
        "lowerRoman" => NumFmt::LowerRoman,
        "upperRoman" => NumFmt::UpperRoman,
        "bullet" => NumFmt::Bullet,
        "none" => NumFmt::None_,
        _ => NumFmt::Decimal,
    }
}

fn parse_numbering_xml(xml: &str) -> Numbering {
    let mut nb = Numbering::default();
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut cur_abstract: Option<u32> = None;
    let mut cur_ilvl: Option<u32> = None;
    let mut cur_lvl: Option<Lvl> = None;
    let mut cur_num: Option<u32> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match e.name().as_ref() {
                b"w:abstractNum" => cur_abstract = get_attr(&e, b"w:abstractNumId").and_then(|s| s.parse().ok()),
                b"w:lvl" => {
                    cur_ilvl = get_attr(&e, b"w:ilvl").and_then(|s| s.parse().ok());
                    cur_lvl = Some(Lvl { fmt: NumFmt::Decimal, text: String::new(), start: 1, ind_left: None, ind_hanging: None });
                }
                b"w:start" => {
                    if let (Some(l), Some(v)) = (cur_lvl.as_mut(), get_attr(&e, b"w:val").and_then(|s| s.parse().ok())) {
                        l.start = v;
                    }
                }
                b"w:numFmt" => {
                    if let (Some(l), Some(v)) = (cur_lvl.as_mut(), get_attr(&e, b"w:val")) {
                        l.fmt = num_fmt(&v);
                    }
                }
                b"w:lvlText" => {
                    if let (Some(l), Some(v)) = (cur_lvl.as_mut(), get_attr(&e, b"w:val")) {
                        l.text = v;
                    }
                }
                b"w:ind" => {
                    if let Some(l) = cur_lvl.as_mut() {
                        l.ind_left = get_attr(&e, b"w:left").and_then(|s| s.parse::<f32>().ok()).map(|v| v * TWIP_TO_PX);
                        l.ind_hanging = get_attr(&e, b"w:hanging").and_then(|s| s.parse::<f32>().ok()).map(|v| v * TWIP_TO_PX);
                    }
                }
                b"w:num" => cur_num = get_attr(&e, b"w:numId").and_then(|s| s.parse().ok()),
                b"w:abstractNumId" => {
                    if let (Some(n), Some(a)) = (cur_num, get_attr(&e, b"w:val").and_then(|s| s.parse::<u32>().ok())) {
                        nb.num_to_abstract.insert(n, a);
                    }
                }
                _ => {}
            },
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"w:lvl" => {
                    if let (Some(a), Some(il), Some(l)) = (cur_abstract, cur_ilvl.take(), cur_lvl.take()) {
                        nb.abstracts.entry(a).or_default().insert(il, l);
                    }
                }
                b"w:abstractNum" => cur_abstract = None,
                b"w:num" => cur_num = None,
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    nb
}

fn fmt_counter(fmt: NumFmt, v: i32) -> String {
    match fmt {
        NumFmt::DecimalZero => {
            if (0..10).contains(&v) {
                format!("0{v}")
            } else {
                v.to_string()
            }
        }
        NumFmt::LowerLetter => alpha(v, false),
        NumFmt::UpperLetter => alpha(v, true),
        NumFmt::LowerRoman => roman(v, false),
        NumFmt::UpperRoman => roman(v, true),
        _ => v.to_string(),
    }
}

fn alpha(mut v: i32, upper: bool) -> String {
    if v <= 0 {
        return v.to_string();
    }
    let mut s = Vec::new();
    while v > 0 {
        v -= 1;
        s.push((b'a' + (v % 26) as u8) as char);
        v /= 26;
    }
    let out: String = s.into_iter().rev().collect();
    if upper {
        out.to_uppercase()
    } else {
        out
    }
}

fn roman(v: i32, upper: bool) -> String {
    if v <= 0 {
        return v.to_string();
    }
    const T: [(i32, &str); 13] = [
        (1000, "m"), (900, "cm"), (500, "d"), (400, "cd"), (100, "c"), (90, "xc"),
        (50, "l"), (40, "xl"), (10, "x"), (9, "ix"), (5, "v"), (4, "iv"), (1, "i"),
    ];
    let mut n = v;
    let mut out = String::new();
    for (val, sym) in T {
        while n >= val {
            out.push_str(sym);
            n -= val;
        }
    }
    if upper {
        out.to_uppercase()
    } else {
        out
    }
}

fn bullet_char(text: &str) -> String {
    let c = text.chars().next().unwrap_or('\u{2022}');
    let mapped = match c as u32 {
        0xF0B7 => '\u{2022}', // •
        0xF0A7 => '\u{25AA}', // ▪
        0xF06F => '\u{25CB}', // ○
        _ => c,
    };
    mapped.to_string()
}

fn substitute(text: &str, num_id: u32, abstract_id: u32, nb: &Numbering, counters: &HashMap<(u32, u32), i32>) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '%' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit() {
            let ref_ilvl = chars[i + 1].to_digit(10).unwrap().saturating_sub(1);
            let lvl = nb.abstracts.get(&abstract_id).and_then(|m| m.get(&ref_ilvl));
            let val = counters.get(&(num_id, ref_ilvl)).copied().unwrap_or_else(|| lvl.map(|l| l.start).unwrap_or(1));
            let fmt = lvl.map(|l| l.fmt).unwrap_or(NumFmt::Decimal);
            out.push_str(&fmt_counter(fmt, val));
            i += 2;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Walk paragraphs in order, maintaining per-(numId,ilvl) counters, and bake
/// each list paragraph's marker + indents.
fn resolve_numbering(doc: &mut Document, nb: &Numbering) {
    let mut counters: HashMap<(u32, u32), i32> = HashMap::new();
    for para in &mut doc.paras {
        let num_id = match para.num_id {
            Some(n) if n != 0 => n,
            _ => continue,
        };
        let ilvl = para.num_ilvl;
        let abstract_id = match nb.num_to_abstract.get(&num_id) {
            Some(a) => *a,
            None => continue,
        };
        let lvl = match nb.abstracts.get(&abstract_id).and_then(|m| m.get(&ilvl)) {
            Some(l) => l.clone(),
            None => continue,
        };

        let entry = counters.entry((num_id, ilvl)).or_insert(lvl.start - 1);
        *entry += 1;
        // reset deeper levels of this list
        let deeper: Vec<(u32, u32)> = counters.keys().filter(|(n, l)| *n == num_id && *l > ilvl).copied().collect();
        for k in deeper {
            counters.remove(&k);
        }

        let marker = match lvl.fmt {
            NumFmt::Bullet => bullet_char(&lvl.text),
            NumFmt::None_ => String::new(),
            _ => substitute(&lvl.text, num_id, abstract_id, nb, &counters),
        };
        para.marker = if marker.is_empty() { None } else { Some(marker) };
        para.indent = para.d_ind_left.or(lvl.ind_left).unwrap_or(((ilvl + 1) as f32) * 36.0);
        para.hanging = para.d_ind_hanging.or(lvl.ind_hanging).unwrap_or(18.0);
    }
}

/// One glyph placed on a line, page-relative x (at zoom 1), with paint style.
#[derive(Clone, Copy)]
struct PlacedGlyph {
    id: u32,
    x: f32,
    advance: f32,
    size: f32,
    color: Color,
    bold: bool,
    underline: bool,
    strike: bool,
    highlight: Option<Color>,
    vshift: f32,
}

/// An image placed on its own line (page-relative, at zoom 1).
struct ImageBox {
    rgba: Vec<u8>,
    src_w: u32,
    src_h: u32,
    x: f32,
    w: f32,
    h: f32,
}

/// A laid-out line at zoom 1. `top` is the cumulative content-y of its top edge.
struct Line {
    placed: Vec<PlacedGlyph>,
    image: Option<ImageBox>,
    top: f32,
    line_h: f32,
    advance: f32, // line_h + trailing paragraph spacing
    ascent: f32,
    // paragraph background + borders (drawn across [left,right])
    shd: Option<Color>,
    bdr_top: Option<(Color, f32)>,
    bdr_bottom: Option<(Color, f32)>,
    left: f32,
    right: f32,
}

/// Next left tab stop strictly greater than `x` (px, absolute). Uses defined
/// stops, else a default 0.5in grid from the left margin.
fn next_tab(x: f32, margin_l: f32, stops: &[(f32, Align)]) -> f32 {
    for (pos, _) in stops {
        let abs = margin_l + pos;
        if abs > x + 0.5 {
            return abs;
        }
    }
    let grid = 48.0; // 0.5 inch
    let rel = (x - margin_l).max(0.0);
    margin_l + ((rel / grid).floor() + 1.0) * grid
}

/// Shape + wrap all paragraphs into a flat list of laid-out lines (zoom 1).
fn layout_lines(doc: &Document, font: &FontData) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut top = 0.0f32;

    for para in &doc.paras {
        let body_left = doc.margin_l + para.indent;
        let content_w = (doc.page_w - doc.margin_r - body_left).max(32.0);
        let right = doc.page_w - doc.margin_r;
        top += para.spc_before;

        // Inline image paragraph: render as its own block, scaled to fit width.
        if let Some(img) = &para.image {
            let mut w = para.image_w.max(1.0);
            let mut h = para.image_h.max(1.0);
            if w > content_w {
                h *= content_w / w;
                w = content_w;
            }
            let space = para.spc_after.max(max_para_size(para) * 0.3);
            lines.push(Line {
                placed: Vec::new(),
                image: Some(ImageBox { rgba: img.rgba.clone(), src_w: img.width, src_h: img.height, x: body_left, w, h }),
                top,
                line_h: h,
                advance: h + space,
                ascent: h,
                shd: None,
                bdr_top: None,
                bdr_bottom: None,
                left: body_left,
                right,
            });
            top += h + space;
            continue;
        }

        let bdr_top = if para.pbdr.top { Some((para.pbdr.color, para.pbdr.size)) } else { None };
        let bdr_bottom = if para.pbdr.bottom { Some((para.pbdr.color, para.pbdr.size)) } else { None };

        let items = shape_para(font, para);
        if items.is_empty() {
            let line_h = if para.line_exact > 0.0 { para.line_exact } else { DEFAULT_SIZE_PX * 1.2 };
            lines.push(Line {
                placed: Vec::new(),
                image: None,
                top,
                line_h,
                advance: line_h + para.spc_after,
                ascent: DEFAULT_SIZE_PX * 0.92,
                shd: para.shd,
                bdr_top,
                bdr_bottom,
                left: body_left,
                right,
            });
            top += line_h + para.spc_after;
            continue;
        }
        let wrapped = wrap(items, content_w);
        let n = wrapped.len();
        for (li, line) in wrapped.iter().enumerate() {
            let max_size = line.iter().map(|i| i.size).fold(DEFAULT_SIZE_PX, f32::max);
            let line_h = if para.line_exact > 0.0 {
                para.line_exact
            } else if para.line_mult > 0.0 {
                max_size * para.line_mult
            } else {
                max_size * 1.2
            };
            let lw = line_width(line);
            let mut x = match para.align {
                Align::Left => body_left,
                Align::Center => body_left + (content_w - lw) / 2.0,
                Align::Right => body_left + (content_w - lw),
            };
            let mut placed = Vec::with_capacity(line.len() + 4);

            // List marker on the first line, hung to the left of the body text.
            if li == 0 {
                if let Some(marker) = &para.marker {
                    let msize = para.runs.first().map(|r| r.size).unwrap_or(DEFAULT_SIZE_PX);
                    let mcolor = para.runs.first().map(|r| r.color).unwrap_or(Color::BLACK);
                    let shaped = shape(font, marker, msize);
                    let sc = msize / shaped.units_per_em.max(1.0);
                    let mut mx = body_left - para.hanging;
                    for g in &shaped.glyphs {
                        placed.push(PlacedGlyph {
                            id: g.glyph_id,
                            x: mx + g.x_offset * sc,
                            advance: g.x_advance * sc,
                            size: msize,
                            color: mcolor,
                            bold: false,
                            underline: false,
                            strike: false,
                            highlight: None,
                            vshift: 0.0,
                        });
                        mx += g.x_advance * sc;
                    }
                }
            }

            for it in line {
                if it.kind == IKind::Tab {
                    x = next_tab(x, doc.margin_l, &para.tab_stops);
                    continue;
                }
                placed.push(PlacedGlyph {
                    id: it.gid,
                    x: x + it.x_off,
                    advance: it.advance,
                    size: it.size,
                    color: it.color,
                    bold: it.bold,
                    underline: it.underline,
                    strike: it.strike,
                    highlight: it.highlight,
                    vshift: it.vshift,
                });
                x += it.advance;
            }
            let advance = line_h + if li + 1 == n { para.spc_after } else { 0.0 };
            lines.push(Line {
                placed,
                image: None,
                top,
                line_h,
                advance,
                ascent: max_size * 0.92,
                shd: para.shd,
                bdr_top: if li == 0 { bdr_top } else { None },
                bdr_bottom: if li + 1 == n { bdr_bottom } else { None },
                left: body_left,
                right,
            });
            top += advance;
        }
    }
    lines
}

/// Emit one line's glyphs (grouped by run style) at a device `baseline`/`scale`.
fn emit_line(dl: &mut DisplayList, line: &Line, baseline: f32, scale: f32) {
    if let Some(im) = &line.image {
        let y_top = baseline - line.ascent * scale; // ascent == image height for image lines
        dl.push(Command::Image {
            rgba: im.rgba.clone(),
            src_w: im.src_w,
            src_h: im.src_h,
            x: im.x * scale,
            y: y_top,
            w: im.w * scale,
            h: im.h * scale,
            clip: None,
        });
        return;
    }

    // Paragraph shading + borders span the full body width.
    let top_y = baseline - line.ascent * scale;
    let bot_y = top_y + line.line_h * scale;
    let (lx, rx) = (line.left * scale, line.right * scale);
    if let Some(c) = line.shd {
        dl.push(fill_box(lx, top_y, rx - lx, bot_y - top_y, c));
    }
    if let Some((c, w)) = line.bdr_top {
        dl.push(fill_box(lx, top_y, rx - lx, (w * scale).max(1.0), c));
    }
    if let Some((c, w)) = line.bdr_bottom {
        dl.push(fill_box(lx, bot_y - (w * scale).max(1.0), rx - lx, (w * scale).max(1.0), c));
    }

    // Run highlight backgrounds (group consecutive glyphs sharing a highlight).
    let mut i = 0;
    while i < line.placed.len() {
        let hl = line.placed[i].highlight;
        let x0 = line.placed[i].x;
        let mut x1 = x0;
        let mut sz = line.placed[i].size;
        while i < line.placed.len() && line.placed[i].highlight == hl {
            x1 = line.placed[i].x + line.placed[i].advance;
            sz = sz.max(line.placed[i].size);
            i += 1;
        }
        if let Some(c) = hl {
            dl.push(fill_box(x0 * scale, baseline - sz * 0.82 * scale, (x1 - x0) * scale, sz * 1.02 * scale, c));
        }
    }

    // Glyphs grouped by (size, color, bold, vshift).
    let mut i = 0;
    while i < line.placed.len() {
        let g0 = line.placed[i];
        let mut glyphs = Vec::new();
        while i < line.placed.len()
            && line.placed[i].size == g0.size
            && line.placed[i].color == g0.color
            && line.placed[i].bold == g0.bold
            && line.placed[i].vshift == g0.vshift
        {
            glyphs.push(PositionedGlyph { id: line.placed[i].id, x: line.placed[i].x * scale, y: baseline + g0.vshift * scale });
            i += 1;
        }
        dl.push(Command::Glyphs(GlyphRun { font: FontId(0), size: g0.size * scale, paint: Paint::Solid(g0.color), bold: g0.bold, glyphs }));
    }

    // Underline / strike rules (group consecutive runs sharing the decoration).
    for deco in 0..2 {
        let mut i = 0;
        while i < line.placed.len() {
            let on = if deco == 0 { line.placed[i].underline } else { line.placed[i].strike };
            let x0 = line.placed[i].x;
            let (mut x1, mut sz, col) = (x0, line.placed[i].size, line.placed[i].color);
            while i < line.placed.len()
                && (if deco == 0 { line.placed[i].underline } else { line.placed[i].strike }) == on
                && line.placed[i].color == col
            {
                x1 = line.placed[i].x + line.placed[i].advance;
                sz = sz.max(line.placed[i].size);
                i += 1;
            }
            if on && x1 > x0 {
                let y = if deco == 0 { baseline + sz * 0.12 * scale } else { baseline - sz * 0.28 * scale };
                dl.push(fill_box(x0 * scale, y, (x1 - x0) * scale, (sz * 0.06 * scale).max(1.0), col));
            }
        }
    }
}

/// A solid axis-aligned rectangle (device coords).
fn fill_box(x: f32, y: f32, w: f32, h: f32, color: Color) -> Command {
    let mut p = dv_ir::PathData::new();
    p.move_to(x, y);
    p.line_to(x + w, y);
    p.line_to(x + w, y + h);
    p.line_to(x, y + h);
    p.close();
    Command::FillPath { path: p, paint: Paint::Solid(color), fill_rule: dv_ir::FillRule::NonZero, transform: dv_ir::Transform::IDENTITY }
}

/// Lay out and render a DOCX into a single continuous page (no pagination).
pub fn render_document(bytes: &[u8], font: &FontData) -> DisplayList {
    let xml = match read_zip_entry(bytes, "word/document.xml") {
        Some(x) => x,
        None => return DisplayList::new(816.0, 200.0),
    };
    let mut doc = parse_document(&xml);
    let table = read_zip_entry(bytes, "word/styles.xml").map(|s| parse_styles_xml(&s)).unwrap_or_default();
    resolve_document(&mut doc, &table);
    let numbering = read_zip_entry(bytes, "word/numbering.xml").map(|s| parse_numbering_xml(&s)).unwrap_or_default();
    resolve_numbering(&mut doc, &numbering);
    resolve_images(&mut doc, bytes);
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
        let mut doc = read_zip_entry(bytes, "word/document.xml").map(|x| parse_document(&x)).unwrap_or_else(|| Document {
            paras: Vec::new(),
            page_w: 816.0,
            page_h: 1056.0,
            margin_l: 96.0,
            margin_r: 96.0,
            margin_t: 96.0,
            margin_b: 96.0,
        });
        let table = read_zip_entry(bytes, "word/styles.xml").map(|s| parse_styles_xml(&s)).unwrap_or_default();
        resolve_document(&mut doc, &table);
        let numbering = read_zip_entry(bytes, "word/numbering.xml").map(|s| parse_numbering_xml(&s)).unwrap_or_default();
        resolve_numbering(&mut doc, &numbering);
        resolve_images(&mut doc, bytes);
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

fn read_zip_entry(bytes: &[u8], name: &str) -> Option<String> {
    let mut zip = ZipArchive::new(Cursor::new(bytes.to_vec())).ok()?;
    let mut f = zip.by_name(name).ok()?;
    let mut s = String::new();
    f.read_to_string(&mut s).ok()?;
    Some(s)
}

fn read_zip_bytes(bytes: &[u8], name: &str) -> Option<Vec<u8>> {
    let mut zip = ZipArchive::new(Cursor::new(bytes.to_vec())).ok()?;
    let mut f = zip.by_name(name).ok()?;
    let mut v = Vec::new();
    f.read_to_end(&mut v).ok()?;
    Some(v)
}

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

fn resolve_rel(base_dir: &str, target: &str) -> String {
    if let Some(s) = target.strip_prefix('/') {
        return s.to_string();
    }
    let mut parts: Vec<&str> = base_dir.trim_end_matches('/').split('/').filter(|s| !s.is_empty()).collect();
    for seg in target.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            _ => parts.push(seg),
        }
    }
    parts.join("/")
}

/// Resolve each paragraph's inline image: rels lookup → media bytes → decode.
fn resolve_images(doc: &mut Document, bytes: &[u8]) {
    let rels = match read_zip_entry(bytes, "word/_rels/document.xml.rels") {
        Some(s) => rels_map(&s),
        None => return,
    };
    for para in &mut doc.paras {
        let Some(rid) = para.image_rid.clone() else { continue };
        let Some(target) = rels.get(&rid) else { continue };
        let path = resolve_rel("word", target);
        if let Some(img) = read_zip_bytes(bytes, &path).and_then(|b| dv_image::decode(&b)) {
            if para.image_w <= 0.0 {
                para.image_w = img.width as f32;
            }
            if para.image_h <= 0.0 {
                para.image_h = img.height as f32;
            }
            para.image = Some(img);
        }
    }
}

// --- styles.xml inheritance ------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum StyleKind {
    Paragraph,
    Character,
}

#[derive(Clone)]
struct Style {
    #[allow(dead_code)]
    kind: StyleKind,
    based_on: Option<String>,
    rpr: RPr,
    ppr: PPr,
}

#[derive(Default)]
struct StyleTable {
    styles: HashMap<String, Style>,
    default_rpr: RPr,
    default_ppr: PPr,
}

/// Synthetic fallback for common built-in styles referenced but not defined
/// (lightweight generators often `pStyle="Heading1"` without a styles.xml entry).
fn builtin_style(id: &str) -> Option<(RPr, PPr)> {
    let bold = |s: Option<f32>| RPr { bold: Some(true), size: s, ..RPr::default() };
    match id {
        "Title" => Some((bold(Some(28.0)), PPr { align: Some(Align::Center) })),
        "Heading1" => Some((bold(Some(24.0)), PPr::default())),
        "Heading2" => Some((bold(Some(20.0)), PPr::default())),
        "Heading3" => Some((bold(Some(17.0)), PPr::default())),
        "Heading4" => Some((bold(Some(15.0)), PPr::default())),
        "Heading5" | "Heading6" => Some((bold(None), PPr::default())),
        _ => None,
    }
}

fn parse_styles_xml(xml: &str) -> StyleTable {
    let mut table = StyleTable::default();
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut in_doc_defaults = false;
    let mut cur_id: Option<String> = None;
    let mut cur: Option<Style> = None;

    let bold_val = |e: &BytesStart| -> bool {
        !matches!(get_attr(e, b"w:val").as_deref(), Some("false") | Some("0") | Some("off"))
    };

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match e.name().as_ref() {
                b"w:docDefaults" => in_doc_defaults = true,
                b"w:style" => {
                    let kind = match get_attr(&e, b"w:type").as_deref() {
                        Some("character") => StyleKind::Character,
                        _ => StyleKind::Paragraph,
                    };
                    cur_id = get_attr(&e, b"w:styleId");
                    cur = Some(Style { kind, based_on: None, rpr: RPr::default(), ppr: PPr::default() });
                }
                b"w:basedOn" => {
                    if let Some(s) = cur.as_mut() {
                        s.based_on = get_attr(&e, b"w:val");
                    }
                }
                b"w:b" => {
                    let v = Some(bold_val(&e));
                    if in_doc_defaults {
                        table.default_rpr.bold = v;
                    } else if let Some(s) = cur.as_mut() {
                        s.rpr.bold = v;
                    }
                }
                b"w:sz" => {
                    if let Some(px) = get_attr(&e, b"w:val").and_then(|s| s.parse::<f32>().ok()).map(|v| v * 2.0 / 3.0) {
                        if in_doc_defaults {
                            table.default_rpr.size = Some(px);
                        } else if let Some(s) = cur.as_mut() {
                            s.rpr.size = Some(px);
                        }
                    }
                }
                b"w:color" => {
                    if let Some(c) = get_attr(&e, b"w:val").map(|v| parse_color(&v)) {
                        if in_doc_defaults {
                            table.default_rpr.color = Some(c);
                        } else if let Some(s) = cur.as_mut() {
                            s.rpr.color = Some(c);
                        }
                    }
                }
                b"w:jc" => {
                    let a = match get_attr(&e, b"w:val").as_deref() {
                        Some("center") => Align::Center,
                        Some("right") | Some("end") => Align::Right,
                        _ => Align::Left,
                    };
                    if in_doc_defaults {
                        table.default_ppr.align = Some(a);
                    } else if let Some(s) = cur.as_mut() {
                        s.ppr.align = Some(a);
                    }
                }
                _ => {}
            },
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"w:docDefaults" => in_doc_defaults = false,
                b"w:style" => {
                    if let (Some(id), Some(s)) = (cur_id.take(), cur.take()) {
                        table.styles.insert(id, s);
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    table
}

/// Style ids from a style following its `basedOn` chain: `[derived..root]`.
fn style_chain(table: &StyleTable, start: &str) -> Vec<String> {
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    let mut cur = Some(start.to_string());
    while let Some(id) = cur {
        if !seen.insert(id.clone()) {
            break;
        }
        cur = table.styles.get(&id).and_then(|s| s.based_on.clone());
        chain.push(id);
    }
    chain
}

fn style_rpr(table: &StyleTable, id: &str) -> RPr {
    table.styles.get(id).map(|s| s.rpr.clone()).unwrap_or_else(|| builtin_style(id).map(|(r, _)| r).unwrap_or_default())
}
fn style_align(table: &StyleTable, id: &str) -> Option<Align> {
    table.styles.get(id).map(|s| s.ppr.align).unwrap_or_else(|| builtin_style(id).and_then(|(_, p)| p.align))
}

fn resolve_run_rpr(table: &StyleTable, p_style: Option<&str>, r_style: Option<&str>, direct: &RPr) -> RPr {
    let mut acc = RPr::default();
    overlay_rpr(&mut acc, &table.default_rpr);
    if let Some(ps) = p_style {
        for id in style_chain(table, ps).iter().rev() {
            overlay_rpr(&mut acc, &style_rpr(table, id));
        }
    }
    if let Some(cs) = r_style {
        for id in style_chain(table, cs).iter().rev() {
            overlay_rpr(&mut acc, &style_rpr(table, id));
        }
    }
    overlay_rpr(&mut acc, direct);
    acc
}

fn resolve_para_align(table: &StyleTable, p_style: Option<&str>, direct: &PPr) -> Align {
    let mut acc = table.default_ppr.align;
    if let Some(ps) = p_style {
        for id in style_chain(table, ps).iter().rev() {
            if let Some(a) = style_align(table, id) {
                acc = Some(a);
            }
        }
    }
    if let Some(a) = direct.align {
        acc = Some(a);
    }
    acc.unwrap_or(Align::Left)
}

/// Bake resolved (size/bold/color/align) into each run/paragraph.
fn resolve_document(doc: &mut Document, table: &StyleTable) {
    for para in &mut doc.paras {
        para.align = resolve_para_align(table, para.p_style.as_deref(), &para.direct);
        let p_style = para.p_style.clone();
        for run in &mut para.runs {
            let rpr = resolve_run_rpr(table, p_style.as_deref(), run.r_style.as_deref(), &run.direct);
            run.bold = rpr.bold.unwrap_or(false);
            run.italic = rpr.italic.unwrap_or(false);
            run.underline = rpr.underline.unwrap_or(false);
            run.strike = rpr.strike.unwrap_or(false);
            run.size = rpr.size.unwrap_or(DEFAULT_SIZE_PX);
            run.vert_align = rpr.vert_align.unwrap_or(0);
            run.color = rpr.color.unwrap_or(Color::BLACK);
            run.highlight = rpr.highlight;
        }
    }
}
