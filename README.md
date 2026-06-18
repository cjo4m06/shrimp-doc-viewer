# doc-viewer

A browser, **viewer-only**, high-fidelity multi-format document viewer (PDF / Word
/ Excel / PowerPoint). The rendering core is written in **Rust** and compiled to
**WebAssembly**; it is shipped as a **pure-JS npm library** (`await init()` →
`mount(...)`). No server, no editing, no plugins to install in the page.

> Status (verified in-browser):
> - **PDF — complete.** PDFium-WASM via `mount()`; embedded + non-embedded CJK,
>   Web Worker, virtualized + zoomable viewer.
> - **XLSX — complete.** Self-written Rust grid: widths/heights/merges, styles,
>   number formats, **sheet tabs + viewport virtualization + frozen headers + zoom**.
> - **DOCX — basic.** Self-written flow layout (paragraphs, runs, bold/size/colour,
>   alignment, CJK+Latin wrapping). One continuous page; no pagination/zoom yet.
> - **PPTX — basic.** Self-written DrawingML: positioned text boxes + solid-fill
>   shapes, run formatting, slide navigation. No preset geometry/images/theme yet.
>
> All three working formats render Latin **and 繁體中文** through one shared geba
> (display-list IR + skrifa/rustybuzz text stack + tiny-skia raster).

## Why this shape (the honest tradeoff)

You cannot have **own-source + all four formats + pixel-perfect fidelity, all at
once** — those goals are in tension, and pretending otherwise guarantees failure
(PDFium / LibreOffice are decades of work). The design instead makes a deliberate
per-format choice, optimized for: free commercial use · healthy/maintained deps ·
small payload · responsiveness (即時).

| Format | Strategy | Achievable fidelity |
| --- | --- | --- |
| **PDF** | Embed **PDFium-WASM** (BSD, Chrome's engine — healthiest possible, ~few MB, ms-fast, mature CJK fallback) | **High** |
| **XLSX / DOCX / PPTX** | **Own Rust renderers** over healthy parsers (`calamine`/`quick-xml`/`zip`); no healthy render lib exists, so we write it | 80–90% on typical docs (never pixel-parity with Office) |
| Legacy `.doc/.xls/.ppt`, MS-parity | Only LibreOffice-WASM delivers it (~250MB, slow boot) — **dropped** on size/speed; defer or server-side convert | — |

The **shared geba is the real asset**: every format lowers into one display-list and
is painted by one backend, so the text/font/raster stack is built once and reused.

## Architecture

```
crates/
  dv-ir       backend-agnostic display-list IR (paths, glyph runs, paint) — no deps
  dv-text     text/font stack: shaping (rustybuzz, behind an abstraction) + outlines (skrifa)
  dv-render   tiny-skia CPU raster backend: DisplayList -> straight RGBA
  dv-wasm     wasm-bindgen bindings -> the WASM core
packages/
  doc-viewer  the npm package: pure-JS API (init/mount) + generated wasm glue
examples/
  browser     M1 demo page (renders 繁體中文 through the WASM core)
```

Frontends (PDF/DOCX/XLSX/PPTX) lower into `dv-ir::DisplayList`; the backend only
paints. Text is represented as **pre-positioned glyph runs** — layout/shaping live in
the frontend, the backend rasterizes outlines. tiny-skia is the mandatory CPU
baseline (works in every browser/Worker, no WebGPU dependency); a `vello`/WebGPU
backend can slot in later behind a capability check.

### Notable decisions
- **Rust**, not Go/C: no GC runtime (smallest wasm), best JS interop (auto `.d.ts`),
  and the whole high-fidelity stack (skrifa/tiny-skia/calamine) is best-in-class in Rust.
- **Single-threaded** mandatory path: wasm threads need `SharedArrayBuffer` →
  COOP/COEP headers, which an npm library can't impose on consumers.
- **Fonts are lazy-loaded assets**, never bundled into the wasm. A full 繁中 font is
  5–20 MB (larger than the core itself); it is fetched/subset and browser-cached.
- Shaping is isolated in `dv-text::shape` (M1: `rustybuzz`) so migrating to
  `harfrust` later is a one-function change.

## Build & run

Prerequisites: Rust (stable) + `wasm32-unknown-unknown` target, `wasm-bindgen-cli`
(matching the `wasm-bindgen` crate version, currently `0.2.125`), Node, and
optionally `wasm-opt` (binaryen) for size.

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --locked

# Build the WASM core + JS glue into packages/doc-viewer/wasm
./scripts/build-wasm.sh

# Serve the repo root and open the M1 demo
python3 -m http.server 8123
# http://localhost:8123/examples/browser/index.html
```

Native pipeline smoke test (renders to `demo.png`, no browser needed):

```bash
cargo run -p dv-render --example demo -- <font.ttf|otf> "Hello,你好，繁體中文"
```

## API (current)

```js
import { init, renderToCanvas, mount } from "doc-viewer";

await init();                                  // load+instantiate the wasm core once
await renderToCanvas(canvas, {                 // M1 geba demo (Rust/tiny-skia text)
  fontUrl: "/fonts/NotoSansTC.ttf",
  text: "你好，繁體中文",
  size: 64,
});

// M2: render a PDF (PDFium-WASM) into a container. PDF/OOXML/OLE are sniffed by
// magic bytes; only PDF is wired so far.
const viewer = await mount(document.getElementById("doc"), pdfBytesOrBlobOrUrl, {
  pdfiumWasmUrl: "/path/to/pdfium.wasm",        // optional; defaults to embedpdf CDN
  cjkFallbackFontUrl: "/fonts/NotoSansTC.ttf",  // so non-embedded zh-TW PDFs render
});
console.log(viewer.pageCount);
viewer.zoomIn();          // also zoomOut(), setZoom(1.5), fitWidth(); Ctrl/⌘-wheel works too
// viewer.destroy() when done (PDFium has no GC)
```

> **繁中 in PDF — handled.** Pass `cjkFallbackFontUrl` (a Noto Sans/Serif CJK TC
> font) and `mount()` installs it into PDFium via `FPDF_SetSystemFontInfo`, so PDFs
> that reference a **non-embedded** CJK font (Adobe-CNS1 / predefined-CMap, the
> common zh-TW case) render instead of going blank. Verified before/after on a
> non-embedded `MingLiU`/`UniCNS-UCS2-H` PDF. **Inherent limit:** an `Identity-H`
> PDF that names a specific proprietary font (e.g. MingLiU) without embedding it
> uses that font's own glyph ids — only the original font reproduces it exactly; a
> generic fallback can't. Embedded-font PDFs are unaffected (no regression).

## Roadmap

- **M1 ✅** — shared geba + WASM + JS API + browser demo (Latin + 繁體中文 verified).
- **M2 ✅** — PDF via PDFium-WASM behind `mount()` (`@embedpdf/pdfium`, BSD-3,
  single-threaded — no COOP/COEP needed). Verified rendering an embedded-font 繁中
  PDF at full fidelity. **Ship-worthy on its own.**
- **M2.5 ✅ — PDF complete (core).** Non-embedded zh-TW PDFs render via
  `FPDF_SetSystemFontInfo` + a Noto Sans TC fallback (built from JS with Emscripten
  `addFunction`/`setValue`); verified before/after, no regression on embedded PDFs.
  Rendering runs in a **Web Worker** (PDFium off the main thread, pages streamed
  back as `ImageBitmap`s; main-thread fallback). Encrypted/corrupt PDFs surface a
  typed `PdfOpenError` (+ `password` option). Rust core: dropped the unused `png`
  encoder feature + `wasm-opt -Oz` (1.21 MB → 1.10 MB raw, ~430 KB gzip).
- **M2.6 ✅ — virtualized, zoomable viewer.** `mount()` returns a stateful
  `PdfViewer`: one placeholder per page (sized from page dimensions, so the
  scrollbar is correct without rendering), only pages near the viewport render,
  and pages that scroll away are freed — constant memory on 100+ page docs.
  `setZoom`/`zoomIn`/`zoomOut`/`fitWidth` + Ctrl/⌘-wheel re-render only the visible
  pages at the new resolution. Verified on a 24-page PDF: 24 placeholders, ~2
  canvases live, eviction on scroll, crisp re-render at 156%.
- **M3 — XLSX viewer (self-written Rust over the shared geba).** The first format
  rendered by our own code, not an embedded engine.
  - **M3.1 ✅** — `dv-xlsx` crate: parse values with `calamine`, lower a sheet into
    the display list (column-letter/row-number headers, grid lines, per-cell text
    shaped + truncated, numbers right-aligned). `mount()` sniffs the OOXML zip and
    routes xlsx → `render_xlsx` (WASM) → tiny-skia. Verified in-browser on a 繁中
    workbook.
  - **M3.2 ✅** — real column widths / row heights, merged cells, alignment.
  - **M3.3 ✅** — styles (fonts/bold/colour, fills, borders) + number formats
    (thousands, currency, percent, dates).
  - **M3.4 ✅** — stateful `XlsxViewer`: **viewport virtualization** (only the
    visible cell window is rendered on scroll), **sheet tabs**, **frozen column/row
    headers**, and **zoom** (buttons + Ctrl/⌘-wheel + fit-width). Verified on a
    2-sheet / 120-row workbook. *Remaining: sheet-defined frozen panes, charts
    (DrawingML), conditional formatting.*
- **M4 — DOCX viewer (self-written flow layout).**
  - **M4.1 ✅** — `dv-docx`: parse paragraphs/runs + page geometry; greedy line
    wrapping (spaces for Latin, any boundary for CJK); bold (faux) / size / colour;
    paragraph alignment. Verified in-browser. *Rendered as one continuous canvas —
    NOT yet paginated/virtualized/zoomable like PDF/XLSX. Remaining: tables, lists,
    images, real pagination, styles.xml inheritance, italic.*
- **M5 — PPTX viewer (self-written DrawingML).**
  - **M5.1 ✅** — `dv-pptx`: parse `presentation.xml` (slide size/order) + slide
    `p:sp` shapes (a:off/a:ext EMU position, solid fills, text bodies with run
    size/bold/colour + alignment); CJK+Latin wrapping per box; `PptxDeck` WASM
    session + `PptxViewer` (slide nav + fit-width). Verified in-browser on a
    2-slide deck. *Remaining: preset shape geometry, images, theme/master
    inheritance, tables, charts.*
- **Cross-cutting** — SSIM screenshot-diff harness to measure fidelity honestly;
  wasm size pass (wasm-opt, drop unused features).

## License

MIT OR Apache-2.0.
