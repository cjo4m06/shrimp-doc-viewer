//! A lightweight rich-text **flow** renderer shared by the markdown/plain-text,
//! RTF and ODT-text frontends. Callers build a `Vec<Block>` of styled content;
//! `FlowDoc` lays it out into A4-ish pages (wrap, CJK+Latin, multi-font) and paints
//! one page at a time into the shared display list, so it plugs into the same
//! virtualized page viewer as DOCX.

use dv_ir::{Color, Command, DisplayList, FillRule, FontId, GlyphRun, Paint, PathData, PositionedGlyph, Transform};
use dv_text::{is_cjk, shape, Fonts};

/// An inline styled text span.
#[derive(Clone, Default)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
    pub strike: bool,
    pub underline: bool,
    pub color: Option<Color>,
    /// Explicit size in px (overrides the block default).
    pub size: Option<f32>,
}

impl Span {
    pub fn new(text: impl Into<String>) -> Span {
        Span { text: text.into(), ..Default::default() }
    }
}

/// A block-level element.
pub enum Block {
    /// Heading, level 1..=6.
    Heading(u8, Vec<Span>),
    Para(Vec<Span>),
    /// List item: indent level (0-based), ordered marker text (e.g. "1.") or bullet.
    ListItem { level: u8, marker: String, spans: Vec<Span> },
    /// Pre-formatted code block (each entry is a line; rendered monospace on a tint).
    Code(Vec<String>),
    /// Block quote (rendered indented with a left bar).
    Quote(Vec<Span>),
    /// Horizontal rule.
    Rule,
}

const PAGE_W: f32 = 816.0;
const PAGE_H: f32 = 1056.0;
const MARGIN: f32 = 72.0;
const BODY_SIZE: f32 = 15.0;
const CODE_TINT: Color = Color { r: 0xF2, g: 0xF3, b: 0xF5, a: 255 };
const QUOTE_BAR: Color = Color { r: 0xC8, g: 0xCC, b: 0xD2, a: 255 };
const QUOTE_TEXT: Color = Color { r: 0x55, g: 0x59, b: 0x60, a: 255 };
const RULE_COLOR: Color = Color { r: 0xD0, g: 0xD3, b: 0xD8, a: 255 };

/// One positioned glyph at zoom 1 (page-relative).
#[derive(Clone, Copy)]
struct G {
    id: u32,
    x: f32,
    baseline: f32,
    size: f32,
    color: Color,
    bold: bool,
    font: usize,
}

/// A decoration rect (fills + rules + underlines), page-relative at zoom 1.
#[derive(Clone, Copy)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: Color,
}

pub struct FlowDoc {
    fonts: Fonts,
    glyphs: Vec<G>,
    rects: Vec<Rect>,
    total_h: f32,
}

impl FlowDoc {
    /// Lay out the blocks into pages using `fonts` (index 0 = default face).
    pub fn new(blocks: &[Block], fonts: Fonts) -> FlowDoc {
        let mut lay = Layout { fonts: &fonts, glyphs: Vec::new(), rects: Vec::new(), y: MARGIN };
        for b in blocks {
            lay.block(b);
        }
        let total_h = lay.y + MARGIN;
        let glyphs = lay.glyphs;
        let rects = lay.rects;
        FlowDoc { fonts, glyphs, rects, total_h }
    }

    pub fn page_count(&self) -> usize {
        ((self.total_h / PAGE_H).ceil() as usize).max(1)
    }

    /// `[width, height]` of a page in base (zoom=1) px.
    pub fn page_size(&self) -> (f32, f32) {
        (PAGE_W, PAGE_H)
    }

    /// Render page `idx` at `scale` (= zoom × dpr).
    pub fn render_page(&self, idx: usize, scale: f32) -> DisplayList {
        let mut dl = DisplayList::new(PAGE_W * scale, PAGE_H * scale);
        // white page
        dl.push(fill(0.0, 0.0, PAGE_W * scale, PAGE_H * scale, Color { r: 255, g: 255, b: 255, a: 255 }));
        let top = idx as f32 * PAGE_H;
        let bot = top + PAGE_H;

        for r in &self.rects {
            if r.y + r.h < top || r.y > bot {
                continue;
            }
            dl.push(fill(r.x * scale, (r.y - top) * scale, r.w * scale, r.h * scale, r.color));
        }

        // glyphs grouped by (size, color, bold, font)
        let mut i = 0;
        let mut on_page: Vec<&G> = self.glyphs.iter().filter(|g| g.baseline >= top - 4.0 && g.baseline <= bot + 4.0).collect();
        on_page.sort_by(|a, b| (a.size as i32, a.color.r, a.color.g, a.color.b, a.bold as i32, a.font).cmp(&(b.size as i32, b.color.r, b.color.g, b.color.b, b.bold as i32, b.font)).then(a.baseline.partial_cmp(&b.baseline).unwrap()));
        while i < on_page.len() {
            let g0 = on_page[i];
            let mut run = Vec::new();
            while i < on_page.len()
                && on_page[i].size == g0.size
                && on_page[i].color == g0.color
                && on_page[i].bold == g0.bold
                && on_page[i].font == g0.font
            {
                let g = on_page[i];
                run.push(PositionedGlyph { id: g.id, x: g.x * scale, y: (g.baseline - top) * scale });
                i += 1;
            }
            dl.push(Command::Glyphs(GlyphRun { font: FontId(g0.font as u32), size: g0.size * scale, paint: Paint::Solid(g0.color), bold: g0.bold, glyphs: run }));
        }
        dl
    }

    pub fn fonts(&self) -> &Fonts {
        &self.fonts
    }
}

fn fill(x: f32, y: f32, w: f32, h: f32, color: Color) -> Command {
    let mut p = PathData::new();
    p.move_to(x, y);
    p.line_to(x + w, y);
    p.line_to(x + w, y + h);
    p.line_to(x, y + h);
    p.close();
    Command::FillPath { path: p, paint: Paint::Solid(color), fill_rule: FillRule::NonZero, transform: Transform::IDENTITY }
}

struct Layout<'a> {
    fonts: &'a Fonts,
    glyphs: Vec<G>,
    rects: Vec<Rect>,
    y: f32,
}

/// A laid-out inline glyph candidate (before line breaking).
struct Tok {
    id: u32,
    advance: f32,
    size: f32,
    color: Color,
    bold: bool,
    underline: bool,
    strike: bool,
    font: usize,
    break_ok: bool, // a break opportunity may follow this glyph
    is_space: bool,
}

impl Layout<'_> {
    fn block(&mut self, b: &Block) {
        match b {
            Block::Heading(level, spans) => {
                let size = match level {
                    1 => 30.0,
                    2 => 24.0,
                    3 => 20.0,
                    4 => 17.0,
                    5 => 15.0,
                    _ => 14.0,
                };
                self.y += if *level <= 2 { 18.0 } else { 12.0 };
                self.flow(spans, MARGIN, PAGE_W - MARGIN, size, true, None, false);
                self.y += 6.0;
            }
            Block::Para(spans) => {
                self.flow(spans, MARGIN, PAGE_W - MARGIN, BODY_SIZE, false, None, false);
                self.y += BODY_SIZE * 0.6;
            }
            Block::ListItem { level, marker, spans } => {
                let indent = MARGIN + 18.0 + (*level as f32) * 22.0;
                // marker hung to the left of the text
                let m: Vec<Span> = vec![Span { text: marker.clone(), ..Default::default() }];
                let y0 = self.y;
                self.flow(&m, indent - 18.0, indent - 2.0, BODY_SIZE, false, None, true);
                self.y = y0; // marker shares the first line
                self.flow(spans, indent, PAGE_W - MARGIN, BODY_SIZE, false, None, false);
                self.y += BODY_SIZE * 0.35;
            }
            Block::Code(lines) => {
                let pad = 8.0;
                let lh = BODY_SIZE * 1.45;
                let h = lines.len() as f32 * lh + pad * 2.0;
                self.rects.push(Rect { x: MARGIN, y: self.y, w: PAGE_W - 2.0 * MARGIN, h, color: CODE_TINT });
                self.y += pad;
                for ln in lines {
                    let s = vec![Span { text: ln.clone(), mono: true, color: Some(Color { r: 0x2a, g: 0x2d, b: 0x33, a: 255 }), ..Default::default() }];
                    self.flow_fixed(&s, MARGIN + pad, PAGE_W - MARGIN - pad, BODY_SIZE * 0.92, lh);
                }
                self.y += pad + BODY_SIZE * 0.4;
            }
            Block::Quote(spans) => {
                let y0 = self.y + 2.0;
                self.flow(spans, MARGIN + 20.0, PAGE_W - MARGIN, BODY_SIZE, false, Some(QUOTE_TEXT), false);
                self.rects.push(Rect { x: MARGIN + 6.0, y: y0, w: 3.0, h: (self.y - y0 - 4.0).max(BODY_SIZE), color: QUOTE_BAR });
                self.y += BODY_SIZE * 0.5;
            }
            Block::Rule => {
                self.y += 10.0;
                self.rects.push(Rect { x: MARGIN, y: self.y, w: PAGE_W - 2.0 * MARGIN, h: 1.0, color: RULE_COLOR });
                self.y += 12.0;
            }
        }
    }

    /// Shape spans into tokens, wrap within [left,right], emit glyph rows. When
    /// `force_color` is set it overrides span colours (e.g. quote grey).
    fn flow(&mut self, spans: &[Span], left: f32, right: f32, base_size: f32, bold_all: bool, force_color: Option<Color>, single_line: bool) {
        let toks = self.tokens(spans, base_size, bold_all, force_color);
        let width = (right - left).max(40.0);
        let line_h = base_size * 1.5;
        let mut line: Vec<Tok> = Vec::new();
        let mut line_w = 0.0f32;
        let emit = |this: &mut Self, line: &mut Vec<Tok>| {
            if line.is_empty() {
                this.y += line_h;
                return;
            }
            let baseline = this.y + base_size * 0.95;
            let mut x = left;
            for t in line.iter() {
                if !t.is_space || x > left {
                    this.glyphs.push(G { id: t.id, x, baseline, size: t.size, color: t.color, bold: t.bold, font: t.font });
                    if t.underline {
                        this.rects.push(Rect { x, y: baseline + t.size * 0.12, w: t.advance, h: (t.size * 0.06).max(1.0), color: t.color });
                    }
                    if t.strike {
                        this.rects.push(Rect { x, y: baseline - t.size * 0.28, w: t.advance, h: (t.size * 0.06).max(1.0), color: t.color });
                    }
                }
                x += t.advance;
            }
            this.y += line_h;
            line.clear();
        };
        for t in toks {
            if !line.is_empty() && line_w + t.advance > width && !single_line {
                // break at the last break-opportunity
                if let Some(bi) = line.iter().rposition(|x| x.break_ok) {
                    let rest = line.split_off(bi + 1);
                    emit(self, &mut line);
                    line = rest;
                    line_w = line.iter().map(|x| x.advance).sum();
                } else {
                    emit(self, &mut line);
                    line_w = 0.0;
                }
            }
            line_w += t.advance;
            line.push(t);
        }
        emit(self, &mut line);
    }

    /// Emit a single fixed-height row (no wrapping), used for code lines.
    fn flow_fixed(&mut self, spans: &[Span], left: f32, _right: f32, size: f32, line_h: f32) {
        let toks = self.tokens(spans, size, false, None);
        let baseline = self.y + size * 0.95;
        let mut x = left;
        for t in &toks {
            self.glyphs.push(G { id: t.id, x, baseline, size: t.size, color: t.color, bold: t.bold, font: t.font });
            x += t.advance;
        }
        self.y += line_h;
    }

    fn tokens(&self, spans: &[Span], base_size: f32, bold_all: bool, force_color: Option<Color>) -> Vec<Tok> {
        let mut out = Vec::new();
        for sp in spans {
            let size = sp.size.unwrap_or(base_size);
            let bold = sp.bold || bold_all;
            let color = force_color.or(sp.color).unwrap_or(Color::BLACK);
            // pick font per char: mono -> declared "monospace"/symbol fallback; else default
            let ea = if sp.mono { Some("monospace") } else { None };
            let chars: Vec<char> = sp.text.chars().collect();
            let mut i = 0;
            while i < chars.len() {
                let fi = self.fonts.idx_for(ea, ea, chars[i]);
                let mut seg = String::new();
                while i < chars.len() && self.fonts.idx_for(ea, ea, chars[i]) == fi {
                    seg.push(chars[i]);
                    i += 1;
                }
                let shaped = shape(self.fonts.get(fi), &seg, size);
                let s = size / shaped.units_per_em.max(1.0);
                for g in &shaped.glyphs {
                    let ch = seg.get(g.cluster as usize..).and_then(|x| x.chars().next()).unwrap_or(' ');
                    out.push(Tok {
                        id: g.glyph_id,
                        advance: g.x_advance * s,
                        size,
                        color,
                        bold,
                        underline: sp.underline,
                        strike: sp.strike,
                        font: fi,
                        break_ok: ch.is_whitespace() || is_cjk(ch),
                        is_space: ch.is_whitespace(),
                    });
                }
            }
        }
        out
    }
}
