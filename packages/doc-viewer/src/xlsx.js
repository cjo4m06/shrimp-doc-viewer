// XLSX frontend — the first format rendered by our own Rust code through the
// shared geba (display-list IR + tiny-skia + the CJK text stack). The WASM core
// parses values (calamine) and lowers the sheet into a display list; here we
// just fetch a font, call it, and blit the RGBA to a canvas.

import { init } from "./index.js";
import { render_xlsx, xlsx_sheet_names } from "../wasm/dv_wasm.js";

/**
 * Render one sheet of an XLSX workbook into `container`.
 * @param {HTMLElement} container
 * @param {Uint8Array} bytes
 * @param {{ fontUrl?: string, cjkFallbackFontUrl?: string, sheetIndex?: number,
 *   maxRows?: number, maxCols?: number }} [opts]
 * @returns {Promise<{ sheetNames: string[], sheetIndex: number, width: number, height: number, destroy: () => void }>}
 */
export async function renderXlsxInto(container, bytes, opts = {}) {
  await init();

  const fontUrl = opts.fontUrl || opts.cjkFallbackFontUrl;
  if (!fontUrl) {
    throw new Error("renderXlsxInto: provide opts.fontUrl (a CJK-capable font, e.g. Noto Sans TC).");
  }
  const fontBytes = new Uint8Array(await (await fetch(fontUrl)).arrayBuffer());
  const sheetIndex = opts.sheetIndex || 0;

  const img = render_xlsx(bytes, fontBytes, sheetIndex, opts.maxRows || 0, opts.maxCols || 0);
  const width = img.width;
  const height = img.height;
  const data = img.takeData();
  img.free();

  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  canvas.style.display = "block";
  canvas.getContext("2d").putImageData(new ImageData(new Uint8ClampedArray(data), width, height), 0, 0);
  container.replaceChildren(canvas);

  let sheetNames = [];
  try { sheetNames = xlsx_sheet_names(bytes); } catch { /* ignore */ }

  return { sheetNames, sheetIndex, width, height, destroy: () => container.replaceChildren() };
}
