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
    Justify,
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
    page_break_after: bool, // a w:br type=page / mid-doc sectPr forces a new page after
    d_ind_right: f32,       // right indent (px)
    d_ind_first: f32,       // first-line indent (px, mutually exclusive with hanging)
    keep_lines: bool,
    // floating anchored drawings attached to this paragraph
    floats: Vec<Float>,
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
            page_break_after: false,
            d_ind_right: 0.0,
            d_ind_first: 0.0,
            keep_lines: false,
            floats: Vec::new(),
            align: Align::Left,
            marker: None,
            indent: 0.0,
            hanging: 0.0,
            image: None,
        }
    }
}

/// A side's border (on for a visible line).
#[derive(Clone, Copy)]
struct Border {
    on: bool,
    color: Color,
    size: f32, // px
}
impl Default for Border {
    fn default() -> Self {
        Border { on: false, color: Color::BLACK, size: 1.0 }
    }
}

#[derive(Clone, Copy, Default)]
struct BorderSet {
    top: Border,
    bottom: Border,
    left: Border,
    right: Border,
    inside_h: Border,
    inside_v: Border,
}

#[derive(Clone, Copy, PartialEq)]
enum VMerge {
    None,
    Restart,
    Continue,
}

struct Cell {
    blocks: Vec<Block>,
    grid_span: u32,
    vmerge: VMerge,
    borders: BorderSet, // tcBorders override
    shd: Option<Color>,
    valign: u8, // 0 top, 1 center, 2 bottom
}

struct Row {
    cells: Vec<Cell>,
    min_h: f32, // trHeight (px)
    is_header: bool,
}

struct Table {
    grid: Vec<f32>, // column widths (px)
    rows: Vec<Row>,
    borders: BorderSet,
    cell_mar_l: f32,
    cell_mar_r: f32,
    cell_mar_t: f32,
    cell_mar_b: f32,
    ind: f32, // table left indent (px)
    align: Align,
}

enum Block {
    Para(Para),
    Table(Table),
}

/// One drawable inside a floating anchor (a text box / autoshape / connector).
struct DShape {
    x: f32, // px, relative to the float origin
    y: f32,
    w: f32,
    h: f32,
    fill: Option<Color>,
    outline: Option<(Color, f32)>,
    rounded: bool, // roundRect / callout -> rounded corners
    is_line: bool, // connector -> stroke a diagonal
    flip_v: bool,  // xfrm flipV -> diagonal runs bottom-left to top-right
    flip_h: bool,
    blocks: Vec<Block>,
    // text laid out at layout time (so rendering needs no font): glyphs + height
    glyphs: Vec<(u32, f32, f32, f32, Color, bool)>,
    text_h: f32,
}

/// A floating anchored drawing (`<w:drawing><wp:anchor>`): one or more shapes /
/// an image, positioned relative to the margin (x) and its anchor paragraph (y).
struct Float {
    off_x: f32, // px from left margin
    off_y: f32, // px from the anchor paragraph's top
    shapes: Vec<DShape>,
    image_rid: Option<String>,
    image: Option<dv_image::DecodedImage>,
    img_w: f32,
    img_h: f32,
    body_y: f32,  // anchor paragraph top in body coords (assigned at layout time)
    behind: bool, // behindDoc / wrapNone -> doesn't reserve body space
    grouped: bool, // shapes came through a real <wpg> group (page-absolute coords)
}

struct Document {
    blocks: Vec<Block>,
    page_w: f32,
    page_h: f32,
    margin_l: f32,
    margin_r: f32,
    margin_t: f32,
    margin_b: f32,
    header_dist: f32,
    footer_dist: f32,
    // section header/footer references: (type, relationship id)
    hdr_refs: Vec<(String, String)>,
    ftr_refs: Vec<(String, String)>,
    title_pg: bool,
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

/// A table being assembled during parsing.
struct TableBuild {
    table: Table,
    cur_row: Option<Row>,
    cur_cell: Option<Cell>,
}

fn parse_document(xml: &str) -> Document {
    let mut doc = Document {
        blocks: Vec::new(),
        page_w: 816.0,  // US Letter default (12240 twips)
        page_h: 1056.0, // 15840 twips
        margin_l: 96.0,
        margin_r: 96.0,
        margin_t: 96.0,
        margin_b: 96.0,
        header_dist: 48.0,
        footer_dist: 48.0,
        hdr_refs: Vec::new(),
        ftr_refs: Vec::new(),
        title_pg: false,
    };

    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();

    let mut cur_para: Option<Para> = None;
    let mut cur_run: Option<Run> = None;
    let mut in_t = false;
    let mut in_tabs = false;

    let open = |doc: &mut Document,
                cur_para: &mut Option<Para>,
                cur_run: &mut Option<Run>,
                in_t: &mut bool,
                in_tabs: &mut bool,
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
                        let pick = |a: &[u8]| get_attr(e, a).and_then(|s| s.parse::<f32>().ok());
                        p.d_ind_left = pick(b"w:left").or_else(|| pick(b"w:start"));
                        p.d_ind_hanging = pick(b"w:hanging");
                        if let Some(v) = pick(b"w:right").or_else(|| pick(b"w:end")) {
                            p.d_ind_right = v * TWIP_TO_PX;
                        }
                        if let Some(v) = pick(b"w:firstLine") {
                            p.d_ind_first = v * TWIP_TO_PX;
                        }
                        p.d_ind_left = p.d_ind_left.map(|v| v * TWIP_TO_PX);
                        p.d_ind_hanging = p.d_ind_hanging.map(|v| v * TWIP_TO_PX);
                    }
                }
            }
            b"w:keepLines" | b"w:keepNext" => {
                if let Some(p) = cur_para.as_mut() {
                    if cur_run.is_none() && !matches!(get_attr(e, b"w:val").as_deref(), Some("false") | Some("0") | Some("off")) {
                        p.keep_lines = true;
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
                        Some("both") | Some("distribute") => Align::Justify,
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
            b"w:br" => match get_attr(e, b"w:type").as_deref() {
                Some("page") | Some("column") => {
                    if let Some(p) = cur_para.as_mut() {
                        p.page_break_after = true; // forces the next paragraph onto a new page
                    }
                }
                _ => {
                    if let Some(r) = cur_run.as_mut() {
                        r.text.push('\n');
                    }
                }
            },
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
                if let Some(v) = get_attr(e, b"w:header").and_then(|s| s.parse::<f32>().ok()) {
                    doc.header_dist = v * TWIP_TO_PX;
                }
                if let Some(v) = get_attr(e, b"w:footer").and_then(|s| s.parse::<f32>().ok()) {
                    doc.footer_dist = v * TWIP_TO_PX;
                }
            }
            _ => {}
        }
    };

    // Table-building state.
    let mut tables: Vec<TableBuild> = Vec::new();
    let mut border_ctx: u8 = 0; // 0 none, 1 pBdr, 2 tblBorders, 3 tcBorders
    let mut in_tcpr = false;
    // depth inside revision-tracking *Change snapshots (pPrChange / tblGridChange /
    // tcPrChange …) — their contents are stale and must be ignored.
    let mut in_change: u32 = 0;
    let mut in_fallback: u32 = 0; // mc:Fallback (VML) — skip, prefer mc:Choice DrawingML
    // floating anchored drawing state
    let mut cur_float: Option<Float> = None;
    let mut cur_dshape: Option<DShape> = None;
    let mut in_txbx = false;
    let mut in_dln = false; // inside a:ln of a drawing shape
    let mut pos_target: u8 = 0; // 1=H, 2=V (capturing wp:posOffset text)
    // saved (body) para/run while inside a text box, so nested txbx paragraphs
    // don't clobber the paragraph that anchors the float.
    let mut para_stack: Vec<(Option<Para>, Option<Run>)> = Vec::new();
    // group transform stack for floats: (sx, sy, tx, ty) mapping raw shape coords
    // (EMU at top level, child units inside groups) -> px relative to the anchor.
    let base_tf = (EMU_TO_PX, EMU_TO_PX, 0.0, 0.0);
    let mut gstack: Vec<(f32, f32, f32, f32)> = vec![base_tf];
    let mut in_grpspr = false;
    let mut g_xfrm = [0.0f32; 8];

    loop {
        let event = reader.read_event_into(&mut buf);
        let is_empty = matches!(&event, Ok(Event::Empty(_)));
        match event {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = e.name();
                if name.as_ref().ends_with(b"Change") {
                    if !is_empty {
                        in_change += 1;
                    }
                    buf.clear();
                    continue;
                }
                if name.as_ref() == b"mc:Fallback" {
                    if !is_empty {
                        in_fallback += 1;
                    }
                    buf.clear();
                    continue;
                }
                if in_change > 0 || in_fallback > 0 {
                    buf.clear();
                    continue;
                }
                match name.as_ref() {
                    // ---- floating anchored drawings (text boxes / autoshapes) ----
                    b"wp:anchor" => {
                        let behind = get_attr(&e, b"behindDoc").as_deref() == Some("1");
                        cur_float = Some(Float { off_x: 0.0, off_y: 0.0, shapes: Vec::new(), image_rid: None, image: None, img_w: 0.0, img_h: 0.0, body_y: 0.0, behind, grouped: false });
                        gstack = vec![base_tf];
                        in_grpspr = false;
                    }
                    b"wp:wrapNone" => {
                        if let Some(f) = cur_float.as_mut() {
                            f.behind = true; // floats over text without reserving space
                        }
                    }
                    b"wp:positionH" => pos_target = 10, // armed for H (set to 1 on posOffset)
                    b"wp:positionV" => pos_target = 20,
                    b"wp:posOffset" => {
                        pos_target = if pos_target >= 20 { 2 } else { 1 };
                    }
                    b"wps:wsp" | b"wps:cxnSp" if cur_float.is_some() => {
                        cur_dshape = Some(DShape { x: 0.0, y: 0.0, w: 0.0, h: 0.0, fill: None, outline: None, rounded: false, is_line: false, flip_v: false, flip_h: false, blocks: Vec::new(), glyphs: Vec::new(), text_h: 0.0 });
                    }
                    b"wpg:grpSpPr" if cur_float.is_some() => {
                        in_grpspr = !is_empty;
                        g_xfrm = [0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0];
                    }
                    b"w:txbxContent" => {
                        in_txbx = true;
                        para_stack.push((cur_para.take(), cur_run.take()));
                    }
                    b"a:off" if in_grpspr => {
                        g_xfrm[0] = get_attr(&e, b"x").and_then(|v| v.parse().ok()).unwrap_or(0.0);
                        g_xfrm[1] = get_attr(&e, b"y").and_then(|v| v.parse().ok()).unwrap_or(0.0);
                    }
                    b"a:ext" if in_grpspr => {
                        g_xfrm[2] = get_attr(&e, b"cx").and_then(|v| v.parse().ok()).unwrap_or(1.0);
                        g_xfrm[3] = get_attr(&e, b"cy").and_then(|v| v.parse().ok()).unwrap_or(1.0);
                    }
                    b"a:chOff" if in_grpspr => {
                        g_xfrm[4] = get_attr(&e, b"x").and_then(|v| v.parse().ok()).unwrap_or(0.0);
                        g_xfrm[5] = get_attr(&e, b"y").and_then(|v| v.parse().ok()).unwrap_or(0.0);
                    }
                    b"a:chExt" if in_grpspr => {
                        g_xfrm[6] = get_attr(&e, b"cx").and_then(|v| v.parse().ok()).unwrap_or(1.0);
                        g_xfrm[7] = get_attr(&e, b"cy").and_then(|v| v.parse().ok()).unwrap_or(1.0);
                    }
                    b"a:xfrm" if cur_float.is_some() => {
                        if let Some(s) = cur_dshape.as_mut() {
                            s.flip_v = get_attr(&e, b"flipV").as_deref() == Some("1");
                            s.flip_h = get_attr(&e, b"flipH").as_deref() == Some("1");
                        }
                    }
                    b"a:off" if cur_float.is_some() => {
                        if let Some(s) = cur_dshape.as_mut() {
                            s.x = get_attr(&e, b"x").and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
                            s.y = get_attr(&e, b"y").and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
                        }
                    }
                    b"a:ext" if cur_float.is_some() => {
                        if let Some(s) = cur_dshape.as_mut() {
                            s.w = get_attr(&e, b"cx").and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
                            s.h = get_attr(&e, b"cy").and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
                        }
                    }
                    b"a:prstGeom" if cur_float.is_some() => {
                        if let Some(s) = cur_dshape.as_mut() {
                            let prst = get_attr(&e, b"prst").unwrap_or_default();
                            s.rounded = prst.contains("ound") || prst.contains("allout");
                            s.is_line = prst.contains("onnector") || prst == "line";
                        }
                    }
                    b"a:ln" if cur_float.is_some() => in_dln = !is_empty,
                    b"a:noFill" if cur_float.is_some() => {
                        if in_dln {
                            if let Some(s) = cur_dshape.as_mut() {
                                s.outline = None;
                            }
                        }
                    }
                    b"a:srgbClr" | b"a:schemeClr" if cur_float.is_some() => {
                        if let Some(col) = get_attr(&e, b"val").map(|v| parse_color(&v)) {
                            if let Some(s) = cur_dshape.as_mut() {
                                if in_dln {
                                    s.outline = Some((col, 1.0));
                                } else {
                                    s.fill = Some(col);
                                }
                            }
                        }
                    }
                    b"a:blip" if cur_float.is_some() => {
                        if let Some(f) = cur_float.as_mut() {
                            if let Some(rid) = get_attr(&e, b"r:embed") {
                                f.image_rid = Some(rid);
                            }
                        }
                    }
                    b"wp:extent" if cur_float.is_some() => {
                        if let Some(f) = cur_float.as_mut() {
                            f.img_w = get_attr(&e, b"cx").and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0) * EMU_TO_PX;
                            f.img_h = get_attr(&e, b"cy").and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0) * EMU_TO_PX;
                        }
                    }
                    // ---- table structure ----
                    b"w:tbl" => tables.push(TableBuild {
                        table: Table {
                            grid: Vec::new(),
                            rows: Vec::new(),
                            borders: BorderSet::default(),
                            cell_mar_l: 108.0 * TWIP_TO_PX,
                            cell_mar_r: 108.0 * TWIP_TO_PX,
                            cell_mar_t: 0.0,
                            cell_mar_b: 0.0,
                            ind: 0.0,
                            align: Align::Left,
                        },
                        cur_row: None,
                        cur_cell: None,
                    }),
                    b"w:tr" => {
                        if let Some(tb) = tables.last_mut() {
                            tb.cur_row = Some(Row { cells: Vec::new(), min_h: 0.0, is_header: false });
                        }
                    }
                    b"w:tc" => {
                        if let Some(tb) = tables.last_mut() {
                            tb.cur_cell = Some(Cell {
                                blocks: Vec::new(),
                                grid_span: 1,
                                vmerge: VMerge::None,
                                borders: BorderSet::default(),
                                shd: None,
                                valign: 0,
                            });
                        }
                    }
                    b"w:gridCol" => {
                        if let (Some(tb), Some(w)) = (tables.last_mut(), get_attr(&e, b"w:w").and_then(|s| s.parse::<f32>().ok())) {
                            tb.table.grid.push(w * TWIP_TO_PX);
                        }
                    }
                    b"w:tblBorders" => border_ctx = 2,
                    b"w:tcBorders" => border_ctx = 3,
                    b"w:pBdr" => border_ctx = 1,
                    b"w:tcPr" => in_tcpr = true,
                    b"w:gridSpan" => {
                        if let (Some(tb), Some(v)) = (tables.last_mut(), get_attr(&e, b"w:val").and_then(|s| s.parse::<u32>().ok())) {
                            if let Some(c) = tb.cur_cell.as_mut() {
                                c.grid_span = v.max(1);
                            }
                        }
                    }
                    b"w:vMerge" => {
                        if let Some(c) = tables.last_mut().and_then(|tb| tb.cur_cell.as_mut()) {
                            c.vmerge = match get_attr(&e, b"w:val").as_deref() {
                                Some("restart") => VMerge::Restart,
                                _ => VMerge::Continue,
                            };
                        }
                    }
                    b"w:vAlign" => {
                        if let Some(c) = tables.last_mut().and_then(|tb| tb.cur_cell.as_mut()) {
                            c.valign = match get_attr(&e, b"w:val").as_deref() {
                                Some("center") => 1,
                                Some("bottom") => 2,
                                _ => 0,
                            };
                        }
                    }
                    b"w:trHeight" => {
                        if let (Some(tb), Some(v)) = (tables.last_mut(), get_attr(&e, b"w:val").and_then(|s| s.parse::<f32>().ok())) {
                            if let Some(r) = tb.cur_row.as_mut() {
                                r.min_h = v * TWIP_TO_PX;
                            }
                        }
                    }
                    b"w:tblHeader" => {
                        if let Some(r) = tables.last_mut().and_then(|tb| tb.cur_row.as_mut()) {
                            r.is_header = true;
                        }
                    }
                    b"w:tblInd" => {
                        if let (Some(tb), Some(v)) = (tables.last_mut(), get_attr(&e, b"w:w").and_then(|s| s.parse::<f32>().ok())) {
                            tb.table.ind = v * TWIP_TO_PX;
                        }
                    }
                    b"w:jc" if in_tcpr || (tables.last().is_some() && cur_para.is_none()) => {
                        // table alignment (jc in tblPr) vs cell — only treat as table jc
                        if let Some(tb) = tables.last_mut() {
                            if cur_para.is_none() {
                                tb.table.align = match get_attr(&e, b"w:val").as_deref() {
                                    Some("center") => Align::Center,
                                    Some("right") | Some("end") => Align::Right,
                                    _ => Align::Left,
                                };
                            }
                        }
                    }
                    b"w:left" | b"w:right" | b"w:top" | b"w:bottom" | b"w:insideH" | b"w:insideV" => {
                        // Border side — route by context. (w:top/bottom/start/end of cell margins ignored here.)
                        if border_ctx != 0 {
                            let bd = parse_border(&e);
                            let side = name.as_ref();
                            let set: Option<&mut BorderSet> = match border_ctx {
                                2 => tables.last_mut().map(|tb| &mut tb.table.borders),
                                3 => tables.last_mut().and_then(|tb| tb.cur_cell.as_mut()).map(|c| &mut c.borders),
                                _ => None,
                            };
                            if let Some(set) = set {
                                match side {
                                    b"w:top" => set.top = bd,
                                    b"w:bottom" => set.bottom = bd,
                                    b"w:left" => set.left = bd,
                                    b"w:right" => set.right = bd,
                                    b"w:insideH" => set.inside_h = bd,
                                    b"w:insideV" => set.inside_v = bd,
                                    _ => {}
                                }
                            } else if border_ctx == 1 && (side == b"w:top" || side == b"w:bottom") {
                                if let Some(p) = cur_para.as_mut() {
                                    let on = bd.on;
                                    if side == b"w:top" {
                                        p.pbdr.top = on;
                                    } else {
                                        p.pbdr.bottom = on;
                                    }
                                    p.pbdr.color = bd.color;
                                    p.pbdr.size = bd.size;
                                }
                            }
                        }
                    }
                    b"w:shd" => {
                        let fill = get_attr(&e, b"w:fill");
                        let col = fill.as_deref().filter(|f| !f.eq_ignore_ascii_case("auto") && *f != "FFFFFF").map(parse_color);
                        if in_tcpr {
                            if let Some(c) = tables.last_mut().and_then(|tb| tb.cur_cell.as_mut()) {
                                c.shd = col;
                            }
                        } else if let Some(r) = cur_run.as_mut() {
                            if r.direct.highlight.is_none() {
                                r.direct.highlight = col;
                            }
                        } else if let Some(p) = cur_para.as_mut() {
                            p.shd = col;
                        }
                    }
                    b"w:headerReference" => {
                        if let (Some(ty), Some(id)) = (get_attr(&e, b"w:type"), get_attr(&e, b"r:id")) {
                            doc.hdr_refs.push((ty, id));
                        }
                    }
                    b"w:footerReference" => {
                        if let (Some(ty), Some(id)) = (get_attr(&e, b"w:type"), get_attr(&e, b"r:id")) {
                            doc.ftr_refs.push((ty, id));
                        }
                    }
                    b"w:titlePg" => doc.title_pg = true,
                    _ => open(&mut doc, &mut cur_para, &mut cur_run, &mut in_t, &mut in_tabs, &e),
                }
            }
            Ok(Event::Text(t)) => {
                if pos_target == 1 || pos_target == 2 {
                    if let Ok(v) = t.unescape().unwrap_or_default().trim().parse::<f32>() {
                        if let Some(f) = cur_float.as_mut() {
                            if pos_target == 1 {
                                f.off_x = v * EMU_TO_PX;
                            } else {
                                f.off_y = v * EMU_TO_PX;
                            }
                        }
                    }
                } else if in_t {
                    if let Some(r) = cur_run.as_mut() {
                        r.text.push_str(&t.unescape().unwrap_or_default());
                    }
                }
            }
            Ok(Event::End(e)) if e.name().as_ref().ends_with(b"Change") => {
                in_change = in_change.saturating_sub(1);
            }
            Ok(Event::End(e)) if e.name().as_ref() == b"mc:Fallback" => {
                in_fallback = in_fallback.saturating_sub(1);
            }
            Ok(Event::End(_)) if in_change > 0 || in_fallback > 0 => {}
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"w:t" => in_t = false,
                b"w:tabs" => in_tabs = false,
                b"w:tblBorders" | b"w:tcBorders" | b"w:pBdr" => border_ctx = 0,
                b"w:tcPr" => in_tcpr = false,
                b"wp:posOffset" => pos_target = 0,
                b"a:ln" if cur_float.is_some() => in_dln = false,
                b"w:txbxContent" => {
                    in_txbx = false;
                    if let Some((p, r)) = para_stack.pop() {
                        cur_para = p;
                        cur_run = r;
                    }
                }
                b"wpg:grpSpPr" if cur_float.is_some() => {
                    in_grpspr = false;
                    if let Some(f) = cur_float.as_mut() {
                        f.grouped = true;
                    }
                    let gsx = if g_xfrm[6] != 0.0 { g_xfrm[2] / g_xfrm[6] } else { 1.0 };
                    let gsy = if g_xfrm[7] != 0.0 { g_xfrm[3] / g_xfrm[7] } else { 1.0 };
                    let ltx = g_xfrm[0] - g_xfrm[4] * gsx;
                    let lty = g_xfrm[1] - g_xfrm[5] * gsy;
                    let (psx, psy, ptx, pty) = *gstack.last().unwrap();
                    gstack.push((psx * gsx, psy * gsy, psx * ltx + ptx, psy * lty + pty));
                }
                b"wpg:grpSp" | b"wpg:wgp" if cur_float.is_some() => {
                    if gstack.len() > 1 {
                        gstack.pop();
                    }
                }
                b"wps:wsp" | b"wps:cxnSp" => {
                    if let (Some(f), Some(mut s)) = (cur_float.as_mut(), cur_dshape.take()) {
                        let (sx, sy, tx, ty) = *gstack.last().unwrap();
                        s.x = s.x * sx + tx;
                        s.y = s.y * sy + ty;
                        s.w *= sx;
                        s.h *= sy;
                        if s.w <= 0.0 {
                            s.w = f.img_w;
                        }
                        if s.h <= 0.0 {
                            s.h = f.img_h;
                        }
                        f.shapes.push(s);
                    }
                }
                b"wp:anchor" => {
                    if let (Some(f), Some(p)) = (cur_float.take(), cur_para.as_mut()) {
                        p.floats.push(f);
                    }
                }
                b"w:r" => {
                    if let (Some(p), Some(r)) = (cur_para.as_mut(), cur_run.take()) {
                        if !r.text.is_empty() {
                            p.runs.push(r);
                        }
                    }
                }
                b"w:p" => {
                    if let Some(p) = cur_para.take() {
                        if in_txbx {
                            if let Some(s) = cur_dshape.as_mut() {
                                s.blocks.push(Block::Para(p));
                            }
                        } else {
                            push_para(&mut tables, &mut doc.blocks, p);
                        }
                    }
                }
                b"w:tc" => {
                    if let Some(tb) = tables.last_mut() {
                        if let Some(cell) = tb.cur_cell.take() {
                            if let Some(row) = tb.cur_row.as_mut() {
                                row.cells.push(cell);
                            }
                        }
                    }
                }
                b"w:tr" => {
                    if let Some(tb) = tables.last_mut() {
                        if let Some(row) = tb.cur_row.take() {
                            tb.table.rows.push(row);
                        }
                    }
                }
                b"w:tbl" => {
                    if let Some(tb) = tables.pop() {
                        push_block(&mut tables, &mut doc.blocks, Block::Table(tb.table));
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

/// Collect every body paragraph (incl. table cells) in document order.
fn collect_paras_mut<'a>(blocks: &'a mut [Block], out: &mut Vec<&'a mut Para>) {
    for b in blocks.iter_mut() {
        match b {
            Block::Para(p) => out.push(p),
            Block::Table(t) => {
                for row in t.rows.iter_mut() {
                    for cell in row.cells.iter_mut() {
                        collect_paras_mut(&mut cell.blocks, out);
                    }
                }
            }
        }
    }
}

/// Collect paragraphs that live inside floating shapes' text boxes (disjoint from
/// the body paragraphs, so a separate pass avoids aliasing the container).
fn collect_float_paras_mut<'a>(blocks: &'a mut [Block], out: &mut Vec<&'a mut Para>) {
    for b in blocks.iter_mut() {
        match b {
            Block::Para(p) => {
                for fl in p.floats.iter_mut() {
                    for sh in fl.shapes.iter_mut() {
                        collect_paras_mut(&mut sh.blocks, out);
                    }
                }
            }
            Block::Table(t) => {
                for row in t.rows.iter_mut() {
                    for cell in row.cells.iter_mut() {
                        collect_float_paras_mut(&mut cell.blocks, out);
                    }
                }
            }
        }
    }
}

/// Parse a border element (`w:sz` eighths-of-pt, `w:color`, `w:val`).
fn parse_border(e: &BytesStart) -> Border {
    let on = !matches!(get_attr(e, b"w:val").as_deref(), Some("none") | Some("nil") | None);
    let size = get_attr(e, b"w:sz").and_then(|s| s.parse::<f32>().ok()).map(|sz| (sz / 8.0 * 4.0 / 3.0).max(0.75)).unwrap_or(1.0);
    let color = get_attr(e, b"w:color").map(|c| parse_color(&c)).unwrap_or(Color::BLACK);
    Border { on, color, size }
}

/// Route a finished paragraph into the current cell, else the root block list.
fn push_para(tables: &mut [TableBuild], root: &mut Vec<Block>, p: Para) {
    push_block(tables, root, Block::Para(p));
}

fn push_block(tables: &mut [TableBuild], root: &mut Vec<Block>, b: Block) {
    if let Some(tb) = tables.last_mut() {
        if let Some(cell) = tb.cur_cell.as_mut() {
            cell.blocks.push(b);
            return;
        }
    }
    root.push(b);
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
    let mut paras = Vec::new();
    collect_paras_mut(&mut doc.blocks, &mut paras);
    for para in paras {
        let num_id = match para.num_id {
            Some(n) if n != 0 => n,
            // Non-numbered paragraph: still honour its direct left/hanging indent.
            _ => {
                para.indent = para.d_ind_left.unwrap_or(0.0);
                para.hanging = para.d_ind_hanging.unwrap_or(0.0);
                continue;
            }
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

/// A pre-rendered table row band: rects (cell fills + borders) and glyphs, all in
/// coords relative to the line's top-left, at zoom 1.
#[derive(Default)]
struct TableDraw {
    rects: Vec<(f32, f32, f32, f32, Color)>,          // x,y,w,h,color
    glyphs: Vec<(u32, f32, f32, f32, Color, bool)>,   // gid, x, y(baseline), size, color, bold
}

/// A laid-out line at zoom 1. `top` is the cumulative content-y of its top edge.
struct Line {
    placed: Vec<PlacedGlyph>,
    image: Option<ImageBox>,
    table: Option<TableDraw>,
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
    force_break_before: bool, // start a new page before this line
    keep_lines: bool,         // keep with the next line (no break between)
}

/// Line height for a paragraph given the line's max font size. `auto` line
/// spacing (value/240) multiplies the font's natural ~1.2em leading; exact/atLeast
/// act as a minimum so CJK never overlaps.
fn para_line_h(para: &Para, max_size: f32) -> f32 {
    let natural = max_size * 1.2;
    if para.line_exact > 0.0 {
        para.line_exact.max(natural)
    } else if para.line_mult > 0.0 {
        natural * para.line_mult
    } else {
        natural
    }
}

/// Next tab stop (absolute px + alignment) strictly greater than `x`. Uses the
/// paragraph's defined stops, else a default 0.5in left-grid.
fn next_tab_stop(x: f32, margin_l: f32, stops: &[(f32, Align)]) -> (f32, Align) {
    for (pos, al) in stops {
        let abs = margin_l + pos;
        if abs > x + 0.5 {
            return (abs, *al);
        }
    }
    let grid = 48.0; // 0.5 inch
    let rel = (x - margin_l).max(0.0);
    (margin_l + ((rel / grid).floor() + 1.0) * grid, Align::Left)
}

/// Place a wrapped line's items into glyphs, honouring tab stops + alignment
/// (left advances to the stop, centre/right align the following segment to it).
fn place_items(line: &[Item], x_start: f32, margin_l: f32, stops: &[(f32, Align)], extra: f32, out: &mut Vec<PlacedGlyph>) {
    let mut x = x_start;
    let mut i = 0;
    while i < line.len() {
        let it = &line[i];
        if it.kind == IKind::Tab {
            let (pos, al) = next_tab_stop(x, margin_l, stops);
            let mut j = i + 1;
            while j < line.len() && line[j].kind != IKind::Tab {
                j += 1;
            }
            let w: f32 = line[i + 1..j].iter().filter(|t| t.kind == IKind::Glyph).map(|t| t.advance).sum();
            x = match al {
                Align::Left | Align::Justify => pos,
                Align::Center => (pos - w / 2.0).max(x),
                Align::Right => (pos - w).max(x),
            };
            i += 1;
            continue;
        }
        if it.kind == IKind::Glyph {
            out.push(PlacedGlyph {
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
            x += it.advance + if it.break_after { extra } else { 0.0 };
        }
        i += 1;
    }
}

/// Shape + wrap all paragraphs into a flat list of laid-out lines (zoom 1).
fn layout_lines(doc: &mut Document, font: &FontData) -> (Vec<Line>, Vec<Float>) {
    let mut lines = Vec::new();
    let mut floats: Vec<Float> = Vec::new();
    let mut top = 0.0f32;
    let mut pending_break = false; // a preceding w:br page / page_break_after
    let mut pending_reserve = 0.0f32; // vertical space a float reserves after its anchor para
    let (page_w, margin_l, margin_r) = (doc.page_w, doc.margin_l, doc.margin_r);

    for block in &mut doc.blocks {
        top += std::mem::take(&mut pending_reserve);
        let para = match block {
            Block::Para(p) => p,
            Block::Table(t) => {
                top = layout_table_block(t, page_w, margin_l, margin_r, font, &mut lines, top);
                continue;
            }
        };
        let mut consume_break = pending_break || para.page_break_before;
        pending_break = para.page_break_after;
        let kl = para.keep_lines;
        let body_left = margin_l + para.indent;
        let content_w = (page_w - margin_r - para.d_ind_right - body_left).max(32.0);
        let right = page_w - margin_r - para.d_ind_right;
        top += para.spc_before;
        // Floating anchored drawings attached here: position at the paragraph top
        // and pre-lay-out each shape's text-box glyphs (so rendering needs no font).
        for mut fl in std::mem::take(&mut para.floats) {
            fl.body_y = top;
            for sh in &mut fl.shapes {
                if !sh.blocks.is_empty() && sh.w > 0.0 {
                    let (g, h) = layout_cell(&sh.blocks, font, sh.w - 8.0);
                    sh.glyphs = g;
                    sh.text_h = h;
                }
            }
            // Reserve vertical space (wrapTopAndBottom) so body text below the anchor
            // paragraph flows past the float instead of overlapping it.
            if !fl.behind {
                let fh = if fl.shapes.is_empty() {
                    fl.img_h
                } else if fl.grouped {
                    let bot = fl.shapes.iter().map(|s| s.y + s.h).fold(f32::MIN, f32::max);
                    let topy = fl.shapes.iter().map(|s| s.y).fold(f32::MAX, f32::min);
                    (bot - topy).max(0.0)
                } else {
                    fl.shapes.iter().map(|s| s.h).fold(0.0, f32::max)
                };
                pending_reserve = pending_reserve.max(fl.off_y + fh + 8.0);
            }
            floats.push(fl);
        }

        // Image in the paragraph: render it as a block. If the paragraph ALSO has
        // text (e.g. a header logo + title), render the image then fall through to
        // lay out the text below it (rather than dropping the text).
        if let Some(img) = &para.image {
            let mut w = para.image_w.max(1.0);
            let mut h = para.image_h.max(1.0);
            if w > content_w {
                h *= content_w / w;
                w = content_w;
            }
            let has_text = para.runs.iter().any(|r| !r.text.is_empty());
            let space = if has_text { 0.0 } else { para.spc_after.max(max_para_size(para) * 0.3) };
            lines.push(Line {
                placed: Vec::new(),
                image: Some(ImageBox { rgba: img.rgba.clone(), src_w: img.width, src_h: img.height, x: body_left, w, h }),
                table: None,
                top,
                line_h: h,
                advance: h + space,
                ascent: h,
                shd: None,
                bdr_top: None,
                bdr_bottom: None,
                left: body_left,
                right,
                force_break_before: std::mem::take(&mut consume_break),
                keep_lines: kl,
            });
            top += h + space;
            if !has_text {
                continue;
            }
            // else fall through: lay out the paragraph's text below the image
        }

        let bdr_top = if para.pbdr.top { Some((para.pbdr.color, para.pbdr.size)) } else { None };
        let bdr_bottom = if para.pbdr.bottom { Some((para.pbdr.color, para.pbdr.size)) } else { None };

        let items = shape_para(font, para);
        if items.is_empty() {
            let line_h = para_line_h(para, DEFAULT_SIZE_PX);
            lines.push(Line {
                placed: Vec::new(),
                image: None,
                table: None,
                top,
                line_h,
                advance: line_h + para.spc_after,
                ascent: DEFAULT_SIZE_PX * 0.92,
                shd: para.shd,
                bdr_top,
                bdr_bottom,
                left: body_left,
                right,
                force_break_before: std::mem::take(&mut consume_break),
                keep_lines: kl,
            });
            top += line_h + para.spc_after;
            continue;
        }
        let wrapped = wrap(items, content_w);
        let n = wrapped.len();
        for (li, line) in wrapped.iter().enumerate() {
            let max_size = line.iter().map(|i| i.size).fold(DEFAULT_SIZE_PX, f32::max);
            let line_h = para_line_h(para, max_size);
            let lw = line_width(line);
            // first-line indent (only li==0, left/justify, no list marker)
            let fl_ind = if li == 0 && matches!(para.align, Align::Left | Align::Justify) && para.marker.is_none() { para.d_ind_first } else { 0.0 };
            let x = match para.align {
                Align::Left | Align::Justify => body_left + fl_ind,
                Align::Center => body_left + (content_w - lw) / 2.0,
                Align::Right => body_left + (content_w - lw),
            };
            // Justify: distribute the line's slack across its break opportunities
            // (every non-last line of a justified paragraph that has a tab is left alone).
            let has_tab = line.iter().any(|i| i.kind == IKind::Tab);
            let extra = if para.align == Align::Justify && li + 1 != n && !has_tab && content_w > lw {
                let nb = line.iter().filter(|i| i.kind == IKind::Glyph && i.break_after).count().saturating_sub(1);
                if nb > 0 { (content_w - lw - fl_ind) / nb as f32 } else { 0.0 }
            } else {
                0.0
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

            place_items(line, x, margin_l, &para.tab_stops, extra, &mut placed);
            let advance = line_h + if li + 1 == n { para.spc_after } else { 0.0 };
            lines.push(Line {
                placed,
                image: None,
                table: None,
                top,
                line_h,
                advance,
                ascent: max_size * 0.92,
                shd: para.shd,
                bdr_top: if li == 0 { bdr_top } else { None },
                bdr_bottom: if li + 1 == n { bdr_bottom } else { None },
                left: body_left,
                right,
                force_break_before: if li == 0 { std::mem::take(&mut consume_break) } else { false },
                keep_lines: kl && li + 1 < n,
            });
            top += advance;
        }
    }
    (lines, floats)
}

/// Lay out a block list's paragraphs into glyphs (relative to the content origin)
/// and return them plus the content height. Used for table cells + text boxes.
fn layout_cell(blocks: &[Block], font: &FontData, width: f32) -> (Vec<(u32, f32, f32, f32, Color, bool)>, f32) {
    let mut glyphs = Vec::new();
    let mut y = 0.0f32;
    let w = width.max(8.0);
    for block in blocks {
        let para = match block {
            Block::Para(p) => p,
            Block::Table(_) => continue, // nested tables in cells not laid out (rare)
        };
        y += para.spc_before;
        let items = shape_para(font, para);
        if items.is_empty() {
            y += DEFAULT_SIZE_PX * 1.2 + para.spc_after;
            continue;
        }
        let wrapped = wrap(items, w);
        let n = wrapped.len();
        for (li, line) in wrapped.iter().enumerate() {
            let max_size = line.iter().map(|i| i.size).fold(DEFAULT_SIZE_PX, f32::max);
            let line_h = para_line_h(para, max_size);
            let lw = line_width(line);
            let mut x = match para.align {
                Align::Left | Align::Justify => 0.0,
                Align::Center => (w - lw) / 2.0,
                Align::Right => w - lw,
            };
            let baseline = y + max_size * 0.85;
            for it in line {
                if it.kind == IKind::Tab {
                    x = ((x / 48.0).floor() + 1.0) * 48.0;
                    continue;
                }
                if it.kind == IKind::Break {
                    continue;
                }
                glyphs.push((it.gid, x + it.x_off, baseline, it.size, it.color, it.bold));
                x += it.advance;
            }
            y += line_h + if li + 1 == n { para.spc_after } else { 0.0 };
        }
    }
    (glyphs, y)
}

/// Lay out a table as a sequence of row-band `Line`s (row-atomic for pagination).
#[allow(clippy::too_many_arguments)]
fn layout_table_block(t: &Table, page_w: f32, margin_l: f32, margin_r: f32, font: &FontData, lines: &mut Vec<Line>, mut top: f32) -> f32 {
    let content_w = (page_w - margin_l - margin_r).max(32.0);

    // Column widths: from tblGrid, scaled to fit content width.
    let mut cols = t.grid.clone();
    if cols.is_empty() {
        let n = t.rows.first().map(|r| r.cells.iter().map(|c| c.grid_span as usize).sum::<usize>()).unwrap_or(1).max(1);
        cols = vec![content_w / n as f32; n];
    }
    let total: f32 = cols.iter().sum();
    if total > content_w + 0.5 && total > 0.0 {
        let s = content_w / total;
        for c in &mut cols {
            *c *= s;
        }
    }
    let ncols = cols.len();
    let mut col_x = vec![0.0f32; ncols + 1];
    for i in 0..ncols {
        col_x[i + 1] = col_x[i] + cols[i];
    }
    let table_left = margin_l + t.ind;
    let (ml, mr) = (t.cell_mar_l, t.cell_mar_r);
    let (mt, mb) = (t.cell_mar_t.max(1.5), t.cell_mar_b.max(1.5));
    let n_rows = t.rows.len();

    let pick = |cb: Border, outer: Border, inner: Border, is_outer: bool| -> Border {
        if cb.on {
            cb
        } else if is_outer {
            outer
        } else {
            inner
        }
    };

    // Per-row map: column-start -> is this cell a vMerge continue? (for spanning)
    let mut col_cont: Vec<HashMap<usize, bool>> = Vec::with_capacity(n_rows);
    for row in &t.rows {
        let mut ci = 0usize;
        let mut m = HashMap::new();
        for cell in &row.cells {
            if ci >= ncols {
                break;
            }
            let span = (cell.grid_span as usize).max(1).min(ncols - ci);
            m.insert(ci, cell.vmerge == VMerge::Continue);
            ci += span;
        }
        col_cont.push(m);
    }

    for (ri, row) in t.rows.iter().enumerate() {
        // Lay out each cell; track its column span + glyphs + content height.
        let mut ci = 0usize;
        let mut cell_lay: Vec<(usize, usize, Vec<(u32, f32, f32, f32, Color, bool)>, f32, &Cell)> = Vec::new();
        let mut row_content_h = 0.0f32;
        for cell in &row.cells {
            if ci >= ncols {
                break;
            }
            let span = (cell.grid_span as usize).max(1).min(ncols - ci);
            let c0 = ci;
            let c1 = ci + span;
            ci = c1;
            let cell_w = (col_x[c1] - col_x[c0]).max(4.0);
            let inner_w = (cell_w - ml - mr).max(4.0);
            let (glyphs, h) = if cell.vmerge == VMerge::Continue {
                (Vec::new(), 0.0)
            } else {
                layout_cell(&cell.blocks, font, inner_w)
            };
            row_content_h = row_content_h.max(h);
            cell_lay.push((c0, c1, glyphs, h, cell));
        }
        let row_h = (row_content_h + mt + mb).max(row.min_h).max(DEFAULT_SIZE_PX);

        // Build the row band's draw list.
        let mut td = TableDraw::default();
        for (c0, c1, glyphs, content_h, cell) in &cell_lay {
            let x = table_left + col_x[*c0];
            let cell_w = col_x[*c1] - col_x[*c0];
            // shading
            if let Some(c) = cell.shd {
                td.rects.push((x, 0.0, cell_w, row_h, c));
            }
            // borders (cell override else table outer/inner)
            let is_continue = cell.vmerge == VMerge::Continue;
            let bt = pick(cell.borders.top, t.borders.top, t.borders.inside_h, ri == 0);
            let bb = pick(cell.borders.bottom, t.borders.bottom, t.borders.inside_h, ri + 1 == n_rows);
            let bl = pick(cell.borders.left, t.borders.left, t.borders.inside_v, *c0 == 0);
            let br = pick(cell.borders.right, t.borders.right, t.borders.inside_v, *c1 == ncols);
            // Suppress the shared horizontal edge between a vMerge restart/continue
            // and the continue cell directly below it (so a merged cell reads as one).
            let spans_down = ri + 1 < n_rows && col_cont.get(ri + 1).and_then(|m| m.get(c0)).copied() == Some(true);
            if bt.on && !is_continue {
                td.rects.push((x, 0.0, cell_w, bt.size, bt.color));
            }
            if bb.on && !spans_down {
                td.rects.push((x, row_h - bb.size, cell_w, bb.size, bb.color));
            }
            if bl.on {
                td.rects.push((x, 0.0, bl.size, row_h, bl.color));
            }
            if br.on {
                td.rects.push((x + cell_w - br.size, 0.0, br.size, row_h, br.color));
            }
            // glyphs: offset to cell content origin + vertical alignment
            let vshift = match cell.valign {
                1 => ((row_h - mt - mb) - content_h).max(0.0) / 2.0,
                2 => ((row_h - mt - mb) - content_h).max(0.0),
                _ => 0.0,
            };
            for (gid, gx, gy, sz, col, bold) in glyphs {
                td.glyphs.push((*gid, x + ml + gx, mt + vshift + gy, *sz, *col, *bold));
            }
        }

        lines.push(Line {
            placed: Vec::new(),
            image: None,
            table: Some(td),
            top,
            line_h: row_h,
            advance: row_h,
            ascent: row_h,
            shd: None,
            bdr_top: None,
            bdr_bottom: None,
            left: table_left,
            right: table_left + col_x[ncols],
            force_break_before: false,
            keep_lines: !row.is_header && false,
        });
        top += row_h;
    }
    top + 4.0 // small gap after the table
}

/// A laid-out header or footer part.
struct HdrFtr {
    lines: Vec<Line>,
    floats: Vec<Float>,
    height: f32,
}

/// Parse + lay out a header/footer part (`word/headerN.xml`).
fn build_hdrftr(bytes: &[u8], part: &str, font: &FontData, table: &StyleTable, nb: &Numbering, page_w: f32, ml: f32, mr: f32) -> Option<HdrFtr> {
    let xml = read_zip_entry(bytes, part)?;
    let mut doc = parse_document(&xml);
    doc.page_w = page_w;
    doc.margin_l = ml;
    doc.margin_r = mr;
    // Headers/footers have implicit centre + right tab stops at the content midpoint
    // and right edge (used to centre / right-align running text against a tab).
    let content_w = (page_w - ml - mr).max(32.0);
    for para in &mut doc.blocks {
        if let Block::Para(p) = para {
            if p.tab_stops.is_empty() {
                p.tab_stops.push((content_w / 2.0, Align::Center));
                p.tab_stops.push((content_w, Align::Right));
            }
        }
    }
    resolve_document(&mut doc, table);
    resolve_numbering(&mut doc, nb);
    let rels = part.rsplit_once('/').map(|(d, f)| format!("{}/_rels/{}.rels", d, f)).unwrap_or_default();
    resolve_images(&mut doc, bytes, &rels);
    let (lines, floats) = layout_lines(&mut doc, font);
    let line_h = lines.last().map(|l| l.top + l.line_h).unwrap_or(0.0);
    let float_h = floats.iter().map(|f| f.off_y + f.img_h).fold(0.0, f32::max);
    let height = line_h.max(float_h);
    Some(HdrFtr { lines, floats, height })
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

    let top_y = baseline - line.ascent * scale;

    // Table row band: pre-computed rects (fills + borders) then cell glyphs.
    if let Some(td) = &line.table {
        for (x, y, w, h, c) in &td.rects {
            dl.push(fill_box(x * scale, top_y + y * scale, w * scale, h * scale, *c));
        }
        let mut i = 0;
        while i < td.glyphs.len() {
            let (_, _, _, sz, col, bold) = td.glyphs[i];
            let mut glyphs = Vec::new();
            while i < td.glyphs.len() && td.glyphs[i].3 == sz && td.glyphs[i].4 == col && td.glyphs[i].5 == bold {
                let (gid, gx, gy, ..) = td.glyphs[i];
                glyphs.push(PositionedGlyph { id: gid, x: gx * scale, y: top_y + gy * scale });
                i += 1;
            }
            dl.push(Command::Glyphs(GlyphRun { font: FontId(0), size: sz * scale, paint: Paint::Solid(col), bold, glyphs }));
        }
        return;
    }

    // Paragraph shading + borders span the full body width.
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

/// Render a floating anchored drawing. `dy` maps the float's body-y to the page
/// (body_top - page_top); all coords are scaled to device px.
fn render_float(dl: &mut DisplayList, fl: &Float, margin_l: f32, dy: f32, scale: f32) {
    let fx = margin_l + fl.off_x;
    let fy = dy + fl.body_y + fl.off_y;
    if let Some(img) = &fl.image {
        dl.push(Command::Image {
            rgba: img.rgba.clone(),
            src_w: img.width,
            src_h: img.height,
            x: fx * scale,
            y: fy * scale,
            w: fl.img_w.max(1.0) * scale,
            h: fl.img_h.max(1.0) * scale,
            clip: None,
        });
    }
    // Grouped shapes carry group-canvas coords; re-anchor the WHOLE group (both
    // axes) so its top-left corner lands at (fx, fy) — the same origin as the
    // group's background image — otherwise the callouts shift off the picture.
    let (gmin_x, gmin_y) = if fl.grouped {
        (
            fl.shapes.iter().map(|s| s.x).fold(f32::MAX, f32::min),
            fl.shapes.iter().map(|s| s.y).fold(f32::MAX, f32::min),
        )
    } else {
        (0.0, 0.0)
    };
    for sh in &fl.shapes {
        let (sx, sy) = if fl.grouped { (sh.x - gmin_x + fx, sh.y - gmin_y + fy) } else { (fx, fy) };
        if sh.is_line {
            if let Some((c, w)) = sh.outline {
                // A connector's bbox is the rectangle it spans. A mostly-vertical or
                // mostly-horizontal connector is drawn as a straight axis line (Word
                // routes these orthogonally); only a roughly-square bbox is a diagonal.
                let (mut x0, mut y0, mut x1, mut y1);
                if sh.h > sh.w * 2.0 {
                    let mx = sx + sh.w / 2.0;
                    x0 = mx; x1 = mx; y0 = sy; y1 = sy + sh.h;
                } else if sh.w > sh.h * 2.0 {
                    let my = sy + sh.h / 2.0;
                    x0 = sx; x1 = sx + sh.w; y0 = my; y1 = my;
                } else {
                    x0 = sx; x1 = sx + sh.w; y0 = sy; y1 = sy + sh.h;
                    if sh.flip_h { std::mem::swap(&mut x0, &mut x1); }
                    if sh.flip_v { std::mem::swap(&mut y0, &mut y1); }
                }
                let mut p = dv_ir::PathData::new();
                p.move_to(x0 * scale, y0 * scale);
                p.line_to(x1 * scale, y1 * scale);
                dl.push(Command::StrokePath { path: p, paint: Paint::Solid(c), width: (w * scale).max(1.0), transform: dv_ir::Transform::IDENTITY });
            }
            continue;
        }
        if sh.w <= 0.0 || sh.h <= 0.0 {
            continue;
        }
        if let Some(c) = sh.fill {
            dl.push(fill_box(sx * scale, sy * scale, sh.w * scale, sh.h * scale, c));
        }
        if let Some((c, w)) = sh.outline {
            let t = (w * scale).max(1.0);
            dl.push(fill_box(sx * scale, sy * scale, sh.w * scale, t, c));
            dl.push(fill_box(sx * scale, (sy + sh.h) * scale - t, sh.w * scale, t, c));
            dl.push(fill_box(sx * scale, sy * scale, t, sh.h * scale, c));
            dl.push(fill_box((sx + sh.w) * scale - t, sy * scale, t, sh.h * scale, c));
        }
        // text box content, vertically centred, with a small inset
        let pad = 4.0;
        let vshift = ((sh.h - 2.0 * pad - sh.text_h).max(0.0)) / 2.0;
        let (gx0, gy0) = (sx + pad, sy + pad + vshift);
        let mut i = 0;
        while i < sh.glyphs.len() {
            let (_, _, _, size, color, bold) = sh.glyphs[i];
            let mut run = Vec::new();
            while i < sh.glyphs.len() && sh.glyphs[i].3 == size && sh.glyphs[i].4 == color && sh.glyphs[i].5 == bold {
                let (gid, gx, gy, ..) = sh.glyphs[i];
                run.push(PositionedGlyph { id: gid, x: (gx0 + gx) * scale, y: (gy0 + gy) * scale });
                i += 1;
            }
            dl.push(Command::Glyphs(GlyphRun { font: FontId(0), size: size * scale, paint: Paint::Solid(color), bold, glyphs: run }));
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
    resolve_images(&mut doc, bytes, "word/_rels/document.xml.rels");
    let (lines, floats) = layout_lines(&mut doc, font);
    let total_h = doc.margin_t + lines.last().map(|l| l.top + l.advance).unwrap_or(0.0) + doc.margin_b;
    let mut dl = DisplayList::new(doc.page_w, total_h);
    for line in &lines {
        emit_line(&mut dl, line, doc.margin_t + line.top + line.ascent, 1.0);
    }
    for fl in &floats {
        render_float(&mut dl, fl, doc.margin_l, doc.margin_t, 1.0);
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
    body_top: f32,
    header_dist: f32,
    footer_dist: f32,
    title_pg: bool,
    hdr_first: Option<HdrFtr>,
    hdr_default: Option<HdrFtr>,
    ftr_first: Option<HdrFtr>,
    ftr_default: Option<HdrFtr>,
    floats: Vec<Float>,
    margin_l: f32,
}

impl DocxDoc {
    pub fn parse(bytes: &[u8], font: &FontData) -> DocxDoc {
        let mut doc = read_zip_entry(bytes, "word/document.xml").map(|x| parse_document(&x)).unwrap_or_else(|| Document {
            blocks: Vec::new(),
            page_w: 816.0,
            page_h: 1056.0,
            margin_l: 96.0,
            margin_r: 96.0,
            margin_t: 96.0,
            margin_b: 96.0,
            header_dist: 48.0,
            footer_dist: 48.0,
            hdr_refs: Vec::new(),
            ftr_refs: Vec::new(),
            title_pg: false,
        });
        let table = read_zip_entry(bytes, "word/styles.xml").map(|s| parse_styles_xml(&s)).unwrap_or_default();
        resolve_document(&mut doc, &table);
        let numbering = read_zip_entry(bytes, "word/numbering.xml").map(|s| parse_numbering_xml(&s)).unwrap_or_default();
        resolve_numbering(&mut doc, &numbering);
        resolve_images(&mut doc, bytes, "word/_rels/document.xml.rels");
        let (lines, floats) = layout_lines(&mut doc, font);

        // Resolve + lay out header/footer parts (by reference type).
        let doc_rels = read_zip_entry(bytes, "word/_rels/document.xml.rels").map(|s| rels_map(&s)).unwrap_or_default();
        let part = |refs: &[(String, String)], ty: &str| -> Option<HdrFtr> {
            let id = refs.iter().find(|(t, _)| t == ty).map(|(_, id)| id)?;
            let target = doc_rels.get(id)?;
            let path = resolve_rel("word", target);
            build_hdrftr(bytes, &path, font, &table, &numbering, doc.page_w, doc.margin_l, doc.margin_r)
        };
        let hdr_first = part(&doc.hdr_refs, "first");
        let hdr_default = part(&doc.hdr_refs, "default");
        let ftr_first = part(&doc.ftr_refs, "first");
        let ftr_default = part(&doc.ftr_refs, "default");

        // Reserve space so the body never overlaps a tall running header/footer.
        let hdr_h = [&hdr_first, &hdr_default].iter().filter_map(|h| h.as_ref().map(|x| x.height)).fold(0.0f32, f32::max);
        let ftr_h = [&ftr_first, &ftr_default].iter().filter_map(|f| f.as_ref().map(|x| x.height)).fold(0.0f32, f32::max);
        let body_top = doc.margin_t.max(doc.header_dist + hdr_h + 8.0);
        let body_bottom = doc.margin_b.max(doc.footer_dist + ftr_h + 8.0);
        let cap = (doc.page_h - body_top - body_bottom).max(32.0);

        let mut pages = Vec::new();
        let mut start = 0;
        let mut used = 0.0f32;
        let mut page_top = 0.0f32;
        for (i, line) in lines.iter().enumerate() {
            // Force a new page on an explicit break, else when the line overflows.
            if used > 0.0 && (line.force_break_before || used + line.line_h > cap) {
                // keep-with-next: pull a kept group (bounded) onto the next page.
                let mut bp = i;
                if !line.force_break_before {
                    let mut steps = 0;
                    while bp > start && lines[bp - 1].keep_lines && steps < 12 {
                        bp -= 1;
                        steps += 1;
                    }
                    if bp == start {
                        bp = i; // don't create an empty page
                    }
                }
                pages.push(Page { start, end: bp, top_y: page_top });
                start = bp;
                page_top = lines[bp].top;
                used = lines[bp..i].iter().map(|l| l.advance).sum();
            }
            used += line.advance;
        }
        pages.push(Page { start, end: lines.len(), top_y: page_top });

        DocxDoc {
            lines,
            pages,
            page_w: doc.page_w,
            page_h: doc.page_h,
            body_top,
            header_dist: doc.header_dist,
            footer_dist: doc.footer_dist,
            title_pg: doc.title_pg,
            hdr_first,
            hdr_default,
            ftr_first,
            ftr_default,
            floats,
            margin_l: doc.margin_l,
        }
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

        // Header (first page uses the "first" header when titlePg is set).
        let first = idx == 0 && self.title_pg;
        let hdr = if first { self.hdr_first.as_ref().or(self.hdr_default.as_ref()) } else { self.hdr_default.as_ref() };
        if let Some(h) = hdr {
            for fl in &h.floats {
                render_float(&mut dl, fl, self.margin_l, self.header_dist, scale);
            }
            for line in &h.lines {
                emit_line(&mut dl, line, (self.header_dist + line.top + line.ascent) * scale, scale);
            }
        }
        let ftr = if first { self.ftr_first.as_ref().or(self.ftr_default.as_ref()) } else { self.ftr_default.as_ref() };
        if let Some(f) = ftr {
            let foot_top = (self.page_h - self.footer_dist - f.height).max(self.page_h * 0.85);
            for fl in &f.floats {
                render_float(&mut dl, fl, self.margin_l, foot_top, scale);
            }
            for line in &f.lines {
                emit_line(&mut dl, line, (foot_top + line.top + line.ascent) * scale, scale);
            }
        }

        for li in page.start..page.end {
            let line = &self.lines[li];
            let local_top = self.body_top + (line.top - page.top_y);
            emit_line(&mut dl, line, (local_top + line.ascent) * scale, scale);
        }

        // Floating drawings anchored on this page.
        let next_top = self.pages.get(idx + 1).map(|p| p.top_y).unwrap_or(f32::INFINITY);
        let dy = self.body_top - page.top_y;
        for fl in &self.floats {
            if fl.body_y >= page.top_y - 0.5 && fl.body_y < next_top {
                render_float(&mut dl, fl, self.margin_l, dy, scale);
            }
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
fn resolve_images(doc: &mut Document, bytes: &[u8], rels_name: &str) {
    let rels = match read_zip_entry(bytes, rels_name) {
        Some(s) => rels_map(&s),
        None => return,
    };
    let decode = |rid: &str| -> Option<dv_image::DecodedImage> {
        let target = rels.get(rid)?;
        let path = resolve_rel("word", target);
        let img = read_zip_bytes(bytes, &path).and_then(|b| dv_image::decode(&b))?;
        // 1x1 (and other degenerate) images are transparent spacers/placeholders;
        // stretched to the drawing extent they become spurious colour blocks.
        if img.width <= 2 || img.height <= 2 {
            return None;
        }
        Some(img)
    };
    let mut paras = Vec::new();
    collect_paras_mut(&mut doc.blocks, &mut paras);
    for para in paras {
        if let Some(rid) = para.image_rid.clone() {
            if let Some(img) = decode(&rid) {
                if para.image_w <= 0.0 {
                    para.image_w = img.width as f32;
                }
                if para.image_h <= 0.0 {
                    para.image_h = img.height as f32;
                }
                para.image = Some(img);
            }
        }
        // anchored images inside floats (e.g. a header/footer logo)
        for fl in &mut para.floats {
            if let Some(rid) = fl.image_rid.clone() {
                if let Some(img) = decode(&rid) {
                    if fl.img_w <= 0.0 {
                        fl.img_w = img.width as f32;
                    }
                    if fl.img_h <= 0.0 {
                        fl.img_h = img.height as f32;
                    }
                    fl.image = Some(img);
                }
            }
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
                        Some("both") | Some("distribute") => Align::Justify,
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
fn resolve_one_para(para: &mut Para, table: &StyleTable) {
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

fn resolve_document(doc: &mut Document, table: &StyleTable) {
    // Body paragraphs, then (separate pass to avoid aliasing) text-box paragraphs.
    let mut paras = Vec::new();
    collect_paras_mut(&mut doc.blocks, &mut paras);
    for para in paras {
        resolve_one_para(para, table);
    }
    let mut fpars = Vec::new();
    collect_float_paras_mut(&mut doc.blocks, &mut fpars);
    for para in fpars {
        resolve_one_para(para, table);
    }
}
