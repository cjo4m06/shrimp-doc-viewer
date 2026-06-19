# doc-viewer

> A browser, **viewer-only**, multi-format document viewer — drop bytes in, get a
> zoomable, virtualized viewer out. Pure-JS API over a Rust → WebAssembly core. No
> server, no editing, no page plugins.

Open **PDF · Word (.docx) · Excel (.xlsx) · PowerPoint (.pptx) · CSV · Markdown/
text · RTF · OpenDocument (.odt/.ods/.odp) · images** with one call:

```js
import { mount } from "doc-viewer";

const viewer = await mount(document.getElementById("doc"), fileBytesOrBlobOrUrl);
viewer.zoomIn();
```

`mount()` sniffs the format from the bytes and renders it — same call for every
type. **No font configuration required**: a free-for-commercial-use CJK font ships
with the package and is used by default (override it any time — see [Fonts](#fonts)).

---

## Why

Rendering happens in **one shared engine** (display-list IR → text/font stack →
CPU raster), so every format gets the same crisp text, CJK support and zoom. PDF is
handled by **PDFium-WASM** (Chrome's engine); everything else is a **self-written
Rust renderer** — no heavyweight LibreOffice-in-the-browser, no editing surface,
small payload, runs entirely client-side. Heavy rasterization runs in a **Web
Worker**, so zooming and scrolling stay at 60 fps.

| Capability | |
| --- | --- |
| Runs fully in the browser | ✅ no server round-trips |
| Latin **and 繁體中文 / CJK** | ✅ one shared text stack |
| Virtualized + zoomable | ✅ constant memory on large docs |
| Off-main-thread rendering | ✅ plain Web Worker (no `SharedArrayBuffer`, no COOP/COEP) |
| Pure-JS API, TypeScript types | ✅ `mount()` + auto format detection |

## Install

```bash
npm install doc-viewer
```

The package is self-contained — the WebAssembly core **and** a default CJK font
(Noto Sans TC, SIL OFL 1.1) ship inside it, so `mount()` works with zero config.
The font is the bulk of the install size (~12 MB); it is fetched lazily (only when a
non-PDF format is first opened) and you can [swap it for a smaller/self-hosted
one](#fonts). PDF optionally takes a **PDFium wasm URL** (defaults to the embedpdf CDN).

ESM only. Modern browsers (Chrome/Edge/Firefox/Safari). No bundler config required;
the wasm, the render worker and the font are all resolved via `import.meta.url`.

## Quick start

```js
import { init, mount } from "doc-viewer";

await init();                       // optional: warm the wasm core up front

const viewer = await mount(target, source, {
  // all optional:
  fontUrl: "/fonts/MyFont.ttf",     // override the bundled default font
  cjkFallbackFontUrl: "/fonts/NotoSansTC.ttf", // PDF only: render non-embedded CJK
});

console.log(viewer.pageCount);      // (paged formats)
viewer.fitWidth();
// …
viewer.destroy();                   // free resources when you remove it
```

- `target` — an `HTMLElement` to render into (it is cleared and owned by the viewer).
- `source` — `Blob | ArrayBuffer | Uint8Array | string (URL) | URL`.
- returns a **viewer** whose shape depends on the detected format (see [API](#api)).

## Fonts

PDF carries its own fonts; **every other format is painted by our own glyph
renderer**, which needs a real font file (a document only declares font *names*, not
glyph data). The package **bundles Noto Sans TC** (SIL OFL 1.1, commercial-use OK) and
uses it by default, so you don't have to configure anything.

Each glyph is resolved through a layered lookup:

1. **fonts embedded in the file** (e.g. a DOCX `.odttf`) — used automatically;
2. **your font map** — families you supply explicitly (below);
3. matching font by name, then a per-script fallback (CJK / Latin / symbol);
4. the **bundled default** font.

A document usually *declares* proprietary fonts (標楷體, MingLiU, Calibri…) without
embedding them. Provide those and the matching runs render in them:

```js
mount(el, bytes, {
  fonts: {
    "標楷體": "/fonts/BiauKai.ttf",   // value: URL | Uint8Array | ArrayBuffer
    "Calibri": "/fonts/Carlito.ttf",
  },
});
```

To replace the default fallback (e.g. a smaller subset, or to drop the ~12 MB font
from your bundle and self-host it), pass `fontUrl`:

```js
mount(el, bytes, { fontUrl: "/fonts/my-subset.ttf" });
```

For PDF, pass `cjkFallbackFontUrl` so a PDF that *doesn't* embed its CJK fonts renders
instead of going blank (PDF doesn't use the bundled default, to avoid loading a font
the typical embedded-font PDF doesn't need).

## API

### `init(options?) → Promise<{ version }>`

Loads and instantiates the wasm core once (idempotent). Optional — `mount()` calls
it for you.

| option | description |
| --- | --- |
| `wasmUrl` | override the core wasm location |

### `mount(target, source, options?) → Promise<Viewer>`

Detects the format and renders it. Common options:

| option | applies to | description |
| --- | --- | --- |
| `fontUrl` | DOCX·XLSX·PPTX·CSV·MD·RTF·ODF | override the bundled default fallback font (optional) |
| `fonts` | DOCX·PPTX·MD·RTF·ODT·ODP | `{ family: URL \| Uint8Array \| ArrayBuffer }` map of declared families |
| `cjkFallbackFontUrl` | PDF | font for non-embedded CJK PDFs (optional) |
| `pdfiumWasmUrl` | PDF | PDFium wasm location (defaults to CDN) |
| `zoom` | most | initial zoom (default fit/1) |
| `format` | all | force a format, skip sniffing (`"pdf"`, `"docx"`, `"csv"`, `"markdown"`, …) |
| `onZoom(z)` / `onProgress(n,total)` | most / PDF | callbacks |

### Viewer

All viewers expose **`destroy()`** (frees the worker / PDFium / wasm handles — call
it when you remove the element). Beyond that the surface depends on the format:

| Format → viewer | members |
| --- | --- |
| PDF, DOCX, MD, TXT, RTF, ODT, ODP | `pageCount`, `setZoom(z)`, `zoomIn()`, `zoomOut()`, `fitWidth()` |
| XLSX, CSV, ODS | `sheetNames`, `setZoom(z)`, `zoomIn()`, `zoomOut()`, `fitWidth()` |
| PPTX | `next()`, `prev()` (slide navigation) |
| images | `setZoom(z)`, `fitWidth()` |

`Ctrl`/`⌘` + mouse-wheel zooms in every viewer.

### Helpers

`sniffFormat(bytes) → string`, `sniffOoxml(bytes) → string`, `coreVersion: string`.

## Supported formats

| Format | Engine | Notes |
| --- | --- | --- |
| **PDF** | PDFium-WASM | high fidelity; embedded + non-embedded CJK; Web Worker; password support |
| **DOCX** | own flow layout | styles, lists/numbering, **tables** (borders/shading/spanning), headers/footers, rich runs, justify, page breaks, inline images, floating drawings (callouts/connectors), multi-font |
| **XLSX** | own grid | column/row sizes, merges, styles, number formats, sheet tabs, frozen headers |
| **PPTX** | own DrawingML | theme/master/layout inheritance, shapes (preset + custom geometry), images, multi-font |
| **CSV** | own (RFC 4180) | quoted fields, comma/tab/semicolon auto-detect; reuses the grid viewer |
| **Markdown / TXT** | own | headings, lists, code, quotes, inline bold/italic/code/strike/links, paginated |
| **RTF** | own | bold/italic/colour, Unicode + codepage `\'hh` (CJK), paragraphs |
| **ODF (.odt/.ods/.odp)** | own | ODT/ODP text + structure → flow; ODS → grid |
| **Images** | browser-native | PNG/JPEG/GIF/WebP/BMP/SVG; zoom/fit |

**Out of scope:** legacy binary `.doc/.xls/.ppt` (OLE) — convert them server-side
to OOXML/PDF first. The self-written renderers target **viewer-grade fidelity**
(typical documents render faithfully) rather than pixel-parity with Office; a
proprietary system font the file only *declares* renders in the bundled fallback
unless you supply it via `fonts`.

## How it works

```
your bytes ──sniff──▶ frontend (PDF | DOCX | XLSX | PPTX | CSV | MD | RTF | ODF | image)
                          │  lowers into
                          ▼
                   dv-ir DisplayList  ──▶  tiny-skia CPU raster  ──▶  ImageBitmap ──▶ <canvas>
                   (paths + glyph runs)        (in a Web Worker)
```

- **Rust crates** (`dv-ir`, `dv-text`, `dv-render`, `dv-flow`, `dv-xlsx`, `dv-docx`,
  `dv-pptx`, `dv-rtf`, `dv-md`, `dv-odf`) compile to a single `~2 MB` wasm core.
- Text is shaped with **rustybuzz** and outlined with **skrifa**; everything is
  painted by **tiny-skia** (pure-CPU, works in any browser/worker — no WebGPU needed).
- The viewer **virtualizes** pages/cells (renders only what's near the viewport) and
  runs rasterization in a **Web Worker**, so memory stays flat and the UI stays smooth.
- Deliberately **single-threaded** wasm → no `SharedArrayBuffer`, so the host page
  needs no COOP/COEP headers and its cross-origin loads are unaffected.

## Build from source

Prerequisites: Rust + the `wasm32-unknown-unknown` target, `wasm-bindgen-cli`
(matching the pinned `wasm-bindgen`, currently `0.2.125`), Node, and optionally
`wasm-opt` (binaryen).

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --locked

./scripts/build-wasm.sh                 # build the wasm core + JS glue into packages/doc-viewer/wasm
python3 -m http.server 8123             # serve the repo; open examples/browser/*.html
```

Native smoke tests (render a page to PNG, no browser):

```bash
cargo run -p dv-docx --example docx_demo -- <file.docx> <font.ttf>
cargo run -p dv-pptx --example pptx_demo -- <file.pptx> <font.ttf>
```

## License

MIT OR Apache-2.0.
