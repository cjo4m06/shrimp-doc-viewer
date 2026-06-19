// CSV frontend — parses delimited text (CSV / TSV / semicolon) into a single
// grid sheet and renders it through the same virtualized, zoomable viewer as
// XLSX. The WASM `XlsxBook.fromCsv` builds the grid once; JS drives scroll/zoom.

import { init } from "./index.js";
import { XlsxBook } from "../wasm/dv_wasm.js";
import { XlsxViewer } from "./xlsx.js";

/**
 * Mount a virtualized CSV viewer into `container`.
 * @param {HTMLElement} container
 * @param {Uint8Array} bytes
 * @param {{ fontUrl?: string, cjkFallbackFontUrl?: string, zoom?: number, height?: string }} [opts]
 */
export async function renderCsvInto(container, bytes, opts = {}) {
  await init();
  const fontUrl = opts.fontUrl || opts.cjkFallbackFontUrl;
  if (!fontUrl) {
    throw new Error("renderCsvInto: provide opts.fontUrl (a CJK-capable font, e.g. Noto Sans TC).");
  }
  const fontBytes = new Uint8Array(await (await fetch(fontUrl)).arrayBuffer());
  const book = XlsxBook.fromCsv(bytes, fontBytes);
  return new XlsxViewer(container, book, opts);
}
