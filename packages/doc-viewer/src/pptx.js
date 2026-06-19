// PPTX frontend — our own Rust DrawingML renderer draws one slide at a time
// through the shared geba; JS provides slide navigation and fit-to-width sizing.

import { init } from "./index.js";
import { PptxDeck } from "../wasm/dv_wasm.js";

/**
 * Mount a PPTX slide viewer into `container`.
 * @param {HTMLElement} container
 * @param {Uint8Array} bytes
 * @param {{ fontUrl?: string, cjkFallbackFontUrl?: string, fonts?: Record<string,string|Uint8Array|ArrayBuffer>, slideIndex?: number }} [opts]
 *
 * `opts.fonts` maps a declared typeface to a font file (URL | Uint8Array | ArrayBuffer);
 * runs naming that face render with it (e.g. `{ "標楷體": "/fonts/BiauKai.ttf" }`).
 * PowerPoint-embedded fonts are MicroType-Express EOT and cannot be loaded.
 */
export async function renderPptxInto(container, bytes, opts = {}) {
  await init();
  const fontUrl = opts.fontUrl || opts.cjkFallbackFontUrl;
  if (!fontUrl) {
    throw new Error("renderPptxInto: provide opts.fontUrl (a CJK-capable font, e.g. Noto Sans TC).");
  }
  const fontBytes = new Uint8Array(await (await fetch(fontUrl)).arrayBuffer());
  const extra = [];
  for (const [name, src] of Object.entries(opts.fonts || {})) {
    let u8;
    if (src instanceof Uint8Array) u8 = src;
    else if (src instanceof ArrayBuffer) u8 = new Uint8Array(src);
    else u8 = new Uint8Array(await (await fetch(src)).arrayBuffer());
    extra.push([name, u8]);
  }
  const deck = new PptxDeck(bytes, fontBytes, extra);
  return new PptxViewer(container, deck, opts);
}

class PptxViewer {
  constructor(container, deck, opts) {
    this.deck = deck;
    this.count = deck.slideCount();
    const [w, h] = deck.slideSize();
    this.slideW = w;
    this.slideH = h;
    this.idx = Math.min(opts.slideIndex || 0, Math.max(0, this.count - 1));
    this.dpr = (typeof devicePixelRatio === "number" && devicePixelRatio) || 1;

    container.replaceChildren();
    this._build(container);
    this._render();
  }

  get slideCount() { return this.count; }
  get slideIndex() { return this.idx; }

  _build(container) {
    const bar = document.createElement("div");
    bar.style.cssText = "display:flex;gap:.5rem;align-items:center;margin-bottom:.5rem;font:13px system-ui";
    const mk = (label, fn) => {
      const b = document.createElement("button");
      b.textContent = label;
      b.style.cssText = "padding:.2rem .7rem;border:1px solid #cbd2da;border-radius:4px;cursor:pointer;background:#fff";
      b.addEventListener("click", fn);
      bar.appendChild(b);
      return b;
    };
    mk("‹ 上一張", () => this.prev());
    this.counter = document.createElement("span");
    this.counter.style.cssText = "font:13px ui-monospace,monospace;min-width:5em;text-align:center";
    bar.appendChild(this.counter);
    mk("下一張 ›", () => this.next());

    const stage = document.createElement("div");
    stage.style.cssText = "background:#e9ecef;padding:12px;border-radius:6px;overflow:auto";
    const canvas = document.createElement("canvas");
    canvas.style.cssText = "display:block;margin:0 auto;box-shadow:0 2px 12px rgba(0,0,0,.25);background:#fff;max-width:100%";
    stage.appendChild(canvas);

    container.appendChild(bar);
    container.appendChild(stage);

    this.stage = stage;
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d");

    this._ro = new ResizeObserver(() => this._render());
    this._ro.observe(stage);
  }

  _render() {
    const avail = Math.max(64, this.stage.clientWidth - 24);
    const cssScale = Math.min(avail / this.slideW, 2); // fit width, cap at 2×
    const scale = cssScale * this.dpr;

    const img = this.deck.renderSlide(this.idx, scale);
    const w = img.width, h = img.height, data = img.takeData();
    img.free();

    this.canvas.width = w;
    this.canvas.height = h;
    this.canvas.style.width = w / this.dpr + "px";
    this.canvas.style.height = h / this.dpr + "px";
    this.ctx.putImageData(new ImageData(new Uint8ClampedArray(data), w, h), 0, 0);
    this.counter.textContent = `${this.idx + 1} / ${this.count}`;
  }

  goTo(i) {
    const n = Math.max(0, Math.min(this.count - 1, i));
    if (n === this.idx) return;
    this.idx = n;
    this._render();
  }
  next() { this.goTo(this.idx + 1); }
  prev() { this.goTo(this.idx - 1); }

  destroy() {
    this._ro?.disconnect();
    this.stage.parentElement?.replaceChildren?.();
  }
}
