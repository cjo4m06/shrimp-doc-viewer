// Public JS API for doc-viewer — a pure-JS surface over the Rust/WASM core.
//
// The WASM module is loaded lazily and exactly once. Fonts and the .wasm binary
// are separate, cacheable assets (never bundled into JS), matching the
// lazy-load architecture the engine is built around.

import initWasm, { render_text_demo, version as wasmVersion } from "../wasm/dv_wasm.js";

let _ready = null;

/**
 * Load & instantiate the WASM core (idempotent).
 * @param {{ wasmUrl?: string | URL }} [options] Override the .wasm location.
 * @returns {Promise<{ version: string }>}
 */
export function init(options = {}) {
  if (!_ready) {
    const arg = options.wasmUrl ? { module_or_path: options.wasmUrl } : undefined;
    _ready = initWasm(arg).then(() => ({ version: wasmVersion() }));
  }
  return _ready;
}

/**
 * M1 geba demo: shape `text` in the font at `fontUrl` and paint it onto `canvas`.
 * Proves the shape → outline → tiny-skia raster pipeline (incl. 繁體中文)
 * end-to-end in the browser.
 *
 * @param {HTMLCanvasElement} canvas
 * @param {{ fontUrl: string, text: string, size?: number, x?: number, baseline?: number }} opts
 */
export async function renderToCanvas(canvas, { fontUrl, text, size = 56, x = 24, baseline } = {}) {
  await init();
  const fontBytes = new Uint8Array(await (await fetch(fontUrl)).arrayBuffer());
  const w = canvas.width;
  const h = canvas.height;
  const bl = baseline ?? Math.round(h * 0.62);
  const rgba = render_text_demo(w, h, fontBytes, text, size, x, bl);
  const ctx = canvas.getContext("2d");
  const image = new ImageData(new Uint8ClampedArray(rgba), w, h);
  ctx.putImageData(image, 0, 0);
}

/** Normalise any supported source into a `Uint8Array`. */
async function toBytes(source) {
  if (source instanceof Uint8Array) return source;
  if (source instanceof ArrayBuffer) return new Uint8Array(source);
  if (typeof Blob !== "undefined" && source instanceof Blob) {
    return new Uint8Array(await source.arrayBuffer());
  }
  if (typeof source === "string" || source instanceof URL) {
    return new Uint8Array(await (await fetch(source)).arrayBuffer());
  }
  throw new TypeError("mount(): source must be a Uint8Array, ArrayBuffer, Blob, or URL.");
}

/**
 * Distinguish an OOXML zip (docx/xlsx/pptx) by the part names stored in the zip.
 * Zip filenames sit uncompressed in the local headers, so a raw byte scan works.
 */
export function sniffOoxml(bytes) {
  const text = new TextDecoder("latin1").decode(bytes);
  if (text.includes("xl/workbook.xml")) return "xlsx";
  if (text.includes("word/document.xml")) return "docx";
  if (text.includes("ppt/presentation.xml")) return "pptx";
  return "ooxml";
}

/** Detect document format from magic bytes. */
export function sniffFormat(bytes) {
  if (bytes.length >= 4 && bytes[0] === 0x25 && bytes[1] === 0x50 && bytes[2] === 0x44 && bytes[3] === 0x46) {
    return "pdf"; // %PDF
  }
  if (bytes.length >= 4 && bytes[0] === 0x50 && bytes[1] === 0x4b && bytes[2] === 0x03 && bytes[3] === 0x04) {
    return "ooxml"; // PK.. (docx/xlsx/pptx zip) — distinguished by [Content_Types].xml later
  }
  if (bytes.length >= 4 && bytes[0] === 0xd0 && bytes[1] === 0xcf && bytes[2] === 0x11 && bytes[3] === 0xe0) {
    return "ole"; // legacy .doc/.xls/.ppt compound file
  }
  // No binary magic: if it's plausibly text (no NULs, few control bytes), treat
  // it as delimited text (CSV / TSV / semicolon).
  const sample = bytes.subarray(0, 4096);
  let ctrl = 0;
  for (const b of sample) {
    if (b === 0) return "unknown";
    if (b < 9 || (b > 13 && b < 32)) ctrl++;
  }
  if (sample.length > 0 && ctrl < sample.length * 0.05) return "csv";
  return "unknown";
}

/**
 * The real entry point: render a document `source` into `target`.
 *
 * M2 wires the PDF frontend (PDFium). Office (XLSX/DOCX/PPTX) and legacy binary
 * formats are detected but not yet rendered — they throw a clear error rather
 * than pretending support.
 *
 * @param {HTMLElement} target Container to render pages into.
 * @param {Blob|ArrayBuffer|Uint8Array|string|URL} source Document bytes or URL.
 * @param {{ scale?: number, pdfiumWasmUrl?: string, onProgress?: (n:number,total:number)=>void }} [options]
 * @returns {Promise<{ pageCount: number, destroy: () => void }>}
 */
export async function mount(target, source, options = {}) {
  const bytes = await toBytes(source);
  const format = options.format || sniffFormat(bytes);
  if (format === "pdf") {
    const { renderPdfInto } = await import("./pdf.js");
    return renderPdfInto(target, bytes, options);
  }
  if (format === "csv") {
    const { renderCsvInto } = await import("./csv.js");
    return renderCsvInto(target, bytes, options);
  }
  if (format === "ooxml") {
    const sub = sniffOoxml(bytes);
    if (sub === "xlsx") {
      const { renderXlsxInto } = await import("./xlsx.js");
      return renderXlsxInto(target, bytes, options);
    }
    if (sub === "docx") {
      const { renderDocxInto } = await import("./docx.js");
      return renderDocxInto(target, bytes, options);
    }
    if (sub === "pptx") {
      const { renderPptxInto } = await import("./pptx.js");
      return renderPptxInto(target, bytes, options);
    }
    throw new Error(`doc-viewer.mount(): detected "${sub}" — PDF, XLSX, DOCX and PPTX are wired.`);
  }
  throw new Error(
    `doc-viewer.mount(): detected "${format}" — supported: PDF, XLSX, DOCX, PPTX, CSV. ` +
      `Legacy binary (.doc/.xls/.ppt) is out of scope.`
  );
}

export { wasmVersion as coreVersion };
