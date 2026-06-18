//! Self-written PPTX slide renderer.
//!
//! Parses `ppt/presentation.xml` (slide size + order) and each slide's
//! `p:sp` shapes — DrawingML position/size (`a:off`/`a:ext`, EMU), solid fills,
//! and text bodies (paragraphs, runs, run size/bold/colour, alignment) — and
//! lays each text box out (with CJK+Latin wrapping) into the shared
//! [`dv_ir::DisplayList`].
//!
//! Scope (M5.1): positioned text boxes + solid-fill rectangles + run formatting
//! + paragraph alignment. Not yet: preset shape geometry, images, theme/master
//! inheritance, tables, charts, gradients — these are the long tail of PPTX
//! fidelity for any non-Microsoft renderer.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use dv_ir::{Color, Command, DisplayList, FillRule, FontId, GlyphRun, Paint, PathData, PositionedGlyph, Transform};
use dv_text::{shape, FontData};
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use zip::ZipArchive;

type Zip = ZipArchive<Cursor<Vec<u8>>>;
const EMU_PER_PX: f32 = 9525.0; // 914400 EMU/in ÷ 96 px/in

#[derive(Clone, Copy, PartialEq)]
enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone)]
struct Run {
    text: String,
    size: f32, // px (at zoom 1)
    bold: bool,
    color: Color,
}

struct Para {
    runs: Vec<Run>,
    align: Align,
}

struct Shape {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    fill: Option<Color>,
    paras: Vec<Para>,
    image: Option<dv_image::DecodedImage>,
}

/// A parsed presentation, ready for repeated slide renders.
pub struct Deck {
    slides: Vec<Vec<Shape>>,
    width: f32,
    height: f32,
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

fn read_bytes(zip: &mut Zip, name: &str) -> Option<Vec<u8>> {
    let mut f = zip.by_name(name).ok()?;
    let mut v = Vec::new();
    f.read_to_end(&mut v).ok()?;
    Some(v)
}

/// Resolve a relationship `target` against a part's directory (handles `..`/`/`).
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

fn parse_color(s: &str) -> Color {
    if s.len() != 6 {
        return Color::BLACK;
    }
    Color::rgb(
        u8::from_str_radix(&s[0..2], 16).unwrap_or(0),
        u8::from_str_radix(&s[2..4], 16).unwrap_or(0),
        u8::from_str_radix(&s[4..6], 16).unwrap_or(0),
    )
}

fn is_cjk(ch: char) -> bool {
    let c = ch as u32;
    (0x2E80..=0x9FFF).contains(&c) || (0xAC00..=0xD7A3).contains(&c) || (0xF900..=0xFAFF).contains(&c) || (0xFF00..=0xFFEF).contains(&c)
}

impl Deck {
    pub fn parse(bytes: &[u8]) -> Deck {
        let mut deck = Deck { slides: Vec::new(), width: 960.0, height: 720.0 };
        let mut zip = match ZipArchive::new(Cursor::new(bytes.to_vec())) {
            Ok(z) => z,
            Err(_) => return deck,
        };
        let pres = match read_entry(&mut zip, "ppt/presentation.xml") {
            Some(s) => s,
            None => return deck,
        };
        let (w, h, rids) = parse_presentation(&pres);
        deck.width = w;
        deck.height = h;
        let rels = read_entry(&mut zip, "ppt/_rels/presentation.xml.rels").map(|s| rels_map(&s)).unwrap_or_default();
        for rid in rids {
            if let Some(target) = rels.get(&rid).cloned() {
                let path = resolve_rel("ppt", &target);
                let (dir, file) = match path.rsplit_once('/') {
                    Some((d, f)) => (d.to_string(), f.to_string()),
                    None => (String::new(), path.clone()),
                };
                let Some(xml) = read_entry(&mut zip, &path) else { continue };
                let rels_path = format!("{}/_rels/{}.rels", dir, file);
                let slide_rels = read_entry(&mut zip, &rels_path).map(|s| rels_map(&s)).unwrap_or_default();
                deck.slides.push(parse_slide(&xml, &slide_rels, &dir, &mut zip));
            }
        }
        deck
    }

    pub fn slide_count(&self) -> usize {
        self.slides.len()
    }
    pub fn width(&self) -> f32 {
        self.width
    }
    pub fn height(&self) -> f32 {
        self.height
    }

    /// Render slide `idx` at `scale` (= zoom × dpr) into a device-px display list.
    pub fn render_slide(&self, idx: usize, font: &FontData, scale: f32) -> DisplayList {
        let mut dl = DisplayList::new((self.width * scale).max(1.0), (self.height * scale).max(1.0));
        let Some(shapes) = self.slides.get(idx) else { return dl };

        for sh in shapes {
            if let Some(fill) = sh.fill {
                dl.push(fill_rect(sh.x * scale, sh.y * scale, sh.w * scale, sh.h * scale, fill));
            }
            if let Some(img) = &sh.image {
                dl.push(Command::Image {
                    rgba: img.rgba.clone(),
                    src_w: img.width,
                    src_h: img.height,
                    x: sh.x * scale,
                    y: sh.y * scale,
                    w: sh.w * scale,
                    h: sh.h * scale,
                });
            }
        }
        for sh in shapes {
            if sh.paras.is_empty() {
                continue;
            }
            self.layout_shape_text(&mut dl, font, sh, scale);
        }
        dl
    }

    fn layout_shape_text(&self, dl: &mut DisplayList, font: &FontData, sh: &Shape, scale: f32) {
        let pad = 7.0 * scale;
        let left = sh.x * scale + pad;
        let content_w = (sh.w * scale - 2.0 * pad).max(8.0);
        let mut y = sh.y * scale + pad;

        for para in &sh.paras {
            let items = shape_para(font, para, scale);
            if items.is_empty() {
                y += 18.0 * scale;
                continue;
            }
            for line in wrap(items, content_w) {
                let max_size = line.iter().map(|i| i.size).fold(8.0 * scale, f32::max);
                let line_h = max_size * 1.3;
                let baseline = y + max_size * 0.88;
                let lw = line_width(&line);
                let x_start = match para.align {
                    Align::Left => left,
                    Align::Center => left + (content_w - lw) / 2.0,
                    Align::Right => left + (content_w - lw),
                };
                let mut x = x_start;
                let mut i = 0;
                while i < line.len() {
                    let (size, color, bold) = (line[i].size, line[i].color, line[i].bold);
                    let mut glyphs = Vec::new();
                    while i < line.len() && line[i].size == size && line[i].color == color && line[i].bold == bold {
                        glyphs.push(PositionedGlyph { id: line[i].gid, x: x + line[i].x_off, y: baseline });
                        x += line[i].advance;
                        i += 1;
                    }
                    dl.push(Command::Glyphs(GlyphRun { font: FontId(0), size, paint: Paint::Solid(color), bold, glyphs }));
                }
                y += line_h;
            }
        }
    }
}

fn parse_presentation(xml: &str) -> (f32, f32, Vec<String>) {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let (mut w, mut h) = (960.0, 720.0);
    let mut rids = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match e.name().as_ref() {
                b"p:sldSz" => {
                    if let Some(cx) = get_attr(&e, b"cx").and_then(|s| s.parse::<f32>().ok()) {
                        w = cx / EMU_PER_PX;
                    }
                    if let Some(cy) = get_attr(&e, b"cy").and_then(|s| s.parse::<f32>().ok()) {
                        h = cy / EMU_PER_PX;
                    }
                }
                b"p:sldId" => {
                    if let Some(rid) = get_attr(&e, b"r:id") {
                        rids.push(rid);
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    (w, h, rids)
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

fn parse_slide(xml: &str, slide_rels: &HashMap<String, String>, slide_dir: &str, zip: &mut Zip) -> Vec<Shape> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut shapes = Vec::new();

    let mut cur: Option<Shape> = None;
    let mut in_sppr = false;
    let mut in_rpr = false;
    let mut cur_para: Option<Para> = None;
    let mut cur_align = Align::Left;
    let mut cur_run: Option<Run> = None;
    let mut in_t = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match e.name().as_ref() {
                b"p:sp" | b"p:pic" => cur = Some(Shape { x: 0.0, y: 0.0, w: 0.0, h: 0.0, fill: None, paras: Vec::new(), image: None }),
                b"p:spPr" => in_sppr = true,
                b"a:blip" => {
                    if let (Some(s), Some(rid)) = (cur.as_mut(), get_attr(&e, b"r:embed")) {
                        if let Some(target) = slide_rels.get(&rid) {
                            let path = resolve_rel(slide_dir, target);
                            if let Some(img) = read_bytes(zip, &path).and_then(|b| dv_image::decode(&b)) {
                                s.image = Some(img);
                            }
                        }
                    }
                }
                b"a:off" => {
                    if let Some(s) = cur.as_mut() {
                        if let Some(x) = get_attr(&e, b"x").and_then(|v| v.parse::<f32>().ok()) {
                            s.x = x / EMU_PER_PX;
                        }
                        if let Some(y) = get_attr(&e, b"y").and_then(|v| v.parse::<f32>().ok()) {
                            s.y = y / EMU_PER_PX;
                        }
                    }
                }
                b"a:ext" => {
                    if let Some(s) = cur.as_mut() {
                        if let Some(cx) = get_attr(&e, b"cx").and_then(|v| v.parse::<f32>().ok()) {
                            s.w = cx / EMU_PER_PX;
                        }
                        if let Some(cy) = get_attr(&e, b"cy").and_then(|v| v.parse::<f32>().ok()) {
                            s.h = cy / EMU_PER_PX;
                        }
                    }
                }
                b"a:srgbClr" => {
                    let col = get_attr(&e, b"val").map(|v| parse_color(&v));
                    if let Some(col) = col {
                        if in_rpr {
                            if let Some(r) = cur_run.as_mut() {
                                r.color = col;
                            }
                        } else if in_sppr {
                            if let Some(s) = cur.as_mut() {
                                s.fill = Some(col);
                            }
                        }
                    }
                }
                b"a:p" => {
                    cur_align = Align::Left;
                    cur_para = Some(Para { runs: Vec::new(), align: Align::Left });
                }
                b"a:pPr" => {
                    cur_align = match get_attr(&e, b"algn").as_deref() {
                        Some("ctr") => Align::Center,
                        Some("r") => Align::Right,
                        _ => Align::Left,
                    };
                }
                b"a:r" => cur_run = Some(Run { text: String::new(), size: 24.0, bold: false, color: Color::BLACK }),
                b"a:rPr" => {
                    in_rpr = true;
                    if let Some(r) = cur_run.as_mut() {
                        if let Some(sz) = get_attr(&e, b"sz").and_then(|v| v.parse::<f32>().ok()) {
                            r.size = sz / 75.0; // hundredths-of-point -> px
                        }
                        r.bold = get_attr(&e, b"b").as_deref() == Some("1");
                    }
                }
                b"a:t" => in_t = true,
                _ => {}
            },
            Ok(Event::Text(t)) => {
                if in_t {
                    if let Some(r) = cur_run.as_mut() {
                        r.text.push_str(&t.unescape().unwrap_or_default());
                    }
                }
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"p:spPr" => in_sppr = false,
                b"a:rPr" => in_rpr = false,
                b"a:t" => in_t = false,
                b"a:r" => {
                    if let (Some(p), Some(r)) = (cur_para.as_mut(), cur_run.take()) {
                        if !r.text.is_empty() {
                            p.runs.push(r);
                        }
                    }
                }
                b"a:p" => {
                    if let (Some(s), Some(mut p)) = (cur.as_mut(), cur_para.take()) {
                        p.align = cur_align;
                        s.paras.push(p);
                    }
                }
                b"p:sp" | b"p:pic" => {
                    if let Some(s) = cur.take() {
                        shapes.push(s);
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    shapes
}

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

fn shape_para(font: &FontData, para: &Para, scale: f32) -> Vec<Item> {
    let mut items = Vec::new();
    for run in &para.runs {
        let px = run.size * scale;
        let shaped = shape(font, &run.text, px);
        let s = px / shaped.units_per_em.max(1.0);
        for g in &shaped.glyphs {
            let ch = run.text.get(g.cluster as usize..).and_then(|x| x.chars().next()).unwrap_or(' ');
            let is_space = ch.is_whitespace();
            items.push(Item {
                gid: g.glyph_id,
                advance: g.x_advance * s,
                x_off: g.x_offset * s,
                size: px,
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
    let mut lines = Vec::new();
    let mut cur: Vec<Item> = Vec::new();
    let mut cur_w = 0.0f32;
    for it in items {
        if !cur.is_empty() && cur_w + it.advance > content_w {
            if let Some(bi) = cur.iter().rposition(|i| i.break_after) {
                let rem = cur.split_off(bi + 1);
                lines.push(std::mem::take(&mut cur));
                cur = rem;
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
    let end = line.iter().rposition(|i| !i.is_space).map(|i| i + 1).unwrap_or(0);
    line[..end].iter().map(|i| i.advance).sum()
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
