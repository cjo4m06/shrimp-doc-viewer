// OpenDocument frontend. ODT/ODP lower into the rich-text flow (DOCX viewer, with
// worker rasterization); ODS reuses the XLSX grid viewer. Subtype in `opts.odfKind`.

import { init, DEFAULT_FONT_URL } from "./index.js";
import { XlsxBook } from "../wasm/dv_wasm.js";
import { DocxViewer, resolveFontMap } from "./docx.js";
import { XlsxViewer } from "./xlsx.js";
import { WorkerDoc } from "./worker-doc.js";

/**
 * Mount an ODF (.odt / .ods / .odp) viewer into `container`.
 * @param {{ odfKind?: "odt"|"ods"|"odp", fontUrl?: string,
 *   fonts?: Record<string,string|Uint8Array|ArrayBuffer>, zoom?: number }} [opts]
 */
export async function renderOdfInto(container, bytes, opts = {}) {
  await init();
  const fontUrl = opts.fontUrl || opts.cjkFallbackFontUrl || DEFAULT_FONT_URL;
  const fontBytes = new Uint8Array(await (await fetch(fontUrl)).arrayBuffer());

  if (opts.odfKind === "ods") {
    const book = XlsxBook.fromOds(bytes, fontBytes);
    return new XlsxViewer(container, book, opts);
  }
  const extra = await resolveFontMap(opts.fonts);
  const doc = await WorkerDoc.open(opts.odfKind === "odp" ? "odp" : "odt", bytes, fontBytes, extra);
  return new DocxViewer(container, doc, opts);
}
