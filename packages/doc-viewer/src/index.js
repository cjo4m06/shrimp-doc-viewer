// Public JS API for ShrimpDocViewer — a pure-JS surface over the Rust/WASM core.
//
// The WASM module is loaded lazily and exactly once. Fonts and the .wasm binary
// are separate, cacheable assets (fetched at runtime, never inlined into JS) — the
// package ships a default font that's resolved by URL and loaded on first use.

import initWasm, { render_text_demo, version as wasmVersion } from "../wasm/dv_wasm.js";

let _ready = null;

/**
 * The bundled default font (Noto Sans TC, SIL OFL 1.1 — free for commercial use).
 * Used as the fallback when the caller passes no `fontUrl`, so `mount()` works with
 * zero configuration. Override per call with `fontUrl`, or map specific families
 * with `fonts`. Resolved relative to this module so bundlers copy it automatically.
 */
export const DEFAULT_FONT_URL = new URL("../fonts/NotoSansTC-VF.ttf", import.meta.url).href;

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
 * Distinguish a zip-based document (OOXML docx/xlsx/pptx OR ODF odt/ods/odp) by
 * the part names stored in the zip. Zip filenames sit uncompressed in the local
 * headers, so a raw byte scan works.
 */
export function sniffOoxml(bytes) {
  // Scan the whole archive: part names live in local headers (scattered through the
  // file, after any leading media) and in the central directory at the end, so a
  // small prefix window misses them on real documents.
  const text = new TextDecoder("latin1").decode(bytes);
  if (text.includes("xl/workbook.xml")) return "xlsx";
  if (text.includes("word/document.xml")) return "docx";
  if (text.includes("ppt/presentation.xml")) return "pptx";
  // ODF: a "mimetype" entry declares the subtype near the very start of the zip.
  if (text.includes("opendocument.spreadsheet")) return "ods";
  if (text.includes("opendocument.text")) return "odt";
  if (text.includes("opendocument.presentation")) return "odp";
  return "zip"; // an unrecognized zip (epub/jar/…) → clean "unsupported" error
}

/** Distinguish delimited tabular text (csv) from prose (markdown / plain text). */
function looksTabular(bytes) {
  const text = new TextDecoder("utf-8").decode(bytes.subarray(0, 8192));
  const lines = text.split(/\r?\n/).filter((l) => l.trim()).slice(0, 12);
  if (lines.length < 2) return false;
  // markdown / prose signals → not a table
  const md = lines.filter((l) => /^(#{1,6}\s|>\s|[-*+]\s|\d+[.)]\s|```|~~~|\|)/.test(l.trimStart())).length;
  if (md >= Math.max(1, lines.length * 0.3)) return false;
  // tab/semicolon are unambiguous; comma last (and rejected if it reads like prose)
  for (const delim of ["\t", ";", ","]) {
    const counts = lines.map((l) => l.split(delim).length - 1);
    const withDelim = counts.filter((n) => n >= 1).length;
    if (withDelim < lines.length * 0.8) continue;
    const max = Math.max(...counts);
    const exact = counts.filter((n) => n === max).length; // require an EXACT, stable column count
    if (max >= 1 && exact >= lines.length * 0.8) {
      if (delim === "," && max < 2 && lines.filter((l) => /, /.test(l)).length > lines.length * 0.5) continue;
      return true;
    }
  }
  return false;
}

/** Detect document format from magic bytes / content. */
export function sniffFormat(bytes) {
  const m = (i, ...sig) => sig.every((b, k) => bytes[i + k] === b);
  if (m(0, 0x25, 0x50, 0x44, 0x46)) return "pdf"; // %PDF
  if (m(0, 0x50, 0x4b, 0x03, 0x04)) return "zip"; // PK.. (OOXML or ODF)
  if (m(0, 0xd0, 0xcf, 0x11, 0xe0)) return "ole"; // legacy .doc/.xls/.ppt
  if (m(0, 0x89, 0x50, 0x4e, 0x47) || m(0, 0xff, 0xd8, 0xff) || m(0, 0x47, 0x49, 0x46) || m(0, 0x42, 0x4d) || (m(0, 0x52, 0x49, 0x46, 0x46) && m(8, 0x57, 0x45, 0x42, 0x50))) {
    return "image";
  }
  // text-based formats
  const head = new TextDecoder("utf-8").decode(bytes.subarray(0, 512));
  const trimmed = head.replace(/^﻿/, "").trimStart();
  if (trimmed.startsWith("{\\rtf")) return "rtf";
  // SVG: <svg> anywhere in the head (after an optional xml prolog / doctype / comments)
  if (trimmed.toLowerCase().includes("<svg")) return "image";
  const sample = bytes.subarray(0, 4096);
  let ctrl = 0;
  for (const b of sample) {
    if (b === 0) return "unknown";
    if (b < 9 || (b > 13 && b < 32)) ctrl++;
  }
  if (sample.length > 0 && ctrl < sample.length * 0.05) {
    return looksTabular(bytes) ? "csv" : "text"; // text = markdown / plain text
  }
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
/** Transcode UTF-16 (BOM or NUL pattern) to UTF-8 bytes; else return input unchanged. */
function transcodeIfUtf16(bytes) {
  const utf8 = (s) => new TextEncoder().encode(s);
  if (bytes.length >= 2 && bytes[0] === 0xff && bytes[1] === 0xfe) return utf8(new TextDecoder("utf-16le").decode(bytes));
  if (bytes.length >= 2 && bytes[0] === 0xfe && bytes[1] === 0xff) return utf8(new TextDecoder("utf-16be").decode(bytes));
  const n = Math.min(bytes.length, 512);
  let evenNul = 0, oddNul = 0;
  for (let i = 0; i < n; i++) if (bytes[i] === 0) (i % 2 === 0 ? evenNul++ : oddNul++);
  if (n >= 16) {
    if (oddNul > n / 4 && evenNul < 2) return utf8(new TextDecoder("utf-16le").decode(bytes));
    if (evenNul > n / 4 && oddNul < 2) return utf8(new TextDecoder("utf-16be").decode(bytes));
  }
  return bytes;
}

export async function mount(target, source, options = {}) {
  let bytes = await toBytes(source);
  let format = options.format || sniffFormat(bytes);
  if (format === "unknown") {
    // maybe UTF-16 text (NUL bytes make the byte scan bail) — transcode + re-sniff
    const t = transcodeIfUtf16(bytes);
    if (t !== bytes) {
      bytes = t;
      format = sniffFormat(bytes);
    }
  }
  if (format === "zip") format = sniffOoxml(bytes); // resolve OOXML/ODF subtype

  try {
    return await dispatch(target, bytes, format, options);
  } catch (e) {
    // surface a clean error instead of a raw wasm-bindgen panic / rejection
    const msg = e && e.message ? e.message : String(e);
    throw new Error(`ShrimpDocViewer.mount(): failed to render "${format}": ${msg}`);
  }
}

async function dispatch(target, bytes, format, options) {
  switch (format) {
    case "pdf":
      return (await import("./pdf.js")).renderPdfInto(target, bytes, options);
    case "image":
      return (await import("./image.js")).renderImageInto(target, bytes, options);
    case "csv":
      return (await import("./csv.js")).renderCsvInto(target, bytes, options);
    case "text":
    case "markdown":
    case "md":
      return (await import("./text.js")).renderTextInto(target, bytes, options);
    case "rtf":
      return (await import("./rtf.js")).renderRtfInto(target, bytes, options);
    case "xlsx":
      return (await import("./xlsx.js")).renderXlsxInto(target, bytes, options);
    case "docx":
      return (await import("./docx.js")).renderDocxInto(target, bytes, options);
    case "pptx":
      return (await import("./pptx.js")).renderPptxInto(target, bytes, options);
    case "odt":
    case "ods":
    case "odp":
      return (await import("./odf.js")).renderOdfInto(target, bytes, { ...options, odfKind: format });
    default:
      throw new Error(
        `ShrimpDocViewer.mount(): detected "${format}" — supported: PDF · DOCX · XLSX · PPTX · ` +
          `CSV · TXT/Markdown · RTF · ODF (odt/ods/odp) · images. ` +
          `Legacy binary (.doc/.xls/.ppt) is out of scope (convert server-side). ` +
          `If this format should be supported, or a file fails to render, please open an issue ` +
          `with the smallest file that reproduces it: https://github.com/cjo4m06/shrimp-doc-viewer/issues`
      );
  }
}

export { wasmVersion as coreVersion };
