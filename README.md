# doc-viewer

A browser, **viewer-only**, high-fidelity multi-format document viewer (PDF / Word
/ Excel / PowerPoint / CSV). The rendering core is written in **Rust** and compiled
to **WebAssembly**; it is shipped as a **pure-JS npm library** (`await init()` →
`mount(...)`). No server, no editing, no plugins to install in the page.

> Status (verified in-browser):
> - **PDF — complete.** PDFium-WASM via `mount()`; embedded + non-embedded CJK,
>   Web Worker, virtualized + zoomable viewer.
> - **XLSX — complete.** Self-written Rust grid: widths/heights/merges, styles,
>   number formats, **sheet tabs + viewport virtualization + frozen headers + zoom**.
> - **CSV — complete.** RFC-4180 parser (quoted fields, embedded delimiters/newlines,
>   `""` escapes; auto-detects comma / tab / semicolon) → reuses the XLSX grid viewer
>   (virtualization + zoom; numbers right-align, columns auto-size).
> - **DOCX — complete (viewer-grade).** Self-written flow layout: **paginated,
>   virtualized, zoomable** viewer; styles.xml inheritance; lists/numbering;
>   **tables** (borders/shading/gridSpan/**true vMerge spanning**/vAlign); **headers/
>   footers** (titlePg, running logo + separator rules); rich runs (bold/italic/
>   underline/strike/super-subscript/highlight/colour); paragraph spacing, tabs,
>   **explicit + keepLines pagination**, **justify**, borders, shading; inline images;
>   **floating drawings** (anchored images, rounded callouts with pointer tails,
>   straight + custGeom-curve connectors with arrowheads); **multi-font** (see below);
>   CJK+Latin wrapping. Verified against a real 56-page manual.
> - **PPTX — near-complete.** Self-written DrawingML: theme/master/layout colour
>   inheritance, positioned text boxes, **preset + custom geometry + outlines**,
>   solid fills, **raster images**, run formatting, **multi-font**, slide navigation.
>   *Remaining: tables (`a:tbl`).*
>
> All formats render Latin **and 繁體中文** through one shared geba (display-list IR
> + skrifa/rustybuzz text stack + tiny-skia raster).

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
| **CSV** | **Own Rust parser** (RFC 4180) reusing the XLSX grid renderer | **High** |
| Legacy `.doc/.xls/.ppt`, MS-parity | Only LibreOffice-WASM delivers it (~250MB, slow boot) — **dropped** on size/speed; defer or server-side convert | — |

The **shared geba is the real asset**: every format lowers into one display-list and
is painted by one backend, so the text/font/raster stack is built once and reused.

## Architecture

```
crates/
  dv-ir       backend-agnostic display-list IR (paths, glyph runs, paint) — no deps
  dv-text     text/font stack: shaping (rustybuzz) + outlines (skrifa) + multi-font selection
  dv-render   tiny-skia CPU raster backend: DisplayList -> straight RGBA
  dv-image    PNG/JPEG decode -> straight RGBA
  dv-xlsx     XLSX grid model + viewport renderer (also powers CSV via Sheet::from_csv)
  dv-docx     DOCX flow layout + pagination
  dv-pptx     PPTX DrawingML engine
  dv-wasm     wasm-bindgen bindings -> the WASM core
packages/
  doc-viewer  the npm package: pure-JS API (init/mount) + generated wasm glue
examples/
  browser     per-format demo pages (render 繁體中文 through the WASM core)
```

Frontends (PDF/DOCX/XLSX/PPTX/CSV) lower into `dv-ir::DisplayList`; the backend only
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
- **Multi-font selection** (`dv_text::Fonts`, shared by DOCX + PPTX): each run names
  font families (`w:rFonts` / `a:latin`/`a:ea`/`a:cs`); per character we pick the
  declared face if it covers the glyph (coverage precomputed per font), else a
  script default (CJK / Latin / symbol), else any loaded font that covers it — so a
  font that lacks a glyph never shows `.notdef`. Fonts come from three sources, in
  priority: **embedded in the file** (DOCX `word/fonts/*`, de-obfuscating `.odttf`
  with the `w:fontKey` GUID), **caller-provided** via `mount({ fonts: { … } })`, and
  the **default** fallback. *Limit:* proprietary system faces (標楷體, MingLiU…) can't
  be bundled (licensing) and the document only declares them — supply them through
  `fonts`. *Limit:* PowerPoint-embedded fonts are MicroType-Express-compressed EOT
  and are not loadable, so PPTX relies on the caller map + script fallback.

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

// mount() auto-detects the format (PDF / OOXML docx·xlsx·pptx / CSV) from the
// bytes and routes to the right frontend. Pass options once; each frontend uses
// what it needs.
const viewer = await mount(document.getElementById("doc"), bytesOrBlobOrUrl, {
  pdfiumWasmUrl: "/path/to/pdfium.wasm",        // PDF: optional; defaults to embedpdf CDN
  cjkFallbackFontUrl: "/fonts/NotoSansTC.ttf",  // PDF: so non-embedded zh-TW PDFs render
  fontUrl: "/fonts/NotoSansTC.ttf",             // DOCX/XLSX/PPTX/CSV: default/fallback face
  fonts: {                                       // DOCX/PPTX: supply faces the file only declares
    "標楷體": "/fonts/BiauKai.ttf",               //   value: URL | Uint8Array | ArrayBuffer
  },
  // format: "csv",                              // optional: force a format instead of sniffing
});
console.log(viewer.pageCount);
viewer.zoomIn();          // also zoomOut(), setZoom(1.5), fitWidth(); Ctrl/⌘-wheel works too
// viewer.destroy() when done (PDFium has no GC)
```

> **Fonts for self-rendered formats.** DOCX/XLSX/PPTX/CSV are painted by our own
> glyph renderer, so they need at least `fontUrl` (a CJK-capable face, e.g. Noto
> Sans TC). A document declares font families but usually doesn't embed proprietary
> ones (標楷體, MingLiU…); pass those via `fonts` to render them, otherwise text
> falls back to `fontUrl` per script. Embedded fonts in the file (DOCX `.odttf`) are
> loaded automatically.

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
  - **M4.1 ✅** flow layout (paragraphs/runs, bold/size/colour, alignment, CJK+Latin wrapping).
  - **M4.2 ✅** pagination + virtualized, zoomable `DocxViewer` (page geometry from sectPr).
  - **M4.3 ✅** styles.xml inheritance (docDefaults → basedOn chain → direct; Heading/Title).
  - **M4.4 ✅** lists/numbering (numbering.xml, (numId,ilvl) counters, %N, bullet/decimal/letter/roman, hanging indents).
  - **M4.5 ✅** inline images (`w:drawing` → blip → media → decode → Image).
  - **M4.6 ✅** tables (`w:tbl`): Block(Para|Table) model + cell context stack,
    tblGrid columns, gridSpan/vMerge, table/cell borders + shading, row-atomic
    pagination, cell vAlign.
  - **M4.7 ✅** rich runs (italic/underline/strike/super+subscript/highlight),
    paragraph spacing (w:spacing line/before/after), tabs, line/page breaks,
    paragraph borders + shading.
  - **M4.8 ✅** headers/footers (sectPr references + titlePg first-page), running
    on every page; verified against a real 49-page manual (cover matches).
  - **M4.9 ✅ — high-fidelity pass** (verified against a real 56-page manual, audited
    parameter-by-parameter): explicit `w:br`/`pageBreakBefore` + `keepLines`
    pagination; `w:jc` justify; right/first-line indent; true `vMerge` vertical
    spanning; running-header logo + separator rules; **floating drawings** —
    anchored images sized/positioned within groups, rounded `wedge*Callout` bodies
    with pointer tails, straight + `custGeom` cubic-bézier connectors with arrowheads,
    text-wrap reservation; paragraph-mark vs paragraph-background `w:shd`.
  - **M4.10 ✅ — multi-font.** `w:rFonts` per-run/per-glyph selection via
    `dv_text::Fonts`; loads fonts embedded in the file (de-obfuscating `.odttf`);
    caller font map (`mount({ fonts })`); script fallback so missing glyphs never
    render `.notdef`. Verified: mapping 標楷體→a CJK face switches that run's glyphs.
  - *Remaining DOCX long tail: tab dot-leaders, pixel-exact float anchor origins,
    bundling a kai-style free face for 標楷體 lookalike substitution.*
- **M5 — PPTX viewer (self-written DrawingML).**
  - **M5.1 ✅** positioned text boxes + solid-fill rects, run formatting, slide nav (`PptxDeck`/`PptxViewer`).
  - **M5.2 ✅** raster images (`p:pic` → blip → media → decode → Image).
  - **M5.3 ✅** shape geometry: 12 presets + custGeom + outlines.
  - **M5.4 ✅** theme + master/layout inheritance: `schemeClr` via `clrMap` +
    lumMod/lumOff/shade/tint; slide→layout→master→theme chain; master/layout
    background decoration + bg colour; placeholder text-style cascade. Verified
    against a real 11-slide deck (matches the reference render).
  - **M5.5 ✅ — multi-font.** Same `dv_text::Fonts` engine: `a:latin`/`a:ea`/`a:cs`
    per-run selection + caller font map (`mount({ fonts })`) + script fallback.
    (PowerPoint-embedded fonts are MicroType-Express EOT — not loadable.)
  - **M5.6 — tables (`a:tbl`). NOT done** — port of the DOCX table layout to EMU/DrawingML.
- **M6 — CSV ✅.** RFC-4180 parser in `dv-xlsx` (`Sheet::from_csv`: quoted fields,
  embedded delimiters/newlines, `""` escapes, comma/tab/semicolon auto-detect; numbers
  right-align, columns auto-size) reusing the XLSX viewport renderer + viewer. `mount()`
  sniffs non-binary text → CSV (or force with `{ format: "csv" }`).
- **Cross-cutting** — SSIM screenshot-diff harness to measure fidelity honestly;
  wasm size pass (wasm-opt, drop unused features).

## License

MIT OR Apache-2.0.
