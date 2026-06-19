// OpenDocument frontend. ODT/ODP lower into the rich-text flow (DOCX viewer); ODS
// reuses the XLSX grid viewer. The subtype is passed as `opts.odfKind`.

import { init } from "./index.js";
import { FlowDoc, XlsxBook } from "../wasm/dv_wasm.js";
import { DocxViewer } from "./docx.js";
import { XlsxViewer } from "./xlsx.js";

/**
 * Mount an ODF (.odt / .ods / .odp) viewer into `container`.
 * @param {{ odfKind?: "odt"|"ods"|"odp", fontUrl?: string,
 *   fonts?: Record<string,string|Uint8Array|ArrayBuffer>, zoom?: number }} [opts]
 */
export async function renderOdfInto(container, bytes, opts = {}) {
  await init();
  const fontUrl = opts.fontUrl || opts.cjkFallbackFontUrl;
  if (!fontUrl) throw new Error("renderOdfInto: provide opts.fontUrl (a CJK-capable font).");
  const fontBytes = new Uint8Array(await (await fetch(fontUrl)).arrayBuffer());

  if (opts.odfKind === "ods") {
    const book = XlsxBook.fromOds(bytes, fontBytes);
    return new XlsxViewer(container, book, opts);
  }
  const extra = [];
  for (const [name, src] of Object.entries(opts.fonts || {})) {
    const u8 = src instanceof Uint8Array ? src : src instanceof ArrayBuffer ? new Uint8Array(src) : new Uint8Array(await (await fetch(src)).arrayBuffer());
    extra.push([name, u8]);
  }
  const doc = opts.odfKind === "odp" ? FlowDoc.fromOdp(bytes, fontBytes, extra) : FlowDoc.fromOdt(bytes, fontBytes, extra);
  return new DocxViewer(container, doc, opts);
}
