// Image frontend — the browser decodes the bytes natively (PNG/JPEG/GIF/WebP/
// BMP/SVG), so this is a thin pure-JS viewer: a checkerboard backdrop, the image
// centred, and zoom (buttons + Ctrl/⌘-wheel + fit). No WASM needed.

const MIME = [
  [[0x89, 0x50, 0x4e, 0x47], "image/png"],
  [[0xff, 0xd8, 0xff], "image/jpeg"],
  [[0x47, 0x49, 0x46], "image/gif"],
  [[0x42, 0x4d], "image/bmp"],
];

/** Sniff an image MIME type from magic bytes (null if not a known image). */
export function sniffImage(bytes) {
  for (const [sig, mime] of MIME) {
    if (sig.every((b, i) => bytes[i] === b)) return mime;
  }
  // RIFF....WEBP
  if (bytes[0] === 0x52 && bytes[1] === 0x49 && bytes[2] === 0x46 && bytes[3] === 0x46 && bytes[8] === 0x57 && bytes[9] === 0x45 && bytes[10] === 0x42 && bytes[11] === 0x50) {
    return "image/webp";
  }
  // SVG (text): look for "<svg" near the start, possibly after an XML/BOM prolog.
  const head = new TextDecoder("utf-8").decode(bytes.subarray(0, 512)).toLowerCase();
  if (head.includes("<svg")) return "image/svg+xml";
  return null;
}

const clamp = (v, lo, hi) => Math.max(lo, Math.min(hi, v));

/**
 * Mount an image viewer into `container`.
 * @param {HTMLElement} container
 * @param {Uint8Array} bytes
 * @param {{ zoom?: number, height?: string, mime?: string }} [opts]
 */
export async function renderImageInto(container, bytes, opts = {}) {
  const mime = opts.mime || sniffImage(bytes) || "application/octet-stream";
  const blob = new Blob([bytes], { type: mime });
  const url = URL.createObjectURL(blob);
  return new ImageViewer(container, url, opts);
}

class ImageViewer {
  constructor(container, url, opts) {
    this.url = url;
    this.zoom = clamp(opts.zoom ?? 1, 0.05, 32);
    this.fitMode = opts.zoom == null; // start fit-to-width unless an explicit zoom given
    this.onZoom = opts.onZoom;
    container.replaceChildren();
    this._build(container, opts.height || "75vh");
  }

  _build(container, height) {
    const bar = document.createElement("div");
    bar.style.cssText = "display:flex;gap:.4rem;align-items:center;margin-bottom:.5rem;font:13px system-ui";
    const mk = (t, fn) => { const b = document.createElement("button"); b.textContent = t; b.onclick = fn; return b; };
    this.pct = document.createElement("span");
    this.pct.style.cssText = "font:13px ui-monospace,monospace;min-width:4em;text-align:center";
    bar.append(
      mk("−", () => this.setZoom(this.zoom / 1.25)),
      this.pct,
      mk("+", () => this.setZoom(this.zoom * 1.25)),
      mk("符合寬度", () => this.fitWidth()),
      mk("原始大小", () => this.setZoom(1)),
    );

    this.scroll = document.createElement("div");
    this.scroll.style.cssText = `height:${height};overflow:auto;border:1px solid #d8dadf;border-radius:8px;` +
      "background:#fff conic-gradient(#eee 90deg,#fff 90deg 180deg,#eee 180deg 270deg,#fff 270deg) 0 0/24px 24px;" +
      "display:flex;align-items:flex-start;justify-content:center";

    this.img = document.createElement("img");
    this.img.style.cssText = "display:block;image-rendering:auto;max-width:none";
    this.img.alt = "image";
    this.img.onload = () => {
      this.natW = this.img.naturalWidth || 1;
      this.natH = this.img.naturalHeight || 1;
      if (this.fitMode) this.fitWidth();
      else this._apply();
      // the bitmap is decoded into the <img>; free the blob URL (no leak on re-mount)
      URL.revokeObjectURL(this.url);
      this._revoked = true;
    };
    this.img.src = this.url;
    this.scroll.appendChild(this.img);

    this.scroll.addEventListener("wheel", (e) => {
      if (!(e.ctrlKey || e.metaKey)) return;
      e.preventDefault();
      this.fitMode = false;
      this.setZoom(this.zoom * (e.deltaY < 0 ? 1.1 : 1 / 1.1));
    }, { passive: false });

    container.append(bar, this.scroll);
  }

  _apply() {
    this.img.style.width = `${Math.round(this.natW * this.zoom)}px`;
    this.img.style.height = "auto";
    if (this.pct) this.pct.textContent = `${Math.round(this.zoom * 100)}%`;
    if (this.onZoom) this.onZoom(this.zoom);
  }

  setZoom(z) {
    this.fitMode = false;
    this.zoom = clamp(z, 0.05, 32);
    this._apply();
  }

  fitWidth() {
    this.fitMode = true;
    const avail = this.scroll.clientWidth - 4;
    if (this.natW) this.zoom = clamp(avail / this.natW, 0.05, 32);
    this._apply();
  }

  destroy() {
    if (!this._revoked) URL.revokeObjectURL(this.url);
  }
}
