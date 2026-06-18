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

use dv_ir::{Color, Command, DisplayList, FillRule, FontId, GlyphRun, Paint, PathData, PathVerb, PositionedGlyph, Transform};
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
    underline: bool,
    color: Color,
    highlight: Option<Color>,
}

struct Para {
    runs: Vec<Run>,
    align: Align,
    ln_spc: f32,     // line-height multiple of font size (default 1.2)
    ln_spc_pts: f32, // absolute line height px (>0 overrides ln_spc)
    spc_bef: f32,    // space before paragraph (px)
    spc_aft: f32,    // space after paragraph (px)
    mar_l: f32,      // text left margin (px)
    indent: f32,     // first-line indent relative to mar_l (px; negative = hanging)
    bullet: Option<String>,
}

impl Default for Para {
    fn default() -> Self {
        Para {
            runs: Vec::new(),
            align: Align::Left,
            ln_spc: 1.2,
            ln_spc_pts: 0.0,
            spc_bef: 0.0,
            spc_aft: 0.0,
            mar_l: 0.0,
            indent: 0.0,
            bullet: None,
        }
    }
}

/// Text-frame properties from `a:bodyPr`.
#[derive(Clone, Copy)]
struct Body {
    anchor: u8, // 0=top, 1=center, 2=bottom
    ins_l: f32,
    ins_t: f32,
    ins_r: f32,
    ins_b: f32,
    font_scale: f32, // normAutofit fontScale (1.0 = none)
}

impl Default for Body {
    fn default() -> Self {
        // OOXML defaults: l/r = 0.1in (91440 EMU), t/b = 0.05in (45720 EMU)
        Body { anchor: 0, ins_l: 9.6, ins_t: 4.8, ins_r: 9.6, ins_b: 4.8, font_scale: 1.0 }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Preset {
    Rect,
    RoundRect,
    Ellipse,
    Triangle,
    RtTriangle,
    Diamond,
    Parallelogram,
    Trapezoid,
    Pentagon,
    Hexagon,
    RightArrow,
    LeftArrow,
    Line,
    Arc,
    BentConn,
}

impl Preset {
    /// Open (stroke-only) shapes: lines, arcs, connectors. The rest are filled.
    fn is_open(self) -> bool {
        matches!(self, Preset::Line | Preset::Arc | Preset::BentConn)
    }
}

fn preset_of(s: &str) -> Preset {
    match s {
        "roundRect" => Preset::RoundRect,
        "ellipse" => Preset::Ellipse,
        "triangle" => Preset::Triangle,
        "rtTriangle" => Preset::RtTriangle,
        "diamond" => Preset::Diamond,
        "parallelogram" => Preset::Parallelogram,
        "trapezoid" => Preset::Trapezoid,
        "pentagon" => Preset::Pentagon,
        "hexagon" => Preset::Hexagon,
        "rightArrow" => Preset::RightArrow,
        "leftArrow" => Preset::LeftArrow,
        "arc" => Preset::Arc,
        "line" | "straightConnector1" => Preset::Line,
        _ if s.contains("bentConnector") || s.contains("curvedConnector") => Preset::BentConn,
        _ if s.contains("onnector") => Preset::Line,
        _ => Preset::Rect, // unknown preset -> rectangle fallback
    }
}

#[derive(Clone, Copy)]
struct Outline {
    color: Color,
    width: f32,
}

/// One sub-path of a custom geometry (local coords already in px).
struct SubPath {
    cmds: Vec<dv_ir::PathVerb>,
    fill: bool,
    stroke: bool,
}

struct Shape {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    fill: Option<Color>,
    outline: Option<Outline>,
    preset: Option<Preset>,
    adj: Vec<i32>,
    custom: Vec<SubPath>,
    paras: Vec<Para>,
    body: Body,
    image: Option<dv_image::DecodedImage>,
    is_ph: bool,
    flip_h: bool,
    flip_v: bool,
    rot: f32, // degrees, clockwise
}

/// Build a shape's local→device affine: flip within bbox, rotate about centre,
/// translate to (x,y), then scale. Matches `from_row(sx,ky,kx,sy,tx,ty)`.
fn shape_tf(sh: &Shape, scale: f32) -> Transform {
    let (w, h) = (sh.w, sh.h);
    let (cx, cy) = (w / 2.0, h / 2.0);
    let (fa, fe) = if sh.flip_h { (-1.0, w) } else { (1.0, 0.0) };
    let (fd, ff) = if sh.flip_v { (-1.0, h) } else { (1.0, 0.0) };
    let th = sh.rot * std::f32::consts::PI / 180.0;
    let (co, si) = (th.cos(), th.sin());
    let a = co * fa;
    let c = -si * fd;
    let e0 = co * (fe - cx) - si * (ff - cy) + cx;
    let b = si * fa;
    let d = co * fd;
    let f0 = si * (fe - cx) + co * (ff - cy) + cy;
    Transform {
        sx: a * scale,
        ky: b * scale,
        kx: c * scale,
        sy: d * scale,
        tx: (e0 + sh.x) * scale,
        ty: (f0 + sh.y) * scale,
    }
}

/// One slide's drawables (master + layout decoration + slide shapes, z-ordered)
/// plus its resolved background colour.
struct SlideData {
    shapes: Vec<Shape>,
    bg: Option<Color>,
    bg_image: Option<dv_image::DecodedImage>,
}

/// A parsed presentation, ready for repeated slide renders.
pub struct Deck {
    slides: Vec<SlideData>,
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
    let s = s.trim();
    let h = if s.len() == 8 { &s[2..] } else { s };
    if h.len() != 6 {
        return Color::BLACK;
    }
    Color::rgb(
        u8::from_str_radix(&h[0..2], 16).unwrap_or(0),
        u8::from_str_radix(&h[2..4], 16).unwrap_or(0),
        u8::from_str_radix(&h[4..6], 16).unwrap_or(0),
    )
}

/// Theme colour scheme (dk1/lt1/dk2/lt2/accent1-6/hlink/folHlink) + the slide
/// master's colour map (bg1/tx1/... -> scheme slot).
#[derive(Clone, Default)]
struct Theme {
    colors: HashMap<String, Color>,
    clrmap: HashMap<String, String>,
}

impl Theme {
    /// Resolve an `a:schemeClr val="..."` to an RGB colour.
    fn scheme(&self, val: &str) -> Color {
        if val == "phClr" {
            return Color::rgb(0x40, 0x40, 0x40);
        }
        let slot = self.clrmap.get(val).cloned().unwrap_or_else(|| val.to_string());
        self.colors.get(&slot).copied().unwrap_or(Color::BLACK)
    }
}

fn parse_theme(xml: &str) -> HashMap<String, Color> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut out = HashMap::new();
    let mut in_scheme = false;
    let mut slot: Option<String> = None;
    const SLOTS: [&str; 12] =
        ["dk1", "lt1", "dk2", "lt2", "accent1", "accent2", "accent3", "accent4", "accent5", "accent6", "hlink", "folHlink"];
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let raw = e.name();
                let name = raw.as_ref().strip_prefix(b"a:").unwrap_or(raw.as_ref());
                if name == b"clrScheme" {
                    in_scheme = true;
                } else if in_scheme {
                    if let Ok(n) = std::str::from_utf8(name) {
                        if SLOTS.contains(&n) {
                            slot = Some(n.to_string());
                        } else if name == b"srgbClr" {
                            if let (Some(s), Some(v)) = (slot.take(), get_attr(&e, b"val")) {
                                out.insert(s, parse_color(&v));
                            }
                        } else if name == b"sysClr" {
                            if let (Some(s), Some(v)) = (slot.take(), get_attr(&e, b"lastClr")) {
                                out.insert(s, parse_color(&v));
                            }
                        }
                    }
                }
            }
            Ok(Event::End(e)) => {
                if e.name().as_ref().strip_prefix(b"a:").unwrap_or(e.name().as_ref()) == b"clrScheme" {
                    break;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

fn parse_clrmap(xml: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if e.name().as_ref() == b"p:clrMap" => {
                for a in e.attributes().flatten() {
                    if let (Ok(k), Ok(v)) = (std::str::from_utf8(a.key.as_ref()), std::str::from_utf8(a.value.as_ref())) {
                        map.insert(k.to_string(), v.to_string());
                    }
                }
                break;
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    map
}

fn rgb_to_hsl(c: Color) -> (f32, f32, f32) {
    let (r, g, b) = (c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if (max - min).abs() < 1e-6 {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        ((g - b) / d + if g < b { 6.0 } else { 0.0 }) / 6.0
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    (h, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> Color {
    let hue = |p: f32, q: f32, mut t: f32| -> f32 {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 1.0 / 2.0 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    let (r, g, b) = if s.abs() < 1e-6 {
        (l, l, l)
    } else {
        let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
        let p = 2.0 * l - q;
        (hue(p, q, h + 1.0 / 3.0), hue(p, q, h), hue(p, q, h - 1.0 / 3.0))
    };
    Color::rgb((r * 255.0).round() as u8, (g * 255.0).round() as u8, (b * 255.0).round() as u8)
}

/// Apply a DrawingML colour modifier (`a:lumMod`/`lumOff`/`shade`/`tint`, val/100000).
fn apply_mod(c: Color, kind: &[u8], val: f32) -> Color {
    match kind {
        b"lumMod" => {
            let (h, s, l) = rgb_to_hsl(c);
            hsl_to_rgb(h, s, (l * val).clamp(0.0, 1.0))
        }
        b"lumOff" => {
            let (h, s, l) = rgb_to_hsl(c);
            hsl_to_rgb(h, s, (l + val).clamp(0.0, 1.0))
        }
        b"shade" => Color::rgb(
            (c.r as f32 * val) as u8,
            (c.g as f32 * val) as u8,
            (c.b as f32 * val) as u8,
        ),
        b"tint" => Color::rgb(
            (c.r as f32 * val + 255.0 * (1.0 - val)) as u8,
            (c.g as f32 * val + 255.0 * (1.0 - val)) as u8,
            (c.b as f32 * val + 255.0 * (1.0 - val)) as u8,
        ),
        _ => c,
    }
}

fn is_cjk(ch: char) -> bool {
    let c = ch as u32;
    (0x2E80..=0x9FFF).contains(&c) || (0xAC00..=0xD7A3).contains(&c) || (0xF900..=0xFAFF).contains(&c) || (0xFF00..=0xFFEF).contains(&c)
}

/// Inherited run defaults for a placeholder kind (title vs body), resolved from
/// the master title/body styles overridden by the layout placeholder styles.
#[derive(Clone, Copy, Default)]
struct PhStyle {
    size: Option<f32>,
    bold: Option<bool>,
    color: Option<Color>,
}

/// Resolved placeholder text styles: master title/body/other defaults plus the
/// layout's per-placeholder overrides keyed by idx and by type.
#[derive(Clone, Default)]
struct PhStyles {
    master_title: PhStyle,
    master_body: PhStyle,
    master_other: PhStyle,
    by_idx: HashMap<String, PhStyle>,
    by_type: HashMap<String, PhStyle>,
}

impl PhStyles {
    /// Resolve a slide placeholder's run defaults (master type default ← layout
    /// type override ← layout idx override, most specific last).
    fn resolve(&self, ty: Option<&str>, idx: Option<&str>) -> PhStyle {
        let mut st = match ty {
            Some("ctrTitle") | Some("title") => self.master_title,
            Some("body") | Some("subTitle") | None => self.master_body,
            _ => self.master_other,
        };
        if let Some(t) = ty {
            if let Some(o) = self.by_type.get(t) {
                merge_into(&mut st, *o);
            }
        }
        if let Some(i) = idx {
            if let Some(o) = self.by_idx.get(i) {
                merge_into(&mut st, *o);
            }
        }
        st
    }
}

fn merge_into(base: &mut PhStyle, over: PhStyle) {
    if over.size.is_some() {
        base.size = over.size;
    }
    if over.bold.is_some() {
        base.bold = over.bold;
    }
    if over.color.is_some() {
        base.color = over.color;
    }
}

/// Build the layout's per-placeholder style maps (by idx, by type).
fn layout_ph_styles(xml: &str, theme: &Theme) -> (HashMap<String, PhStyle>, HashMap<String, PhStyle>) {
    let mut by_idx = HashMap::new();
    let mut by_type = HashMap::new();
    for block in xml.split("<p:sp>").skip(1) {
        let Some(phi) = block.find("<p:ph") else { continue };
        let end = block[phi..].find('>').map(|k| phi + k + 1).unwrap_or(block.len());
        let ph = &block[phi..end];
        let attr = |name: &str| {
            let pat = format!("{name}=\"");
            ph.find(&pat).map(|i| {
                let s = &ph[i + pat.len()..];
                s[..s.find('"').unwrap_or(s.len())].to_string()
            })
        };
        let style = extract_style(block, theme);
        if let Some(ty) = attr("type") {
            // accumulate so a size-less placeholder doesn't mask a sized sibling
            by_type.entry(ty).and_modify(|e| merge_into(e, style)).or_insert(style);
        }
        if let Some(idx) = attr("idx") {
            by_idx.entry(idx).or_insert(style);
        }
    }
    (by_idx, by_type)
}

/// Read the first `<a:defRPr>` (size in px, bold, colour) from a style fragment.
fn extract_style(frag: &str, theme: &Theme) -> PhStyle {
    let mut reader = Reader::from_str(frag);
    let mut buf = Vec::new();
    let mut st = PhStyle::default();
    let mut in_def = false;
    let mut done = false;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let n = e.name();
                let nm = n.as_ref();
                if nm == b"a:defRPr" && !done {
                    in_def = true;
                    if let Some(sz) = get_attr(&e, b"sz").and_then(|v| v.parse::<f32>().ok()) {
                        st.size = Some(sz / 75.0);
                    }
                    if let Some(b) = get_attr(&e, b"b") {
                        st.bold = Some(b == "1");
                    }
                } else if in_def && st.color.is_none() && (nm == b"a:srgbClr" || nm == b"a:schemeClr") {
                    st.color = if nm == b"a:schemeClr" {
                        get_attr(&e, b"val").map(|v| theme.scheme(&v))
                    } else {
                        get_attr(&e, b"val").map(|v| parse_color(&v))
                    };
                }
            }
            Ok(Event::End(e)) => {
                if e.name().as_ref() == b"a:defRPr" && in_def {
                    in_def = false;
                    done = true;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    st
}

fn slice_between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let i = s.find(open)?;
    let j = s[i..].find(close)? + i;
    Some(&s[i..j + close.len()])
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
        let pres_rels = read_entry(&mut zip, "ppt/_rels/presentation.xml.rels").map(|s| rels_map(&s)).unwrap_or_default();

        for rid in rids {
            let Some(target) = pres_rels.get(&rid).cloned() else { continue };
            let slide_path = resolve_rel("ppt", &target);
            let Some(slide_xml) = read_entry(&mut zip, &slide_path) else { continue };
            let (sdir, sfile) = split_path(&slide_path);
            let slide_rels_xml = read_entry(&mut zip, &format!("{}/_rels/{}.rels", sdir, sfile)).unwrap_or_default();
            let slide_rels = rels_map(&slide_rels_xml);

            // Resolve the slide -> layout -> master -> theme chain.
            let mut theme = Theme::default();
            let (mut master_xml, mut master_dir) = (String::new(), String::new());
            let mut master_rels = HashMap::new();
            let (mut layout_xml, mut layout_dir) = (String::new(), String::new());
            let mut layout_rels = HashMap::new();

            if let Some(lt) = rel_target(&slide_rels_xml, "slideLayout") {
                let lp = resolve_rel(&sdir, &lt);
                let (ld, lf) = split_path(&lp);
                layout_dir = ld.clone();
                layout_xml = read_entry(&mut zip, &lp).unwrap_or_default();
                let lrels_xml = read_entry(&mut zip, &format!("{}/_rels/{}.rels", ld, lf)).unwrap_or_default();
                layout_rels = rels_map(&lrels_xml);
                if let Some(mt) = rel_target(&lrels_xml, "slideMaster") {
                    let mp = resolve_rel(&ld, &mt);
                    let (md, mf) = split_path(&mp);
                    master_dir = md.clone();
                    master_xml = read_entry(&mut zip, &mp).unwrap_or_default();
                    let mrels_xml = read_entry(&mut zip, &format!("{}/_rels/{}.rels", md, mf)).unwrap_or_default();
                    master_rels = rels_map(&mrels_xml);
                    theme.clrmap = parse_clrmap(&master_xml);
                    if let Some(tt) = rel_target(&mrels_xml, "theme") {
                        let tp = resolve_rel(&md, &tt);
                        theme.colors = read_entry(&mut zip, &tp).map(|x| parse_theme(&x)).unwrap_or_default();
                    }
                }
            }

            // Placeholder text-style cascade: master title/body/other defaults +
            // master/layout per-placeholder overrides keyed by idx and by type.
            let mut styles = PhStyles::default();
            if !master_xml.is_empty() {
                if let Some(s) = slice_between(&master_xml, "<p:titleStyle>", "</p:titleStyle>") {
                    styles.master_title = extract_style(s, &theme);
                }
                if let Some(s) = slice_between(&master_xml, "<p:bodyStyle>", "</p:bodyStyle>") {
                    styles.master_body = extract_style(s, &theme);
                }
                if let Some(s) = slice_between(&master_xml, "<p:otherStyle>", "</p:otherStyle>") {
                    styles.master_other = extract_style(s, &theme);
                }
                let (mi, mt) = layout_ph_styles(&master_xml, &theme);
                styles.by_idx.extend(mi);
                styles.by_type.extend(mt);
            }
            if !layout_xml.is_empty() {
                let (li, lt) = layout_ph_styles(&layout_xml, &theme);
                styles.by_type.extend(lt); // layout refines/overrides master placeholder styles
                styles.by_idx.extend(li);
            }

            // Z-order: master decoration, then layout decoration, then slide content.
            let def = PhStyles::default();
            let mut shapes = Vec::new();
            let mut bg = None;
            let mut bg_image = None;
            if !master_xml.is_empty() {
                let (ms, mbg, mbi) = parse_part_shapes(&master_xml, &master_rels, &master_dir, &mut zip, &theme, &def, true);
                shapes.extend(ms);
                bg = mbg;
                bg_image = mbi;
            }
            if !layout_xml.is_empty() {
                let (ls, lbg, lbi) = parse_part_shapes(&layout_xml, &layout_rels, &layout_dir, &mut zip, &theme, &def, true);
                shapes.extend(ls);
                bg = lbg.or(bg);
                bg_image = lbi.or(bg_image);
            }
            let (ss, sbg, sbi) = parse_part_shapes(&slide_xml, &slide_rels, &sdir, &mut zip, &theme, &styles, false);
            shapes.extend(ss);
            let bg = sbg.or(bg);
            let bg_image = sbi.or(bg_image);

            deck.slides.push(SlideData { shapes, bg, bg_image });
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
        let Some(sd) = self.slides.get(idx) else { return dl };
        let shapes = &sd.shapes;

        // Slide background: resolved colour, then a full-slide background image.
        if let Some(bg) = sd.bg {
            dl.push(fill_rect(0.0, 0.0, self.width * scale, self.height * scale, bg));
        }
        if let Some(img) = &sd.bg_image {
            dl.push(Command::Image {
                rgba: img.rgba.clone(),
                src_w: img.width,
                src_h: img.height,
                x: 0.0,
                y: 0.0,
                w: self.width * scale,
                h: self.height * scale,
                clip: None,
            });
        }

        for sh in shapes {
            let tf = shape_tf(sh, scale);
            if !sh.custom.is_empty() {
                for sp in &sh.custom {
                    let mut p = PathData::new();
                    p.verbs = sp.cmds.clone();
                    if sp.fill {
                        if let Some(c) = sh.fill {
                            dl.push(Command::FillPath { path: p.clone(), paint: Paint::Solid(c), fill_rule: FillRule::NonZero, transform: tf });
                        }
                    }
                    if sp.stroke {
                        if let Some(o) = sh.outline {
                            dl.push(Command::StrokePath { path: p, paint: Paint::Solid(o.color), width: o.width, transform: tf });
                        }
                    }
                }
            } else if let Some(preset) = sh.preset {
                let p = preset_path(preset, sh.w, sh.h, &sh.adj);
                if !preset.is_open() {
                    if let Some(c) = sh.fill {
                        dl.push(Command::FillPath { path: p.clone(), paint: Paint::Solid(c), fill_rule: FillRule::NonZero, transform: tf });
                    }
                }
                if let Some(o) = sh.outline {
                    dl.push(Command::StrokePath { path: p, paint: Paint::Solid(o.color), width: o.width, transform: tf });
                } else if preset.is_open() {
                    // a connector/line with no explicit outline still needs a stroke
                    dl.push(Command::StrokePath { path: p, paint: Paint::Solid(Color::rgb(0x40, 0x40, 0x40)), width: 1.0, transform: tf });
                }
            } else if let Some(fill) = sh.fill {
                dl.push(fill_rect(sh.x * scale, sh.y * scale, sh.w * scale, sh.h * scale, fill));
            }

            if let Some(img) = &sh.image {
                // Clip the image to the shape's geometry (e.g. an ellipse-cropped photo).
                let clip = match sh.preset {
                    Some(p) if p != Preset::Rect => {
                        let mut path = preset_path(p, sh.w, sh.h, &sh.adj);
                        for v in &mut path.verbs {
                            *v = tf_verb(*v, &tf);
                        }
                        Some(path)
                    }
                    _ if !sh.custom.is_empty() => {
                        let mut path = PathData::new();
                        path.verbs = sh.custom.iter().flat_map(|sp| sp.cmds.iter().copied()).collect();
                        for v in &mut path.verbs {
                            *v = tf_verb(*v, &tf);
                        }
                        Some(path)
                    }
                    _ => None,
                };
                dl.push(Command::Image {
                    rgba: img.rgba.clone(),
                    src_w: img.width,
                    src_h: img.height,
                    x: sh.x * scale,
                    y: sh.y * scale,
                    w: sh.w * scale,
                    h: sh.h * scale,
                    clip,
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
        let b = &sh.body;
        let fs = b.font_scale;
        let left = (sh.x + b.ins_l) * scale;
        let content_w = ((sh.w - b.ins_l - b.ins_r) * scale).max(8.0);
        let top = (sh.y + b.ins_t) * scale;
        let content_h = ((sh.h - b.ins_t - b.ins_b) * scale).max(0.0);

        struct LLine {
            items: Vec<Item>,
            h: f32,
            asc: f32,
            gap: f32, // space before this line (paragraph spacing)
            align: Align,
            mar_l: f32,
            indent: f32,
            bullet: Option<(String, f32, Color)>,
        }
        let mut lines: Vec<LLine> = Vec::new();
        let mut total = 0.0f32;
        let mut pending = 0.0f32;

        for para in &sh.paras {
            pending += para.spc_bef * scale;
            let bullet = para.bullet.as_ref().filter(|c| !c.is_empty()).map(|c| {
                let sz = para.runs.first().map(|r| r.size).unwrap_or(18.0) * scale * fs;
                let col = para.runs.first().map(|r| r.color).unwrap_or(Color::BLACK);
                (c.clone(), sz, col)
            });
            let text_w = (content_w - para.mar_l * scale).max(8.0);
            let items = shape_para(font, para, scale, fs);
            let wrapped = if items.is_empty() { vec![Vec::new()] } else { wrap(items, text_w) };
            for (li, line) in wrapped.into_iter().enumerate() {
                let max_size = line.iter().map(|i| i.size).fold(14.0 * scale * fs, f32::max);
                let h = if para.ln_spc_pts > 0.0 { para.ln_spc_pts * scale } else { max_size * para.ln_spc };
                let gap = if li == 0 { std::mem::take(&mut pending) } else { 0.0 };
                total += gap + h;
                lines.push(LLine {
                    items: line,
                    h,
                    asc: max_size * 0.8,
                    gap,
                    align: para.align,
                    mar_l: para.mar_l * scale,
                    indent: para.indent * scale,
                    bullet: if li == 0 { bullet.clone() } else { None },
                });
            }
            pending += para.spc_aft * scale;
        }

        // Vertical anchor of the whole text block within the content box.
        let mut y = match b.anchor {
            1 => top + (content_h - total) / 2.0,
            2 => top + (content_h - total),
            _ => top,
        };
        if y < top {
            y = top;
        }

        for ln in &lines {
            y += ln.gap;
            let baseline = y + ln.asc;
            let text_left = left + ln.mar_l;
            let text_w = (content_w - ln.mar_l).max(8.0);
            let lw = line_width(&ln.items);
            let x_start = match ln.align {
                Align::Left => text_left,
                Align::Center => text_left + (text_w - lw) / 2.0,
                Align::Right => text_left + (text_w - lw),
            };
            // Bullet (hangs at text_left + indent).
            if let Some((ch, bsz, bcol)) = &ln.bullet {
                let shaped = shape(font, ch, *bsz);
                let s = *bsz / shaped.units_per_em.max(1.0);
                let mut bx = text_left + ln.indent;
                let mut glyphs = Vec::new();
                for g in &shaped.glyphs {
                    glyphs.push(PositionedGlyph { id: g.glyph_id, x: bx + g.x_offset * s, y: baseline });
                    bx += g.x_advance * s;
                }
                dl.push(Command::Glyphs(GlyphRun { font: FontId(0), size: *bsz, paint: Paint::Solid(*bcol), bold: false, glyphs }));
            }
            // Highlight background runs first (so glyphs sit on top).
            {
                let mut x = x_start;
                let mut i = 0;
                while i < ln.items.len() {
                    let hl = ln.items[i].highlight;
                    let run_x0 = x;
                    let mut size = ln.items[i].size;
                    while i < ln.items.len() && ln.items[i].highlight == hl {
                        size = size.max(ln.items[i].size);
                        x += ln.items[i].advance;
                        i += 1;
                    }
                    if let Some(c) = hl {
                        if x > run_x0 {
                            dl.push(fill_rect(run_x0, baseline - size * 0.82, x - run_x0, size * 1.05, c));
                        }
                    }
                }
            }
            // Glyph runs grouped by style; underline drawn per underlined span.
            let mut x = x_start;
            let mut i = 0;
            while i < ln.items.len() {
                let (size, color, bold, ul) = (ln.items[i].size, ln.items[i].color, ln.items[i].bold, ln.items[i].underline);
                let run_x0 = x;
                let mut glyphs = Vec::new();
                while i < ln.items.len()
                    && ln.items[i].size == size
                    && ln.items[i].color == color
                    && ln.items[i].bold == bold
                    && ln.items[i].underline == ul
                {
                    glyphs.push(PositionedGlyph { id: ln.items[i].gid, x: x + ln.items[i].x_off, y: baseline });
                    x += ln.items[i].advance;
                    i += 1;
                }
                dl.push(Command::Glyphs(GlyphRun { font: FontId(0), size, paint: Paint::Solid(color), bold, glyphs }));
                if ul && x > run_x0 {
                    let uy = baseline + size * 0.12;
                    dl.push(fill_rect(run_x0, uy, x - run_x0, (size * 0.06).max(1.0), color));
                }
            }
            y += ln.h;
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

fn split_path(p: &str) -> (String, String) {
    match p.rsplit_once('/') {
        Some((d, f)) => (d.to_string(), f.to_string()),
        None => (String::new(), p.to_string()),
    }
}

/// First relationship Target whose Type ends with `type_suffix`.
fn rel_target(xml: &str, type_suffix: &str) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if e.name().as_ref() == b"Relationship" => {
                if get_attr(&e, b"Type").is_some_and(|t| t.ends_with(type_suffix)) {
                    if let Some(t) = get_attr(&e, b"Target") {
                        return Some(t);
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }
}

#[allow(clippy::too_many_arguments)]
fn parse_part_shapes(
    xml: &str,
    slide_rels: &HashMap<String, String>,
    slide_dir: &str,
    zip: &mut Zip,
    theme: &Theme,
    ph_styles: &PhStyles,
    skip_ph: bool,
) -> (Vec<Shape>, Option<Color>, Option<dv_image::DecodedImage>) {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut shapes = Vec::new();
    let mut bg: Option<Color> = None;
    let mut bg_image: Option<dv_image::DecodedImage> = None;

    let mut cur: Option<Shape> = None;
    let mut cur_ph = PhStyle::default();
    let mut in_sppr = false;
    let mut in_rpr = false;
    let mut in_bg = false;
    let mut in_hl = false; // inside a:highlight
    let mut in_ln = false;
    let mut ln_width = 1.0f32;
    let mut cur_para: Option<Para> = None;
    let mut spc_target: u8 = 0; // 1=lnSpc 2=spcBef 3=spcAft
    let mut cur_run: Option<Run> = None;
    let mut in_t = false;
    // custom geometry state
    let mut cur_sub: Option<SubPath> = None;
    let mut path_w = 1.0f32;
    let mut path_h = 1.0f32;
    let mut cmd_kind: u8 = 0; // 1=move 2=line 3=cubic
    let mut pt_buf: Vec<(f32, f32)> = Vec::new();
    // group nesting: stack of accumulated (scale_x, scale_y, trans_x, trans_y)
    // mapping child coords -> absolute. Base = identity.
    let mut gstack: Vec<(f32, f32, f32, f32)> = vec![(1.0, 1.0, 0.0, 0.0)];
    let mut in_grpspr = false;
    let mut grp_depth: u32 = 0; // nesting of real <p:grpSp> (excludes the spTree root)
    let mut g_xfrm = [0.0f32; 8]; // off x,y, ext cx,cy, chOff x,y, chExt cx,cy

    loop {
        let event = reader.read_event_into(&mut buf);
        // Empty (self-closing) elements have no End event, so scope flags like
        // in_rpr/in_ln must NOT latch on them or they leak into later siblings.
        let empty = matches!(&event, Ok(Event::Empty(_)));
        match event {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match e.name().as_ref() {
                b"p:sp" | b"p:pic" | b"p:cxnSp" => {
                    cur_ph = PhStyle::default();
                    cur = Some(Shape {
                        x: 0.0,
                        y: 0.0,
                        w: 0.0,
                        h: 0.0,
                        fill: None,
                        outline: None,
                        preset: None,
                        adj: Vec::new(),
                        custom: Vec::new(),
                        paras: Vec::new(),
                        body: Body::default(),
                        image: None,
                        is_ph: false,
                        flip_h: false,
                        flip_v: false,
                        rot: 0.0,
                    })
                }
                b"p:grpSp" => grp_depth += 1,
                b"p:grpSpPr" => {
                    in_grpspr = !empty;
                    g_xfrm = [0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0];
                }
                b"a:xfrm" => {
                    if !in_grpspr {
                        if let Some(s) = cur.as_mut() {
                            s.flip_h = get_attr(&e, b"flipH").as_deref() == Some("1");
                            s.flip_v = get_attr(&e, b"flipV").as_deref() == Some("1");
                            s.rot = get_attr(&e, b"rot").and_then(|v| v.parse::<f32>().ok()).map(|r| r / 60000.0).unwrap_or(0.0);
                        }
                    }
                }
                b"a:chOff" => {
                    if in_grpspr {
                        g_xfrm[4] = get_attr(&e, b"x").and_then(|v| v.parse().ok()).unwrap_or(0.0);
                        g_xfrm[5] = get_attr(&e, b"y").and_then(|v| v.parse().ok()).unwrap_or(0.0);
                    }
                }
                b"a:chExt" => {
                    if in_grpspr {
                        g_xfrm[6] = get_attr(&e, b"cx").and_then(|v| v.parse().ok()).unwrap_or(1.0);
                        g_xfrm[7] = get_attr(&e, b"cy").and_then(|v| v.parse().ok()).unwrap_or(1.0);
                    }
                }
                b"p:ph" => {
                    if let Some(s) = cur.as_mut() {
                        s.is_ph = true;
                    }
                    let ty = get_attr(&e, b"type");
                    let idx = get_attr(&e, b"idx");
                    cur_ph = ph_styles.resolve(ty.as_deref(), idx.as_deref());
                }
                b"p:bg" => in_bg = !empty,
                b"p:spPr" => in_sppr = !empty,
                b"a:blip" => {
                    if let Some(rid) = get_attr(&e, b"r:embed") {
                        if let Some(target) = slide_rels.get(&rid) {
                            let path = resolve_rel(slide_dir, target);
                            if let Some(img) = read_bytes(zip, &path).and_then(|b| dv_image::decode(&b)) {
                                if in_bg {
                                    bg_image = Some(img); // p:bg blipFill -> full-slide background
                                } else if let Some(s) = cur.as_mut() {
                                    s.image = Some(img);
                                }
                            }
                        }
                    }
                }
                b"a:off" => {
                    let x = get_attr(&e, b"x").and_then(|v| v.parse::<f32>().ok());
                    let y = get_attr(&e, b"y").and_then(|v| v.parse::<f32>().ok());
                    if in_grpspr {
                        g_xfrm[0] = x.unwrap_or(0.0);
                        g_xfrm[1] = y.unwrap_or(0.0);
                    } else if let Some(s) = cur.as_mut() {
                        if let Some(x) = x {
                            s.x = x / EMU_PER_PX;
                        }
                        if let Some(y) = y {
                            s.y = y / EMU_PER_PX;
                        }
                    }
                }
                b"a:ext" => {
                    let cx = get_attr(&e, b"cx").and_then(|v| v.parse::<f32>().ok());
                    let cy = get_attr(&e, b"cy").and_then(|v| v.parse::<f32>().ok());
                    if in_grpspr {
                        g_xfrm[2] = cx.unwrap_or(1.0);
                        g_xfrm[3] = cy.unwrap_or(1.0);
                    } else if let Some(s) = cur.as_mut() {
                        if let Some(cx) = cx {
                            s.w = cx / EMU_PER_PX;
                        }
                        if let Some(cy) = cy {
                            s.h = cy / EMU_PER_PX;
                        }
                    }
                }
                b"a:srgbClr" | b"a:schemeClr" => {
                    let col = if e.name().as_ref() == b"a:schemeClr" {
                        get_attr(&e, b"val").map(|v| theme.scheme(&v))
                    } else {
                        get_attr(&e, b"val").map(|v| parse_color(&v))
                    };
                    if let Some(col) = col {
                        if in_hl {
                            if let Some(r) = cur_run.as_mut() {
                                r.highlight = Some(col);
                            }
                        } else if in_bg {
                            bg = Some(col);
                        } else if in_ln {
                            if let Some(s) = cur.as_mut() {
                                s.outline = Some(Outline { color: col, width: ln_width });
                            }
                        } else if in_rpr {
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
                b"a:highlight" => in_hl = !empty,
                b"a:lumMod" | b"a:lumOff" | b"a:shade" | b"a:tint" => {
                    if let Some(val) = get_attr(&e, b"val").and_then(|v| v.parse::<f32>().ok()).map(|v| v / 100000.0) {
                        let kind = e.name().as_ref().strip_prefix(b"a:").unwrap_or(b"").to_vec();
                        if in_bg {
                            if let Some(c) = bg {
                                bg = Some(apply_mod(c, &kind, val));
                            }
                        } else if in_ln {
                            if let Some(o) = cur.as_mut().and_then(|s| s.outline.as_mut()) {
                                o.color = apply_mod(o.color, &kind, val);
                            }
                        } else if in_rpr {
                            if let Some(r) = cur_run.as_mut() {
                                r.color = apply_mod(r.color, &kind, val);
                            }
                        } else if in_sppr {
                            if let Some(s) = cur.as_mut() {
                                if let Some(f) = s.fill {
                                    s.fill = Some(apply_mod(f, &kind, val));
                                }
                            }
                        }
                    }
                }
                b"a:noFill" => {
                    if in_ln {
                        if let Some(s) = cur.as_mut() {
                            s.outline = None;
                        }
                    } else if in_bg {
                        bg = None;
                    } else if in_sppr {
                        if let Some(s) = cur.as_mut() {
                            s.fill = None;
                        }
                    }
                }
                b"a:prstGeom" => {
                    if let (Some(s), Some(prst)) = (cur.as_mut(), get_attr(&e, b"prst")) {
                        s.preset = Some(preset_of(&prst));
                    }
                }
                b"a:gd" => {
                    if let Some(s) = cur.as_mut() {
                        if let Some(v) = get_attr(&e, b"fmla").and_then(|f| f.split_whitespace().last().and_then(|x| x.parse::<i32>().ok())) {
                            s.adj.push(v);
                        }
                    }
                }
                b"a:ln" => {
                    // A bare <a:ln/> with no solidFill draws NO line. Remember the
                    // width; the outline is created only when a line colour appears.
                    in_ln = !empty;
                    ln_width = get_attr(&e, b"w").and_then(|v| v.parse::<f32>().ok()).map(|emu| emu / EMU_PER_PX).unwrap_or(1.0).max(0.75);
                }
                b"a:custGeom" => {
                    if let Some(s) = cur.as_mut() {
                        s.custom.clear();
                    }
                }
                b"a:path" => {
                    path_w = get_attr(&e, b"w").and_then(|v| v.parse().ok()).unwrap_or(1.0);
                    path_h = get_attr(&e, b"h").and_then(|v| v.parse().ok()).unwrap_or(1.0);
                    let fill = get_attr(&e, b"fill").as_deref() != Some("none");
                    let stroke = !matches!(get_attr(&e, b"stroke").as_deref(), Some("0") | Some("false"));
                    cur_sub = Some(SubPath { cmds: Vec::new(), fill, stroke });
                }
                b"a:moveTo" => {
                    cmd_kind = 1;
                    pt_buf.clear();
                }
                b"a:lnTo" => {
                    cmd_kind = 2;
                    pt_buf.clear();
                }
                b"a:cubicBezTo" => {
                    cmd_kind = 3;
                    pt_buf.clear();
                }
                b"a:pt" => {
                    if let (Some(s), Some(x), Some(y)) = (
                        cur.as_ref(),
                        get_attr(&e, b"x").and_then(|v| v.parse::<f32>().ok()),
                        get_attr(&e, b"y").and_then(|v| v.parse::<f32>().ok()),
                    ) {
                        pt_buf.push((x / path_w.max(1.0) * s.w, y / path_h.max(1.0) * s.h));
                    }
                }
                b"a:close" => {
                    if let Some(sp) = cur_sub.as_mut() {
                        sp.cmds.push(PathVerb::Close);
                    }
                }
                b"a:bodyPr" => {
                    if let Some(s) = cur.as_mut() {
                        s.body.anchor = match get_attr(&e, b"anchor").as_deref() {
                            Some("ctr") => 1,
                            Some("b") => 2,
                            _ => 0,
                        };
                        let emu = |k: &[u8]| get_attr(&e, k).and_then(|v| v.parse::<f32>().ok()).map(|x| x / EMU_PER_PX);
                        if let Some(v) = emu(b"lIns") {
                            s.body.ins_l = v;
                        }
                        if let Some(v) = emu(b"tIns") {
                            s.body.ins_t = v;
                        }
                        if let Some(v) = emu(b"rIns") {
                            s.body.ins_r = v;
                        }
                        if let Some(v) = emu(b"bIns") {
                            s.body.ins_b = v;
                        }
                    }
                }
                b"a:normAutofit" => {
                    if let Some(s) = cur.as_mut() {
                        if let Some(fs) = get_attr(&e, b"fontScale").and_then(|v| v.parse::<f32>().ok()) {
                            s.body.font_scale = fs / 100000.0;
                        }
                    }
                }
                b"a:p" => {
                    cur_para = Some(Para::default());
                }
                b"a:pPr" => {
                    if let Some(p) = cur_para.as_mut() {
                        p.align = match get_attr(&e, b"algn").as_deref() {
                            Some("ctr") => Align::Center,
                            Some("r") => Align::Right,
                            _ => Align::Left,
                        };
                        if let Some(v) = get_attr(&e, b"marL").and_then(|v| v.parse::<f32>().ok()) {
                            p.mar_l = v / EMU_PER_PX;
                        }
                        if let Some(v) = get_attr(&e, b"indent").and_then(|v| v.parse::<f32>().ok()) {
                            p.indent = v / EMU_PER_PX;
                        }
                    }
                }
                b"a:lnSpc" => spc_target = 1,
                b"a:spcBef" => spc_target = 2,
                b"a:spcAft" => spc_target = 3,
                b"a:spcPct" => {
                    // Percent applies to line spacing; spcBef/spcAft percent is rare -> ignored.
                    if spc_target == 1 {
                        if let (Some(p), Some(v)) = (cur_para.as_mut(), get_attr(&e, b"val").and_then(|v| v.parse::<f32>().ok())) {
                            p.ln_spc = v / 100000.0;
                        }
                    }
                }
                b"a:spcPts" => {
                    if let (Some(p), Some(v)) = (cur_para.as_mut(), get_attr(&e, b"val").and_then(|v| v.parse::<f32>().ok())) {
                        let px = v / 75.0; // hundredths-of-point -> px
                        match spc_target {
                            1 => p.ln_spc_pts = px,
                            2 => p.spc_bef = px,
                            3 => p.spc_aft = px,
                            _ => {}
                        }
                    }
                }
                b"a:buNone" => {
                    if let Some(p) = cur_para.as_mut() {
                        p.bullet = None;
                    }
                }
                b"a:buChar" => {
                    if let Some(p) = cur_para.as_mut() {
                        p.bullet = get_attr(&e, b"char");
                    }
                }
                b"a:r" => {
                    cur_run = Some(Run {
                        text: String::new(),
                        size: cur_ph.size.unwrap_or(24.0),
                        bold: cur_ph.bold.unwrap_or(false),
                        underline: false,
                        color: cur_ph.color.unwrap_or(Color::BLACK),
                        highlight: None,
                    })
                }
                b"a:rPr" => {
                    in_rpr = !empty;
                    if let Some(r) = cur_run.as_mut() {
                        if let Some(sz) = get_attr(&e, b"sz").and_then(|v| v.parse::<f32>().ok()) {
                            r.size = sz / 75.0; // hundredths-of-point -> px
                        }
                        if let Some(b) = get_attr(&e, b"b") {
                            r.bold = b == "1";
                        }
                        if let Some(u) = get_attr(&e, b"u") {
                            r.underline = u != "none";
                        }
                    }
                }
                b"a:latin" | b"a:ea" | b"a:cs" => {
                    // We can't load the embedded Latin fonts (Epilogue Black, …) for CJK,
                    // but approximate their weight: a "Black"/"Bold"/"Heavy" face -> faux-bold.
                    if in_rpr {
                        if let (Some(r), Some(tf)) = (cur_run.as_mut(), get_attr(&e, b"typeface")) {
                            if ["Black", "Bold", "Heavy", "Semibold", "SemiBold"].iter().any(|w| tf.contains(w)) {
                                r.bold = true;
                            }
                        }
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
                b"p:bg" => in_bg = false,
                b"a:rPr" => in_rpr = false,
                b"a:ln" => in_ln = false,
                b"p:grpSpPr" => {
                    in_grpspr = false;
                    // Only real <p:grpSp> groups establish a child transform; the
                    // spTree root's grpSpPr (depth 0) is ignored.
                    if grp_depth > 0 {
                        let gsx = if g_xfrm[6] != 0.0 { g_xfrm[2] / g_xfrm[6] } else { 1.0 };
                        let gsy = if g_xfrm[7] != 0.0 { g_xfrm[3] / g_xfrm[7] } else { 1.0 };
                        let ltx = (g_xfrm[0] - g_xfrm[4] * gsx) / EMU_PER_PX;
                        let lty = (g_xfrm[1] - g_xfrm[5] * gsy) / EMU_PER_PX;
                        let (psx, psy, ptx, pty) = *gstack.last().unwrap();
                        gstack.push((psx * gsx, psy * gsy, psx * ltx + ptx, psy * lty + pty));
                    }
                }
                b"p:grpSp" => {
                    grp_depth = grp_depth.saturating_sub(1);
                    if gstack.len() > 1 {
                        gstack.pop();
                    }
                }
                b"a:moveTo" | b"a:lnTo" | b"a:cubicBezTo" => {
                    if let Some(sp) = cur_sub.as_mut() {
                        match cmd_kind {
                            1 => {
                                if let Some(&(x, y)) = pt_buf.first() {
                                    sp.cmds.push(PathVerb::MoveTo(x, y));
                                }
                            }
                            2 => {
                                if let Some(&(x, y)) = pt_buf.first() {
                                    sp.cmds.push(PathVerb::LineTo(x, y));
                                }
                            }
                            3 => {
                                if pt_buf.len() >= 3 {
                                    let (a, b, c) = (pt_buf[0], pt_buf[1], pt_buf[2]);
                                    sp.cmds.push(PathVerb::CubicTo(a.0, a.1, b.0, b.1, c.0, c.1));
                                }
                            }
                            _ => {}
                        }
                    }
                    cmd_kind = 0;
                }
                b"a:path" => {
                    if let (Some(s), Some(sp)) = (cur.as_mut(), cur_sub.take()) {
                        s.custom.push(sp);
                    }
                }
                b"a:t" => in_t = false,
                b"a:highlight" => in_hl = false,
                b"a:lnSpc" | b"a:spcBef" | b"a:spcAft" => spc_target = 0,
                b"a:r" => {
                    if let (Some(p), Some(r)) = (cur_para.as_mut(), cur_run.take()) {
                        if !r.text.is_empty() {
                            p.runs.push(r);
                        }
                    }
                }
                b"a:p" => {
                    if let (Some(s), Some(p)) = (cur.as_mut(), cur_para.take()) {
                        s.paras.push(p);
                    }
                }
                b"p:sp" | b"p:pic" | b"p:cxnSp" => {
                    if let Some(mut s) = cur.take() {
                        // Map the shape from its group's child space to absolute px.
                        let (gsx, gsy, gtx, gty) = *gstack.last().unwrap();
                        if (gsx, gsy, gtx, gty) != (1.0, 1.0, 0.0, 0.0) {
                            s.x = s.x * gsx + gtx;
                            s.y = s.y * gsy + gty;
                            s.w *= gsx;
                            s.h *= gsy;
                            for sub in &mut s.custom {
                                for v in &mut sub.cmds {
                                    *v = scale_verb(*v, gsx, gsy);
                                }
                            }
                        }
                        if !(skip_ph && s.is_ph) {
                            shapes.push(s);
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    (shapes, bg, bg_image)
}

struct Item {
    gid: u32,
    advance: f32,
    x_off: f32,
    size: f32,
    color: Color,
    bold: bool,
    underline: bool,
    highlight: Option<Color>,
    break_after: bool,
    is_space: bool,
}

fn shape_para(font: &FontData, para: &Para, scale: f32, font_scale: f32) -> Vec<Item> {
    let mut items = Vec::new();
    for run in &para.runs {
        let px = run.size * scale * font_scale;
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
                underline: run.underline,
                highlight: run.highlight,
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

fn adj_or(adj: &[i32], i: usize, default: i32) -> f32 {
    *adj.get(i).unwrap_or(&default) as f32 / 100000.0
}

/// Apply an affine `Transform` to every point of a path verb (local -> device).
fn tf_verb(v: PathVerb, t: &Transform) -> PathVerb {
    let m = |x: f32, y: f32| (t.sx * x + t.kx * y + t.tx, t.ky * x + t.sy * y + t.ty);
    match v {
        PathVerb::MoveTo(x, y) => {
            let (x, y) = m(x, y);
            PathVerb::MoveTo(x, y)
        }
        PathVerb::LineTo(x, y) => {
            let (x, y) = m(x, y);
            PathVerb::LineTo(x, y)
        }
        PathVerb::QuadTo(a, b, c, d) => {
            let (a, b) = m(a, b);
            let (c, d) = m(c, d);
            PathVerb::QuadTo(a, b, c, d)
        }
        PathVerb::CubicTo(a, b, c, d, e, f) => {
            let (a, b) = m(a, b);
            let (c, d) = m(c, d);
            let (e, f) = m(e, f);
            PathVerb::CubicTo(a, b, c, d, e, f)
        }
        PathVerb::Close => PathVerb::Close,
    }
}

fn scale_verb(v: PathVerb, sx: f32, sy: f32) -> PathVerb {
    match v {
        PathVerb::MoveTo(x, y) => PathVerb::MoveTo(x * sx, y * sy),
        PathVerb::LineTo(x, y) => PathVerb::LineTo(x * sx, y * sy),
        PathVerb::QuadTo(a, b, c, d) => PathVerb::QuadTo(a * sx, b * sy, c * sx, d * sy),
        PathVerb::CubicTo(a, b, c, d, e, f) => PathVerb::CubicTo(a * sx, b * sy, c * sx, d * sy, e * sx, f * sy),
        PathVerb::Close => PathVerb::Close,
    }
}

/// Generate a closed path for a preset shape in local px coords (origin top-left).
fn preset_path(p: Preset, w: f32, h: f32, adj: &[i32]) -> PathData {
    let mut path = PathData::new();
    let ss = w.min(h);
    match p {
        Preset::Rect => {
            path.move_to(0.0, 0.0);
            path.line_to(w, 0.0);
            path.line_to(w, h);
            path.line_to(0.0, h);
            path.close();
        }
        Preset::Line => {
            path.move_to(0.0, 0.0);
            path.line_to(w, h);
        }
        Preset::BentConn => {
            // single right-angle bend (orientation comes from xfrm flip/rot)
            path.move_to(0.0, 0.0);
            path.line_to(w, 0.0);
            path.line_to(w, h);
        }
        Preset::Arc => {
            // elliptical arc from adj1 (start) sweeping to adj2 (end), in 60000ths°
            let (cx, cy, rx, ry) = (w / 2.0, h / 2.0, w / 2.0, h / 2.0);
            let st = *adj.first().unwrap_or(&16200000) as f32 / 60000.0 * std::f32::consts::PI / 180.0;
            let en = *adj.get(1).unwrap_or(&0) as f32 / 60000.0 * std::f32::consts::PI / 180.0;
            let n = 24;
            for i in 0..=n {
                let a = st + (en - st) * (i as f32 / n as f32);
                let (x, y) = (cx + rx * a.cos(), cy + ry * a.sin());
                if i == 0 {
                    path.move_to(x, y);
                } else {
                    path.line_to(x, y);
                }
            }
        }
        Preset::RoundRect => {
            let r = (adj_or(adj, 0, 16667) * ss).min(w.min(h) / 2.0);
            path.move_to(r, 0.0);
            path.line_to(w - r, 0.0);
            path.quad_to(w, 0.0, w, r);
            path.line_to(w, h - r);
            path.quad_to(w, h, w - r, h);
            path.line_to(r, h);
            path.quad_to(0.0, h, 0.0, h - r);
            path.line_to(0.0, r);
            path.quad_to(0.0, 0.0, r, 0.0);
            path.close();
        }
        Preset::Ellipse => {
            let (cx, cy, rx, ry, k) = (w / 2.0, h / 2.0, w / 2.0, h / 2.0, 0.5523_f32);
            path.move_to(0.0, cy);
            path.cubic_to(0.0, cy - ry * k, cx - rx * k, 0.0, cx, 0.0);
            path.cubic_to(cx + rx * k, 0.0, w, cy - ry * k, w, cy);
            path.cubic_to(w, cy + ry * k, cx + rx * k, h, cx, h);
            path.cubic_to(cx - rx * k, h, 0.0, cy + ry * k, 0.0, cy);
            path.close();
        }
        Preset::Triangle => {
            let ax = adj_or(adj, 0, 50000) * w;
            path.move_to(ax, 0.0);
            path.line_to(w, h);
            path.line_to(0.0, h);
            path.close();
        }
        Preset::RtTriangle => {
            path.move_to(0.0, 0.0);
            path.line_to(0.0, h);
            path.line_to(w, h);
            path.close();
        }
        Preset::Diamond => {
            path.move_to(w / 2.0, 0.0);
            path.line_to(w, h / 2.0);
            path.line_to(w / 2.0, h);
            path.line_to(0.0, h / 2.0);
            path.close();
        }
        Preset::Parallelogram => {
            let a = (adj_or(adj, 0, 25000) * w).min(w);
            path.move_to(a, 0.0);
            path.line_to(w, 0.0);
            path.line_to(w - a, h);
            path.line_to(0.0, h);
            path.close();
        }
        Preset::Trapezoid => {
            let a = (adj_or(adj, 0, 25000) * ss).min(w / 2.0);
            path.move_to(a, 0.0);
            path.line_to(w - a, 0.0);
            path.line_to(w, h);
            path.line_to(0.0, h);
            path.close();
        }
        Preset::Pentagon => {
            let (cx, cy, rx, ry) = (w / 2.0, h / 2.0, w / 2.0, h / 2.0);
            for (i, ang) in [-90.0_f32, -18.0, 54.0, 126.0, 198.0].iter().enumerate() {
                let a = ang * std::f32::consts::PI / 180.0;
                let (x, y) = (cx + rx * a.cos(), cy + ry * a.sin());
                if i == 0 {
                    path.move_to(x, y);
                } else {
                    path.line_to(x, y);
                }
            }
            path.close();
        }
        Preset::Hexagon => {
            let a = (adj_or(adj, 0, 25000) * ss).min(w / 2.0);
            path.move_to(a, 0.0);
            path.line_to(w - a, 0.0);
            path.line_to(w, h / 2.0);
            path.line_to(w - a, h);
            path.line_to(a, h);
            path.line_to(0.0, h / 2.0);
            path.close();
        }
        Preset::RightArrow => {
            let th = (adj_or(adj, 0, 50000) * h).min(h);
            let head = (adj_or(adj, 1, 50000) * ss).min(w);
            let (y0, y1, neck) = ((h - th) / 2.0, (h + th) / 2.0, w - head);
            path.move_to(0.0, y0);
            path.line_to(neck, y0);
            path.line_to(neck, 0.0);
            path.line_to(w, h / 2.0);
            path.line_to(neck, h);
            path.line_to(neck, y1);
            path.line_to(0.0, y1);
            path.close();
        }
        Preset::LeftArrow => {
            let th = (adj_or(adj, 0, 50000) * h).min(h);
            let head = (adj_or(adj, 1, 50000) * ss).min(w);
            let (y0, y1, neck) = ((h - th) / 2.0, (h + th) / 2.0, head);
            path.move_to(w, y0);
            path.line_to(neck, y0);
            path.line_to(neck, 0.0);
            path.line_to(0.0, h / 2.0);
            path.line_to(neck, h);
            path.line_to(neck, y1);
            path.line_to(w, y1);
            path.close();
        }
    }
    path
}
