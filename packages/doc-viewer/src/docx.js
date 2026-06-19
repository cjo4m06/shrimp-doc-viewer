// DOCX frontend — a paginated, virtualized, zoomable viewer. Our Rust flow-layout
// engine paginates the document; the WASM DocxDoc renders one page at a time, and
// JS virtualizes pages (render only near the viewport, free the rest) + zoom.

import { init, DEFAULT_FONT_URL } from "./index.js";
import { WorkerDoc } from "./worker-doc.js";

const clamp = (v, lo, hi) => Math.max(lo, Math.min(hi, v));

/** Resolve an `opts.fonts` map (family -> URL|Uint8Array|ArrayBuffer) to [name, Uint8Array] pairs. */
export async function resolveFontMap(fonts) {
  const extra = [];
  for (const [name, src] of Object.entries(fonts || {})) {
    let u8;
    if (src instanceof Uint8Array) u8 = src;
    else if (src instanceof ArrayBuffer) u8 = new Uint8Array(src);
    else u8 = new Uint8Array(await (await fetch(src)).arrayBuffer());
    extra.push([name, u8]);
  }
  return extra;
}

/**
 * Mount a virtualized DOCX viewer into `container`.
 * @param {HTMLElement} container
 * @param {Uint8Array} bytes
 * @param {{ fontUrl?: string, cjkFallbackFontUrl?: string, fonts?: Record<string,string|Uint8Array|ArrayBuffer>, zoom?: number, onZoom?: (z:number)=>void }} [opts]
 *
 * `opts.fonts` maps a declared font family to a font file (URL string, Uint8Array,
 * or ArrayBuffer): the document's runs that name that family render with it (e.g.
 * `{ "標楷體": "/fonts/BiauKai.ttf" }`). Embedded fonts in the file load automatically.
 */
export async function renderDocxInto(container, bytes, opts = {}) {
  await init();
  const fontUrl = opts.fontUrl || opts.cjkFallbackFontUrl || DEFAULT_FONT_URL;
  const fontBytes = new Uint8Array(await (await fetch(fontUrl)).arrayBuffer());
  const extra = await resolveFontMap(opts.fonts);
  const doc = await WorkerDoc.open("docx", bytes, fontBytes, extra);
  return new DocxViewer(container, doc, opts);
}

export class DocxViewer {
  constructor(container, doc, opts) {
    this.doc = doc;
    this.pageCount = doc.pageCount();
    const [w, h] = doc.pageSize();
    this.pw = w;
    this.ph = h;
    this.zoom = clamp(opts.zoom ?? 1, 0.25, 5);
    this.dpr = (typeof devicePixelRatio === "number" && devicePixelRatio) || 1;
    this.onZoom = opts.onZoom;
    this.pageEls = [];
    this.rendered = new Set();
    this._inflight = new Set(); // pages whose render is in the worker
    this._zoomToken = 0; // bumped on zoom; stale worker results are discarded

    container.replaceChildren();
    this._build(container);
    this._applyGeometry();
    this._observer = new IntersectionObserver((es) => this._onIntersect(es), { rootMargin: "600px 0px" });
    for (const el of this.pageEls) this._observer.observe(el);
    this._wheel = (e) => { if (e.ctrlKey || e.metaKey) { e.preventDefault(); e.deltaY < 0 ? this.zoomIn() : this.zoomOut(); } };
    this.pages.addEventListener("wheel", this._wheel, { passive: false });
    this._renderVisible();
  }

  _build(container) {
    const bar = document.createElement("div");
    bar.style.cssText = "display:flex;gap:.4rem;align-items:center;margin-bottom:.5rem;font:13px system-ui";
    const mk = (label, fn) => {
      const b = document.createElement("button");
      b.textContent = label;
      b.style.cssText = "padding:.2rem .6rem;border:1px solid #cbd2da;border-radius:4px;cursor:pointer;background:#fff";
      b.addEventListener("click", fn);
      bar.appendChild(b);
      return b;
    };
    mk("−", () => this.zoomOut());
    this.zoomLabel = document.createElement("span");
    this.zoomLabel.style.cssText = "font:13px ui-monospace,monospace;min-width:3.2em;text-align:center";
    bar.appendChild(this.zoomLabel);
    mk("＋", () => this.zoomIn());
    mk("符合寬度", () => this.fitWidth());

    const pages = document.createElement("div");
    pages.style.cssText = "background:#e9ecef;padding:16px;border-radius:6px";
    for (let i = 0; i < this.pageCount; i++) {
      const el = document.createElement("div");
      el.dataset.page = String(i);
      el.style.cssText = "margin:0 auto 16px;background:#fff;box-shadow:0 1px 8px rgba(0,0,0,.18)";
      this.pageEls.push(el);
      pages.appendChild(el);
    }
    container.appendChild(bar);
    container.appendChild(pages);
    this.pages = pages;
    this._updateZoomLabel();
  }

  _applyGeometry() {
    for (const el of this.pageEls) {
      el.style.width = this.pw * this.zoom + "px";
      el.style.height = this.ph * this.zoom + "px";
    }
  }

  _onIntersect(entries) {
    for (const e of entries) {
      const i = Number(e.target.dataset.page);
      if (e.isIntersecting) this._renderPage(i);
      else this._evict(i);
    }
  }

  // Rasterization happens in the render Worker (off the UI thread); the main thread
  // only fires a request and blits the returned ImageBitmap, so nothing here blocks.
  _renderPage(i) {
    if (this.rendered.has(i) || this._inflight.has(i)) return;
    const scale = this.zoom * this.dpr;
    const token = this._zoomToken; // discard results that arrive after a later zoom
    this._inflight.add(i);
    this.doc.renderPage(i, scale).then(
      (m) => {
        this._inflight.delete(i);
        if (token !== this._zoomToken || !this._isNear(i)) {
          m.bitmap.close();
          return;
        }
        const canvas = document.createElement("canvas");
        canvas.width = m.w;
        canvas.height = m.h;
        canvas.style.cssText = `width:${m.w / this.dpr}px;height:${m.h / this.dpr}px;display:block`;
        canvas.getContext("2d").drawImage(m.bitmap, 0, 0);
        m.bitmap.close();
        this.pageEls[i]?.replaceChildren(canvas);
        this.rendered.add(i);
      },
      () => this._inflight.delete(i),
    );
  }

  _evict(i) {
    if (this.rendered.has(i)) {
      this.pageEls[i].replaceChildren();
      this.rendered.delete(i);
    }
  }

  _isNear(i) {
    const r = this.pageEls[i].getBoundingClientRect();
    return r.bottom > -600 && r.top < window.innerHeight + 600;
  }

  _renderVisible() {
    for (let i = 0; i < this.pageCount; i++) if (this._isNear(i)) this._renderPage(i);
  }

  setZoom(z) {
    const next = clamp(z, 0.25, 5);
    if (next === this.zoom) return;
    const scroller = document.scrollingElement || document.documentElement;
    const frac = scroller.scrollTop / Math.max(1, scroller.scrollHeight);
    this.zoom = next;
    this._zoomToken++; // in-flight worker renders for the old zoom are now stale
    this._applyGeometry();
    // Instant feedback: CSS-scale the already-rendered canvases (cheap GPU
    // transform) instead of re-rasterizing on every wheel tick.
    for (const i of this.rendered) {
      const c = this.pageEls[i].firstChild;
      if (c) {
        c.style.width = this.pw * this.zoom + "px";
        c.style.height = this.ph * this.zoom + "px";
      }
    }
    scroller.scrollTop = frac * scroller.scrollHeight;
    this._updateZoomLabel();
    this.onZoom?.(this.zoom);
    // Re-rasterize crisply once zooming settles (coalesces rapid ticks).
    clearTimeout(this._zt);
    this._zt = setTimeout(() => {
      for (const i of [...this.rendered]) this._evict(i);
      this._renderVisible();
    }, 160);
  }

  zoomIn() { this.setZoom(this.zoom * 1.25); }
  zoomOut() { this.setZoom(this.zoom / 1.25); }
  fitWidth() {
    const avail = this.pages.clientWidth - 32;
    if (this.pw > 0) this.setZoom(avail / this.pw);
  }

  _updateZoomLabel() {
    if (this.zoomLabel) this.zoomLabel.textContent = Math.round(this.zoom * 100) + "%";
  }

  destroy() {
    this._observer?.disconnect();
    clearTimeout(this._zt);
    this.pages.removeEventListener("wheel", this._wheel);
    this.doc?.destroy?.(); // terminate the render worker
    this.pages.parentElement?.replaceChildren?.();
  }
}
