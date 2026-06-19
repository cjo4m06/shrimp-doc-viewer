// XLSX frontend — a virtualized, zoomable, multi-sheet viewer rendered by our
// own Rust code through the shared geba. The WASM `XlsxBook` parses the workbook
// once and renders only the cells in the current scroll window (with frozen
// column/row headers); JS drives scroll, zoom and sheet tabs.

import { init } from "./index.js";
import { XlsxBook } from "../wasm/dv_wasm.js";

const clamp = (v, lo, hi) => Math.max(lo, Math.min(hi, v));

/**
 * Mount a virtualized XLSX viewer into `container`.
 * @param {HTMLElement} container
 * @param {Uint8Array} bytes
 * @param {{ fontUrl?: string, cjkFallbackFontUrl?: string, sheetIndex?: number,
 *   zoom?: number, height?: string }} [opts]
 */
export async function renderXlsxInto(container, bytes, opts = {}) {
  await init();
  const fontUrl = opts.fontUrl || opts.cjkFallbackFontUrl;
  if (!fontUrl) {
    throw new Error("renderXlsxInto: provide opts.fontUrl (a CJK-capable font, e.g. Noto Sans TC).");
  }
  const fontBytes = new Uint8Array(await (await fetch(fontUrl)).arrayBuffer());
  const book = new XlsxBook(bytes, fontBytes);
  return new XlsxViewer(container, book, opts);
}

export class XlsxViewer {
  constructor(container, book, opts) {
    this.book = book;
    this.names = book.sheetNames();
    this.sheetIndex = opts.sheetIndex || 0;
    this.zoom = clamp(opts.zoom ?? 1, 0.25, 6);
    this.dpr = (typeof devicePixelRatio === "number" && devicePixelRatio) || 1;
    this.onZoom = opts.onZoom;
    this._raf = 0;

    container.replaceChildren();
    this._build(container, opts.height || "72vh");
    this._applyGeometry();
    this._render();
  }

  get sheetNames() {
    return this.names;
  }

  _build(container, height) {
    const bar = document.createElement("div");
    bar.style.cssText = "display:flex;gap:.4rem;align-items:center;flex-wrap:wrap;margin-bottom:.4rem;font:13px system-ui";

    // Sheet tabs.
    this.tabEls = [];
    this.names.forEach((name, i) => {
      const t = document.createElement("button");
      t.textContent = name || `Sheet${i + 1}`;
      t.addEventListener("click", () => this.setSheet(i));
      bar.appendChild(t);
      this.tabEls.push(t);
    });

    const spacerFlex = document.createElement("span");
    spacerFlex.style.flex = "1";
    bar.appendChild(spacerFlex);

    const mk = (label, fn) => {
      const b = document.createElement("button");
      b.textContent = label;
      b.addEventListener("click", fn);
      bar.appendChild(b);
      return b;
    };
    mk("−", () => this.zoomOut());
    this.zoomLabel = document.createElement("span");
    this.zoomLabel.style.cssText = "font:13px ui-monospace,monospace;min-width:3.2em;text-align:center";
    bar.appendChild(this.zoomLabel);
    mk("＋", () => this.zoomIn());
    mk("符合視窗", () => this.fitWidth());

    const scroll = document.createElement("div");
    scroll.style.cssText = `position:relative;overflow:auto;height:${height};background:#fff;border:1px solid #ddd;border-radius:6px`;
    const spacer = document.createElement("div");
    spacer.style.cssText = "position:relative";
    const canvas = document.createElement("canvas");
    canvas.style.cssText = "position:absolute;top:0;left:0;will-change:transform";
    scroll.appendChild(spacer);
    scroll.appendChild(canvas);

    container.appendChild(bar);
    container.appendChild(scroll);

    this.scroll = scroll;
    this.spacer = spacer;
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d");

    // Pin the canvas to the viewport on scroll, then re-render the window.
    this._onScroll = () => {
      this.canvas.style.transform = `translate(${this.scroll.scrollLeft}px, ${this.scroll.scrollTop}px)`;
      this._schedule();
    };
    scroll.addEventListener("scroll", this._onScroll, { passive: true });

    this._onWheel = (e) => {
      if (!e.ctrlKey && !e.metaKey) return;
      e.preventDefault();
      e.deltaY < 0 ? this.zoomIn() : this.zoomOut();
    };
    scroll.addEventListener("wheel", this._onWheel, { passive: false });

    this._ro = new ResizeObserver(() => this._schedule());
    this._ro.observe(scroll);

    this._updateTabs();
    this._updateZoomLabel();
  }

  _applyGeometry() {
    const [tw, th, hw, hh] = this.book.sheetGeometry(this.sheetIndex);
    this.tw = tw; this.th = th; this.hw = hw; this.hh = hh;
    this.spacer.style.width = (hw + tw) * this.zoom + "px";
    this.spacer.style.height = (hh + th) * this.zoom + "px";
  }

  _render() {
    const cw = this.scroll.clientWidth;
    const ch = this.scroll.clientHeight;
    if (cw <= 0 || ch <= 0) return;
    const sl = this.scroll.scrollLeft;
    const st = this.scroll.scrollTop;
    const scale = this.zoom * this.dpr;
    const devW = Math.max(1, Math.round(cw * this.dpr));
    const devH = Math.max(1, Math.round(ch * this.dpr));

    const img = this.book.renderViewport(this.sheetIndex, sl / this.zoom, st / this.zoom, devW, devH, scale);
    const w = img.width, h = img.height, data = img.takeData();
    img.free();

    this.canvas.width = w;
    this.canvas.height = h;
    this.canvas.style.width = cw + "px";
    this.canvas.style.height = ch + "px";
    this.canvas.style.transform = `translate(${sl}px, ${st}px)`;
    this.ctx.putImageData(new ImageData(new Uint8ClampedArray(data), w, h), 0, 0);
  }

  _schedule() {
    if (this._raf) return;
    this._raf = requestAnimationFrame(() => {
      this._raf = 0;
      this._render();
    });
  }

  setSheet(i) {
    if (i === this.sheetIndex || i < 0 || i >= this.names.length) return;
    this.sheetIndex = i;
    this.scroll.scrollTop = 0;
    this.scroll.scrollLeft = 0;
    this.canvas.style.transform = "translate(0,0)";
    this._applyGeometry();
    this._updateTabs();
    this._render();
  }

  setZoom(z) {
    const next = clamp(z, 0.25, 6);
    if (next === this.zoom) return;
    const fx = this.scroll.scrollLeft / Math.max(1, this.spacer.offsetWidth);
    const fy = this.scroll.scrollTop / Math.max(1, this.spacer.offsetHeight);
    this.zoom = next;
    this._applyGeometry();
    this.scroll.scrollLeft = fx * this.spacer.offsetWidth;
    this.scroll.scrollTop = fy * this.spacer.offsetHeight;
    this._updateZoomLabel();
    this.onZoom?.(this.zoom);
    this._render();
  }

  zoomIn() { this.setZoom(this.zoom * 1.25); }
  zoomOut() { this.setZoom(this.zoom / 1.25); }

  fitWidth() {
    const avail = this.scroll.clientWidth - 2;
    if (this.hw + this.tw > 0) this.setZoom(avail / (this.hw + this.tw));
  }

  _updateTabs() {
    this.tabEls.forEach((t, i) => {
      const on = i === this.sheetIndex;
      t.style.cssText = `padding:.2rem .6rem;border:1px solid #cbd2da;border-radius:4px;cursor:pointer;` +
        (on ? "background:#1f6feb;color:#fff;font-weight:600" : "background:#fff;color:#333");
    });
  }

  _updateZoomLabel() {
    if (this.zoomLabel) this.zoomLabel.textContent = Math.round(this.zoom * 100) + "%";
  }

  get zoom_() { return this.zoom; }

  destroy() {
    if (this._raf) cancelAnimationFrame(this._raf);
    this.scroll.removeEventListener("scroll", this._onScroll);
    this.scroll.removeEventListener("wheel", this._onWheel);
    this._ro?.disconnect();
    this.canvas.parentElement?.parentElement?.replaceChildren?.();
  }
}
