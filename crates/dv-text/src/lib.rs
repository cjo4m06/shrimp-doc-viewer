//! The shared text & font stack.
//!
//! Two responsibilities, both behind a small surface so the rest of the engine
//! never talks to a shaper/parser directly:
//!
//! * [`shape`] — turn a string + font into positioned glyphs. M1 uses
//!   `rustybuzz`; this is the *only* place that knows it, so migrating to
//!   `harfrust` later is a one-function change. CJK / 繁體中文 goes through the
//!   same path (Han shaping is largely 1:1 cmap, so once Latin shaping works,
//!   Han works given a font that has the glyphs).
//! * [`outline_glyph`] — turn a glyph id into a font-unit outline path via
//!   `skrifa`, ready to be scaled and filled by the raster backend.

use dv_ir::PathData;

/// Owns the raw bytes of a single font face. Cheap to construct; the actual
/// parser faces are built on demand (M1 keeps it simple — caching comes later).
#[derive(Clone)]
pub struct FontData {
    bytes: Vec<u8>,
    units_per_em: f32,
}

impl FontData {
    pub fn new(bytes: Vec<u8>) -> Self {
        let units_per_em = rustybuzz::Face::from_slice(&bytes, 0)
            .map(|f| f.units_per_em() as f32)
            .unwrap_or(1000.0);
        Self { bytes, units_per_em }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Font design units per em (the outline coordinate space). Usually 1000 (CFF) or 2048 (TTF).
    pub fn units_per_em(&self) -> f32 {
        self.units_per_em
    }

    /// Whether the font's cmap maps this character to a real glyph. Cheap (a cmap
    /// lookup, no shaping); used to fall back when a chosen font lacks a glyph.
    pub fn has_char(&self, ch: char) -> bool {
        rustybuzz::Face::from_slice(&self.bytes, 0)
            .and_then(|f| f.glyph_index(ch))
            .is_some()
    }

    /// The set of Unicode code points the font covers (cmap). Built once so a
    /// font-selection layer can do pure set lookups instead of per-char shaping.
    pub fn coverage(&self) -> std::collections::HashSet<u32> {
        let mut set = std::collections::HashSet::new();
        if let Some(face) = rustybuzz::Face::from_slice(&self.bytes, 0) {
            if let Some(cmap) = face.tables().cmap {
                for sub in cmap.subtables {
                    if sub.is_unicode() {
                        sub.codepoints(|cp: u32| {
                            set.insert(cp);
                        });
                    }
                }
            }
        }
        set
    }
}

/// CJK ideographs / kana / hangul / fullwidth forms (eastAsia face territory).
pub fn is_cjk(ch: char) -> bool {
    let c = ch as u32;
    (0x2E80..=0x9FFF).contains(&c)
        || (0xAC00..=0xD7A3).contains(&c)
        || (0xF900..=0xFAFF).contains(&c)
        || (0xFF00..=0xFFEF).contains(&c)
}

/// Standalone symbol/dingbat characters (bullets, boxes, arrows, checkboxes) that
/// a dedicated symbol font renders better than a text font.
pub fn is_symbol(ch: char) -> bool {
    let c = ch as u32;
    matches!(c,
        0x2022..=0x2023 | 0x2043 | 0x204C..=0x204D
        | 0x2190..=0x21FF // arrows
        | 0x2460..=0x24FF // enclosed alphanumerics
        | 0x25A0..=0x25FF // geometric shapes ■□●○◆◇▪▶
        | 0x2600..=0x27BF // misc symbols + dingbats + checkboxes ☐☑
        | 0x2B00..=0x2BFF)
}

/// Multiple loaded fonts plus selection by a run's declared family name and the
/// character's script. Index 0 is the default (covers CJK + Latin via Noto);
/// embedded / caller-provided fonts are appended and matched by declared name.
pub struct Fonts {
    list: Vec<FontData>,
    cover: Vec<std::collections::HashSet<u32>>, // parallel to list: covered code points
    by_name: std::collections::HashMap<String, usize>,
    cjk: usize,
    latin: usize,
    symbol: usize,
}

impl Fonts {
    pub fn new(default: FontData, extras: Vec<(String, FontData)>) -> Fonts {
        let mut list = vec![default];
        let mut by_name = std::collections::HashMap::new();
        let mut symbol = 0usize;
        for (name, fd) in extras {
            let idx = list.len();
            list.push(fd);
            let low = name.to_lowercase();
            if low.contains("symbol") || low.contains("wingding") || low.contains("dingbat") || low.contains("webding") {
                symbol = idx;
            }
            by_name.entry(low).or_insert(idx);
        }
        let cover = list.iter().map(|f| f.coverage()).collect();
        Fonts { list, cover, by_name, cjk: 0, latin: 0, symbol }
    }
    pub fn covers(&self, i: usize, ch: char) -> bool {
        ch.is_whitespace() || self.cover.get(i).map(|s| s.contains(&(ch as u32))).unwrap_or(false)
    }
    /// Pick the font index for one character: the run's declared face if it has the
    /// glyph, else the script default, else any loaded font that covers it.
    pub fn idx_for(&self, ascii: Option<&str>, ea: Option<&str>, ch: char) -> usize {
        let declared = if is_cjk(ch) { ea.or(ascii) } else { ascii.or(ea) };
        if let Some(name) = declared {
            if let Some(&i) = self.by_name.get(&name.to_lowercase()) {
                if self.covers(i, ch) {
                    return i;
                }
            }
        }
        let fb = if is_symbol(ch) {
            self.symbol
        } else if is_cjk(ch) {
            self.cjk
        } else {
            self.latin
        };
        if self.covers(fb, ch) {
            return fb;
        }
        (0..self.list.len()).find(|&i| self.covers(i, ch)).unwrap_or(fb)
    }
    pub fn get(&self, i: usize) -> &FontData {
        &self.list[i.min(self.list.len().saturating_sub(1))]
    }
    /// The loaded fonts, indexed by the FontId used in emitted glyph runs.
    pub fn data(&self) -> &[FontData] {
        &self.list
    }
}

/// One shaped glyph. Advances/offsets are in **font design units** (scale by
/// `size / units_per_em` to get px).
#[derive(Clone, Copy, Debug)]
pub struct ShapedGlyph {
    pub glyph_id: u32,
    /// Byte index into the source string this glyph belongs to.
    pub cluster: u32,
    pub x_advance: f32,
    pub y_advance: f32,
    pub x_offset: f32,
    pub y_offset: f32,
}

/// Result of shaping a run of text in a single font.
pub struct ShapedRun {
    pub glyphs: Vec<ShapedGlyph>,
    pub units_per_em: f32,
}

/// Shape `text` with `font`. Direction/script/language are auto-guessed from the
/// text (sufficient for M1; the frontend will pass explicit properties later).
///
/// `_size` is currently unused because advances are returned in font units; it
/// is kept in the signature so callers express intent and future hinting can use it.
pub fn shape(font: &FontData, text: &str, _size: f32) -> ShapedRun {
    let face = match rustybuzz::Face::from_slice(&font.bytes, 0) {
        Some(f) => f,
        None => {
            return ShapedRun { glyphs: Vec::new(), units_per_em: font.units_per_em };
        }
    };

    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();

    let output = rustybuzz::shape(&face, &[], buffer);
    let infos = output.glyph_infos();
    let positions = output.glyph_positions();

    let mut glyphs = Vec::with_capacity(infos.len());
    for (info, pos) in infos.iter().zip(positions.iter()) {
        glyphs.push(ShapedGlyph {
            glyph_id: info.glyph_id,
            cluster: info.cluster,
            x_advance: pos.x_advance as f32,
            y_advance: pos.y_advance as f32,
            x_offset: pos.x_offset as f32,
            y_offset: pos.y_offset as f32,
        });
    }

    ShapedRun { glyphs, units_per_em: face.units_per_em() as f32 }
}

/// Extract the outline of a single glyph, in **font design units** with the
/// font's native y-up orientation. The backend scales by `size/units_per_em`
/// and flips y when placing it on the page.
pub fn outline_glyph(font: &FontData, glyph_id: u32) -> PathData {
    use skrifa::outline::{DrawSettings, OutlinePen};
    use skrifa::instance::{LocationRef, Size};
    use skrifa::{FontRef, GlyphId, MetadataProvider};

    let mut path = PathData::new();

    let font_ref = match FontRef::from_index(&font.bytes, 0) {
        Ok(f) => f,
        Err(_) => return path,
    };

    let outlines = font_ref.outline_glyphs();
    let glyph = match outlines.get(GlyphId::new(glyph_id)) {
        Some(g) => g,
        None => return path,
    };

    struct Pen<'a> {
        path: &'a mut PathData,
    }

    impl OutlinePen for Pen<'_> {
        fn move_to(&mut self, x: f32, y: f32) {
            self.path.move_to(x, y);
        }
        fn line_to(&mut self, x: f32, y: f32) {
            self.path.line_to(x, y);
        }
        fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
            self.path.quad_to(cx, cy, x, y);
        }
        fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
            self.path.cubic_to(c1x, c1y, c2x, c2y, x, y);
        }
        fn close(&mut self) {
            self.path.close();
        }
    }

    let settings = DrawSettings::unhinted(Size::unscaled(), LocationRef::default());
    let mut pen = Pen { path: &mut path };
    let _ = glyph.draw(settings, &mut pen);

    path
}
