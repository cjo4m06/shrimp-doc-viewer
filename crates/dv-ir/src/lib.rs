//! Backend-agnostic display list IR.
//!
//! This is the architectural keystone: every format frontend (PDF/DOCX/XLSX/PPTX)
//! *lowers* its content into a [`DisplayList`], and every raster backend
//! (tiny-skia today, vello later) *consumes* one. Text is represented as
//! pre-positioned [`GlyphRun`]s — layout/shaping happens in the frontend, the
//! backend only paints positioned glyphs. Keep this crate dependency-free.

/// Straight (un-premultiplied) 8-bit RGBA colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const BLACK: Color = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };
    pub const WHITE: Color = Color {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
        Color { r, g, b, a }
    }

    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b, a: 255 }
    }
}

/// 2D affine transform. Layout matches SVG/Canvas `matrix(sx, ky, kx, sy, tx, ty)`:
/// `x' = sx*x + kx*y + tx`, `y' = ky*x + sy*y + ty`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    pub sx: f32,
    pub ky: f32,
    pub kx: f32,
    pub sy: f32,
    pub tx: f32,
    pub ty: f32,
}

impl Transform {
    pub const IDENTITY: Transform = Transform {
        sx: 1.0,
        ky: 0.0,
        kx: 0.0,
        sy: 1.0,
        tx: 0.0,
        ty: 0.0,
    };

    pub const fn translate(tx: f32, ty: f32) -> Transform {
        Transform {
            sx: 1.0,
            ky: 0.0,
            kx: 0.0,
            sy: 1.0,
            tx,
            ty,
        }
    }

    pub const fn scale(sx: f32, sy: f32) -> Transform {
        Transform {
            sx,
            ky: 0.0,
            kx: 0.0,
            sy,
            tx: 0.0,
            ty: 0.0,
        }
    }
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillRule {
    /// Non-zero winding (default for most vector content).
    NonZero,
    /// Even-odd (used by PDF `f*`/`B*` and some glyph outlines).
    EvenOdd,
}

/// A single path segment. Coordinates are in the command's local space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathVerb {
    MoveTo(f32, f32),
    LineTo(f32, f32),
    QuadTo(f32, f32, f32, f32),
    CubicTo(f32, f32, f32, f32, f32, f32),
    Close,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PathData {
    pub verbs: Vec<PathVerb>,
}

impl PathData {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn move_to(&mut self, x: f32, y: f32) {
        self.verbs.push(PathVerb::MoveTo(x, y));
    }

    pub fn line_to(&mut self, x: f32, y: f32) {
        self.verbs.push(PathVerb::LineTo(x, y));
    }

    pub fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.verbs.push(PathVerb::QuadTo(cx, cy, x, y));
    }

    pub fn cubic_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        self.verbs.push(PathVerb::CubicTo(c1x, c1y, c2x, c2y, x, y));
    }

    pub fn close(&mut self) {
        self.verbs.push(PathVerb::Close);
    }

    pub fn is_empty(&self) -> bool {
        self.verbs.is_empty()
    }
}

/// How a shape is filled. M1 ships solid colour; gradients/patterns/images come later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Paint {
    Solid(Color),
}

/// Index into the render-time font registry. Frontends assign these; the backend
/// resolves them to actual font bytes when outlining glyphs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FontId(pub u32);

/// One glyph placed at its final position (device-independent units), baseline-relative.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PositionedGlyph {
    pub id: u32,
    pub x: f32,
    pub y: f32,
}

/// A run of positioned glyphs sharing a font, size and paint.
#[derive(Clone, Debug, PartialEq)]
pub struct GlyphRun {
    pub font: FontId,
    /// Em size in the same units as the display list (px).
    pub size: f32,
    pub paint: Paint,
    /// Faux-bold: the backend strokes glyph outlines (used when no bold face is
    /// loaded). Real bold should select a bold font instead, when available.
    pub bold: bool,
    pub glyphs: Vec<PositionedGlyph>,
}

/// One paint operation. The list is painted in order (painter's algorithm).
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    FillPath {
        path: PathData,
        paint: Paint,
        fill_rule: FillRule,
        transform: Transform,
    },
    StrokePath {
        path: PathData,
        paint: Paint,
        width: f32,
        transform: Transform,
    },
    Glyphs(GlyphRun),
    /// Blit a raster image (straight/un-premultiplied RGBA, row-major,
    /// `src_w*src_h*4` bytes) into the destination rect `(x, y, w, h)`.
    Image {
        rgba: Vec<u8>,
        src_w: u32,
        src_h: u32,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        /// Optional clip path in device coords (e.g. an ellipse crop). `None` = rect.
        clip: Option<PathData>,
    },
}

/// A page/scene to paint. Width/height are the device-independent canvas size (px).
#[derive(Clone, Debug, Default)]
pub struct DisplayList {
    pub width: f32,
    pub height: f32,
    pub commands: Vec<Command>,
}

impl DisplayList {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            width,
            height,
            commands: Vec::new(),
        }
    }

    pub fn push(&mut self, command: Command) {
        self.commands.push(command);
    }
}
