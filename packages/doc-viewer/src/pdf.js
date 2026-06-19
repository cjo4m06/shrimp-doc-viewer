// PDF frontend — wraps PDFium (compiled to WASM, via @embedpdf/pdfium) behind a
// small JS surface. PDFium renders pages to its own BGRA bitmap; we convert to
// RGBA ImageData and blit to a canvas. This path does NOT go through the Rust
// geba (PDFium rasterizes internally) — that's the deliberate "embed a healthy
// engine for PDF" decision.
//
// 繁體中文 caveat: PDFium's bundled fallback is DejaVu (no CJK). PDFs that EMBED
// their fonts (most Office/Chrome exports) render fully, incl. 繁中. PDFs that
// reference a NON-embedded CJK font (MingLiU/PMingLiU/JhengHei/Adobe-CNS1) need a
// CJK fallback installed via FPDF_SetSystemFontInfo — wired in a later milestone.

// PDFium's init is imported lazily so this module also works inside a Web Worker
// (which doesn't honour the document's import map). Bundlers statically resolve
// the bare `import("@embedpdf/pdfium")`; the no-bundler demo / the worker pass an
// explicit `embedpdfUrl`.
async function loadPdfiumInit(opts) {
  if (opts.embedpdfUrl) return (await import(/* @vite-ignore */ opts.embedpdfUrl)).init;
  return (await import("@embedpdf/pdfium")).init;
}

const FPDF_ANNOT = 0x01;
const FPDF_LCD_TEXT = 0x02;

// Windows-style charsets PDFium asks for when it meets a non-embedded CJK font
// (mapped from the CID font's CIDSystemInfo ordering): SHIFTJIS, HANGEUL,
// GB2312, CHINESEBIG5. We serve our bundled CJK fallback for any of these.
const CJK_CHARSETS = new Set([128, 129, 134, 136]);

let _enginePromise = null;
let _fallbackInstalled = false;

/**
 * Install a CJK fallback font into PDFium via FPDF_SetSystemFontInfo.
 *
 * PDFium's WASM build ships only DejaVu (no CJK), so PDFs that reference a
 * non-embedded CJK font (MingLiU/PMingLiU/JhengHei/Adobe-CNS1) render blank.
 * We build an FPDF_SYSFONTINFO struct entirely from JS — Emscripten's
 * `addFunction` turns our JS callbacks into C function pointers, and
 * `setValue` writes them into a heap struct — then hand it to PDFium. When
 * PDFium maps a non-embedded CJK font it calls our MapFont (→ a handle) then
 * GetFontData (→ the font bytes), and rasterizes with our Noto CJK TC.
 *
 * This handles the predefined-CMap / charset-substitution case (Adobe-CNS1
 * etc.). It cannot fix Identity-H PDFs that name a specific proprietary font
 * (e.g. MingLiU) without embedding it — those CIDs are GIDs of that exact
 * font, so only the original font renders correctly.
 *
 * @param {import('@embedpdf/pdfium').WrappedPdfiumModule} mod
 * @param {Uint8Array} fontBytes  Retained for the engine's lifetime (the
 *   callbacks close over it; never freed).
 */
function installCjkFallback(mod, fontBytes) {
  if (_fallbackInstalled) return;
  const { pdfium } = mod;
  const add = pdfium.addFunction;
  const FONT_HANDLE = 1; // any non-null opaque handle PDFium hands back to us

  // FPDF_SYSFONTINFO callbacks (see public/fpdf_sysfontinfo.h).
  const pRelease = add(() => {}, "vi");
  const pEnumFonts = add(() => {}, "vii");
  // void* MapFont(pThis, weight, bItalic, charset, pitch_family, face, bExact)
  const pMapFont = add((_t, _w, _i, charset) => (CJK_CHARSETS.has(charset) ? FONT_HANDLE : 0), "iiiiiiii");
  // void* GetFont(pThis, face)
  const pGetFont = add(() => 0, "iii");
  // unsigned long GetFontData(pThis, hFont, table, buffer, buf_size)
  const pGetFontData = add((_t, hFont, table, buffer, bufSize) => {
    if (hFont !== FONT_HANDLE || table !== 0) return 0; // serve the whole sfnt only
    if (buffer === 0 || bufSize === 0) return fontBytes.length; // size probe
    const n = Math.min(bufSize, fontBytes.length);
    pdfium.HEAPU8.set(fontBytes.subarray(0, n), buffer);
    return n;
  }, "iiiiii");
  // unsigned long GetFaceName(pThis, hFont, buffer, buf_size)
  const pGetFaceName = add(() => 0, "iiiii");
  // int GetFontCharset(pThis, hFont)
  const pGetFontCharset = add(() => 136, "iii");
  // void DeleteFont(pThis, hFont)
  const pDeleteFont = add(() => {}, "vii");

  // version(i32) + 8 function pointers (4 bytes each in wasm32) = 36 bytes.
  const ptr = pdfium.wasmExports.malloc(40);
  pdfium.setValue(ptr + 0, 1, "i32"); // version
  pdfium.setValue(ptr + 4, pRelease, "i32");
  pdfium.setValue(ptr + 8, pEnumFonts, "i32");
  pdfium.setValue(ptr + 12, pMapFont, "i32");
  pdfium.setValue(ptr + 16, pGetFont, "i32");
  pdfium.setValue(ptr + 20, pGetFontData, "i32");
  pdfium.setValue(ptr + 24, pGetFaceName, "i32");
  pdfium.setValue(ptr + 28, pGetFontCharset, "i32");
  pdfium.setValue(ptr + 32, pDeleteFont, "i32");

  mod.FPDF_SetSystemFontInfo(ptr); // PDFium keeps the struct; we never free it
  _fallbackInstalled = true;
}

/**
 * Lazily initialise the PDFium WASM engine (idempotent). Options from the first
 * call win (the engine is a singleton).
 * @param {{ pdfiumWasmUrl?: string, cjkFallbackFontUrl?: string }} [opts]
 */
export function getPdfEngine(opts = {}) {
  if (!_enginePromise) {
    _enginePromise = (async () => {
      const initPdfium = await loadPdfiumInit(opts);
      const overrides = {};
      if (opts.pdfiumWasmUrl) {
        overrides.wasmBinary = await (await fetch(opts.pdfiumWasmUrl)).arrayBuffer();
      }
      const mod = await initPdfium(overrides);
      mod.PDFiumExt_Init();
      if (opts.cjkFallbackFontUrl) {
        const fontBytes = new Uint8Array(await (await fetch(opts.cjkFallbackFontUrl)).arrayBuffer());
        installCjkFallback(mod, fontBytes);
      }
      return mod;
    })();
  }
  return _enginePromise;
}

// FPDF_GetLastError codes (fpdfview.h) -> human messages.
const PDF_ERRORS = {
  1: "unknown error",
  2: "file not found or could not be opened",
  3: "not a PDF or corrupted",
  4: "password required or incorrect", // FPDF_ERR_PASSWORD
  5: "unsupported security scheme",
  6: "page not found or content error",
};

/** Error thrown when a PDF can't be opened; `.code` is the FPDF_ERR_* value. */
export class PdfOpenError extends Error {
  constructor(code) {
    super(`PDF open failed: ${PDF_ERRORS[code] || "error " + code} (FPDF_ERR ${code})`);
    this.name = "PdfOpenError";
    this.code = code;
    this.needsPassword = code === 4;
  }
}

/**
 * Open a PDF from bytes. Returns a handle; call `.destroy()` when done (PDFium
 * has no GC — every resource must be freed).
 * @param {Uint8Array} bytes
 * @param {{ pdfiumWasmUrl?: string, cjkFallbackFontUrl?: string, password?: string }} [opts]
 */
export async function openDocument(bytes, opts = {}) {
  const mod = await getPdfEngine(opts);
  const { pdfium } = mod;

  // The PDF bytes must stay alive for the document's whole lifetime (PDFium
  // reads lazily), so we keep `filePtr` until destroy().
  const filePtr = pdfium.wasmExports.malloc(bytes.length);
  pdfium.HEAPU8.set(bytes, filePtr);

  const doc = mod.FPDF_LoadMemDocument(filePtr, bytes.length, opts.password || "");
  if (!doc) {
    const code = mod.FPDF_GetLastError();
    pdfium.wasmExports.free(filePtr);
    throw new PdfOpenError(code);
  }

  const pageCount = mod.FPDF_GetPageCount(doc);

  return {
    pageCount,

    /** Page size in PDF points (1/72 inch). */
    pageSize(index) {
      const page = mod.FPDF_LoadPage(doc, index);
      const w = mod.FPDF_GetPageWidthF(page);
      const h = mod.FPDF_GetPageHeightF(page);
      mod.FPDF_ClosePage(page);
      return { width: w, height: h };
    },

    /**
     * Render page `index` to RGBA `ImageData`.
     * @param {number} index 0-based page index.
     * @param {number} scale Device pixels per PDF point.
     */
    renderPageToImageData(index, scale) {
      const page = mod.FPDF_LoadPage(doc, index);
      const wPt = mod.FPDF_GetPageWidthF(page);
      const hPt = mod.FPDF_GetPageHeightF(page);
      const w = Math.max(1, Math.ceil(wPt * scale));
      const h = Math.max(1, Math.ceil(hPt * scale));

      // alpha=1 -> FPDFBitmap_BGRA, stride = w*4 (no padding).
      const bmp = mod.FPDFBitmap_Create(w, h, 1);
      mod.FPDFBitmap_FillRect(bmp, 0, 0, w, h, 0xffffffff); // white background
      mod.FPDF_RenderPageBitmap(bmp, page, 0, 0, w, h, 0, FPDF_ANNOT | FPDF_LCD_TEXT);

      const bufPtr = mod.FPDFBitmap_GetBuffer(bmp);
      // Re-read HEAPU8 (memory may have grown) and copy out before next alloc.
      const bgra = new Uint8Array(pdfium.HEAPU8.buffer, bufPtr, w * h * 4).slice();
      mod.FPDFBitmap_Destroy(bmp);
      mod.FPDF_ClosePage(page);

      // PDFium is BGRA; ImageData wants RGBA — swap B/R in place.
      for (let p = 0; p < bgra.length; p += 4) {
        const t = bgra[p];
        bgra[p] = bgra[p + 2];
        bgra[p + 2] = t;
      }
      return { imageData: new ImageData(new Uint8ClampedArray(bgra.buffer), w, h), width: w, height: h };
    },

    destroy() {
      mod.FPDF_CloseDocument(doc);
      pdfium.wasmExports.free(filePtr);
    },
  };
}

// --- Rendering backends ---------------------------------------------------
// Both expose the same async surface: open(bytes) -> { pageCount, sizes } and
// renderPage(index, scale) -> { bitmap, width, height }. `sizes` are PDF points;
// the bitmap is an ImageBitmap painted via a bitmaprenderer canvas.

const sanitizeEngineOpts = (o) => ({
  embedpdfUrl: o.embedpdfUrl,
  pdfiumWasmUrl: o.pdfiumWasmUrl,
  cjkFallbackFontUrl: o.cjkFallbackFontUrl,
  password: o.password,
});

/** PDFium kept alive in a Web Worker; pages rendered on demand off the main thread. */
class WorkerBackend {
  constructor(opts) {
    this.opts = opts;
    this.worker = new Worker(new URL("./pdf-worker.js", import.meta.url), { type: "module" });
    this._id = 0;
    this._pending = new Map();
    this.worker.onmessage = (e) => {
      const m = e.data;
      const p = this._pending.get(m.id);
      if (!p) return;
      this._pending.delete(m.id);
      if (m.type === "error") {
        const err = new Error(m.message);
        err.code = m.code;
        p.reject(err);
      } else {
        p.resolve(m);
      }
    };
    this.worker.onerror = (e) => {
      const err = new Error("worker error: " + (e.message || e.filename || "unknown"));
      for (const { reject } of this._pending.values()) reject(err);
      this._pending.clear();
    };
  }
  _rpc(msg, transfer) {
    const id = ++this._id;
    return new Promise((resolve, reject) => {
      this._pending.set(id, { resolve, reject });
      this.worker.postMessage({ ...msg, id }, transfer || []);
    });
  }
  async open(bytes) {
    const copy = bytes.slice(); // transfer a copy so caller keeps its bytes
    const r = await this._rpc({ type: "open", bytes: copy.buffer, opts: sanitizeEngineOpts(this.opts) }, [copy.buffer]);
    return { pageCount: r.pageCount, sizes: r.sizes };
  }
  async renderPage(index, scale) {
    const r = await this._rpc({ type: "render", index, scale });
    return { bitmap: r.bitmap, width: r.width, height: r.height };
  }
  destroy() {
    try { this._rpc({ type: "close" }); } catch { /* terminating anyway */ }
    this.worker.terminate();
  }
}

/** PDFium on the main thread (fallback when Workers are unavailable). */
class MainBackend {
  constructor(opts) {
    this.opts = opts;
    this.doc = null;
  }
  async open(bytes) {
    this.doc = await openDocument(bytes, this.opts);
    const sizes = [];
    for (let i = 0; i < this.doc.pageCount; i++) sizes.push(this.doc.pageSize(i));
    return { pageCount: this.doc.pageCount, sizes };
  }
  async renderPage(index, scale) {
    const { imageData, width, height } = this.doc.renderPageToImageData(index, scale);
    const bitmap = await createImageBitmap(imageData);
    return { bitmap, width, height };
  }
  destroy() {
    this.doc?.destroy();
    this.doc = null;
  }
}

const clamp = (v, lo, hi) => Math.max(lo, Math.min(hi, v));

/**
 * A virtualized, zoomable PDF viewer. Lays out one placeholder per page (sized
 * from the page dimensions, so the scrollbar is correct without rendering),
 * renders only pages near the viewport, and frees pages that scroll far away.
 * Zoom re-renders just the visible pages.
 */
class PdfViewer {
  constructor(container, backend, meta, opts) {
    this.container = container;
    this.backend = backend;
    this.pageCount = meta.pageCount;
    this.sizes = meta.sizes; // [{ width, height }] in PDF points
    this.opts = opts;
    this.onProgress = opts.onProgress;
    this.onZoom = opts.onZoom;
    this.dpr = (typeof devicePixelRatio === "number" && devicePixelRatio) || 1;
    this.baseScale = 96 / 72; // points -> CSS px at 100%
    this.zoom = clamp(opts.zoom ?? 1, 0.1, 10);
    this.minZoom = 0.1;
    this.maxZoom = 10;
    this.scrollRoot = opts.scrollRoot || null; // null = viewport

    this.pageEls = [];
    this.rendered = new Map(); // index -> deviceScale it was rendered at
    this.inFlight = new Map(); // index -> deviceScale requested

    this._buildPlaceholders();
    this._observer = new IntersectionObserver((entries) => this._onIntersect(entries), {
      root: this.scrollRoot,
      rootMargin: "500px 0px",
    });
    for (const el of this.pageEls) this._observer.observe(el);
    this._wheelHandler = (e) => this._onWheel(e);
    (this.scrollRoot || window).addEventListener("wheel", this._wheelHandler, { passive: false });
    this._renderVisible();
  }

  get cssScale() { return this.baseScale * this.zoom; }
  get deviceScale() { return this.baseScale * this.zoom * this.dpr; }

  _cssSize(i) {
    return { w: this.sizes[i].width * this.cssScale, h: this.sizes[i].height * this.cssScale };
  }

  _buildPlaceholders() {
    this.container.replaceChildren();
    this.pageEls = [];
    for (let i = 0; i < this.pageCount; i++) {
      const { w, h } = this._cssSize(i);
      const el = document.createElement("div");
      el.className = "dv-page";
      el.style.cssText =
        `width:${w}px;height:${h}px;margin:0 auto 12px;background:#fff;` +
        `box-shadow:0 1px 6px rgba(0,0,0,.18);overflow:hidden;`;
      el.dataset.page = String(i + 1);
      this.pageEls.push(el);
      this.container.appendChild(el);
    }
  }

  _onIntersect(entries) {
    for (const e of entries) {
      const i = Number(e.target.dataset.page) - 1;
      if (e.isIntersecting) this._renderPage(i);
      else this._evict(i);
    }
  }

  async _renderPage(i) {
    const scale = this.deviceScale;
    if (this.rendered.get(i) === scale) return; // already correct
    if (this.inFlight.get(i) === scale) return; // request in flight at this scale
    this.inFlight.set(i, scale);
    try {
      const { bitmap, width, height } = await this.backend.renderPage(i, scale);
      // Zoom may have changed while we waited — drop stale bitmap and retry.
      if (this.deviceScale !== scale) {
        bitmap.close?.();
        this.inFlight.delete(i);
        if (this._isNearViewport(i)) this._renderPage(i);
        return;
      }
      const canvas = document.createElement("canvas");
      canvas.width = width;
      canvas.height = height;
      const { w, h } = this._cssSize(i);
      canvas.style.cssText = `width:${w}px;height:${h}px;display:block;`;
      canvas.getContext("bitmaprenderer").transferFromImageBitmap(bitmap);
      this.pageEls[i].replaceChildren(canvas);
      this.rendered.set(i, scale);
      this.onProgress?.(this.rendered.size, this.pageCount);
    } catch (err) {
      console.warn("ShrimpDocViewer: page", i + 1, "render failed:", err);
    } finally {
      if (this.inFlight.get(i) === scale) this.inFlight.delete(i);
    }
  }

  _evict(i) {
    if (this.rendered.has(i)) {
      this.pageEls[i].replaceChildren();
      this.rendered.delete(i);
    }
  }

  _isNearViewport(i) {
    const r = this.pageEls[i].getBoundingClientRect();
    const vh = this.scrollRoot ? this.scrollRoot.clientHeight : window.innerHeight;
    return r.bottom > -500 && r.top < vh + 500;
  }

  _renderVisible() {
    for (let i = 0; i < this.pageCount; i++) if (this._isNearViewport(i)) this._renderPage(i);
  }

  setZoom(z) {
    const next = clamp(z, this.minZoom, this.maxZoom);
    if (next === this.zoom) return;
    // Keep the scroll position stable by preserving the fraction through resize.
    const scroller = this.scrollRoot || document.scrollingElement || document.documentElement;
    const frac = scroller.scrollTop / Math.max(1, scroller.scrollHeight);
    this.zoom = next;
    for (let i = 0; i < this.pageCount; i++) {
      const { w, h } = this._cssSize(i);
      this.pageEls[i].style.width = w + "px";
      this.pageEls[i].style.height = h + "px";
    }
    // Rendered canvases are now the wrong resolution — drop them and re-render visible.
    for (const i of [...this.rendered.keys()]) this._evict(i);
    scroller.scrollTop = frac * scroller.scrollHeight;
    this._renderVisible();
    this.onZoom?.(this.zoom);
  }

  zoomIn() { this.setZoom(this.zoom * 1.25); }
  zoomOut() { this.setZoom(this.zoom / 1.25); }

  /** Fit the widest page to the container's content width. */
  fitWidth() {
    const avail = (this.scrollRoot || this.container).clientWidth - 24;
    const maxPtW = Math.max(...this.sizes.map((s) => s.width));
    this.setZoom(avail / (maxPtW * this.baseScale));
  }

  _onWheel(e) {
    if (!e.ctrlKey && !e.metaKey) return; // Ctrl/Cmd + wheel = zoom
    e.preventDefault();
    if (e.deltaY < 0) this.zoomIn();
    else this.zoomOut();
  }

  destroy() {
    this._observer?.disconnect();
    (this.scrollRoot || window).removeEventListener("wheel", this._wheelHandler);
    this.backend.destroy();
    this.container.replaceChildren();
  }
}

/**
 * Render a PDF into `container` as a virtualized, zoomable {@link PdfViewer}.
 *
 * Rendering runs in a Web Worker by default (PDFium off the main thread; pages
 * rendered on demand and freed when they scroll away — constant memory on big
 * docs). Falls back to the main thread if Workers are unavailable or fail
 * (unless `useWorker: true` forces the worker).
 *
 * @param {HTMLElement} container
 * @param {Uint8Array} bytes
 * @param {{ zoom?: number, pdfiumWasmUrl?: string, cjkFallbackFontUrl?: string,
 *   embedpdfUrl?: string, password?: string, useWorker?: boolean,
 *   scrollRoot?: Element|null, onProgress?: (n:number,total:number)=>void,
 *   onZoom?: (z:number)=>void }} [opts]
 * @returns {Promise<PdfViewer>}
 */
export async function renderPdfInto(container, bytes, opts = {}) {
  container.replaceChildren();

  let backend = null;
  const wantWorker = opts.useWorker !== false && typeof Worker !== "undefined";
  if (wantWorker) {
    try {
      backend = new WorkerBackend(opts);
      const meta = await backend.open(bytes);
      return new PdfViewer(container, backend, meta, opts);
    } catch (e) {
      backend?.destroy();
      if (opts.useWorker === true) throw e;
      console.warn("ShrimpDocViewer: worker backend failed; falling back to main thread.", e);
    }
  }

  backend = new MainBackend(opts);
  const meta = await backend.open(bytes);
  return new PdfViewer(container, backend, meta, opts);
}
