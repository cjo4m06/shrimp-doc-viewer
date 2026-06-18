// DOCX frontend — a paginated, virtualized, zoomable viewer. Our Rust flow-layout
// engine paginates the document; the WASM DocxDoc renders one page at a time, and
// JS virtualizes pages (render only near the viewport, free the rest) + zoom.

import { init } from "./index.js";
import { DocxDoc } from "../wasm/dv_wasm.js";

const clamp = (v, lo, hi) => Math.max(lo, Math.min(hi, v));

/**
 * Mount a virtualized DOCX viewer into `container`.
 * @param {HTMLElement} container
 * @param {Uint8Array} bytes
 * @param {{ fontUrl?: string, cjkFallbackFontUrl?: string, zoom?: number, onZoom?: (z:number)=>void }} [opts]
 */
export async function renderDocxInto(container, bytes, opts = {}) {
  await init();
  const fontUrl = opts.fontUrl || opts.cjkFallbackFontUrl;
  if (!fontUrl) {
    throw new Error("renderDocxInto: provide opts.fontUrl (a CJK-capable font, e.g. Noto Sans TC).");
  }
  const fontBytes = new Uint8Array(await (await fetch(fontUrl)).arrayBuffer());
  const doc = new DocxDoc(bytes, fontBytes);
  return new DocxViewer(container, doc, opts);
}

class DocxViewer {
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

  _renderPage(i) {
    if (this.rendered.has(i)) return;
    const scale = this.zoom * this.dpr;
    const img = this.doc.renderPage(i, scale);
    const w = img.width, h = img.height, data = img.takeData();
    img.free();
    const canvas = document.createElement("canvas");
    canvas.width = w;
    canvas.height = h;
    canvas.style.cssText = `width:${w / this.dpr}px;height:${h / this.dpr}px;display:block`;
    canvas.getContext("2d").putImageData(new ImageData(new Uint8ClampedArray(data), w, h), 0, 0);
    this.pageEls[i].replaceChildren(canvas);
    this.rendered.add(i);
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
    this._applyGeometry();
    for (const i of [...this.rendered]) this._evict(i);
    scroller.scrollTop = frac * scroller.scrollHeight;
    this._updateZoomLabel();
    this.onZoom?.(this.zoom);
    this._renderVisible();
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
    this.pages.removeEventListener("wheel", this._wheel);
    this.pages.parentElement?.replaceChildren?.();
  }
}
