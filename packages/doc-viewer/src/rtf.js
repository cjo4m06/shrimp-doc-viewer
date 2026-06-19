// RTF frontend — parsed into a paginated rich-text flow, shown in the DOCX viewer.

import { init } from "./index.js";
import { FlowDoc } from "../wasm/dv_wasm.js";
import { DocxViewer } from "./docx.js";

/**
 * Mount an RTF viewer into `container`.
 * @param {{ fontUrl?: string, fonts?: Record<string,string|Uint8Array|ArrayBuffer>, zoom?: number }} [opts]
 */
export async function renderRtfInto(container, bytes, opts = {}) {
  await init();
  const fontUrl = opts.fontUrl || opts.cjkFallbackFontUrl;
  if (!fontUrl) throw new Error("renderRtfInto: provide opts.fontUrl (a CJK-capable font).");
  const fontBytes = new Uint8Array(await (await fetch(fontUrl)).arrayBuffer());
  const extra = [];
  for (const [name, src] of Object.entries(opts.fonts || {})) {
    const u8 = src instanceof Uint8Array ? src : src instanceof ArrayBuffer ? new Uint8Array(src) : new Uint8Array(await (await fetch(src)).arrayBuffer());
    extra.push([name, u8]);
  }
  const doc = FlowDoc.fromRtf(bytes, fontBytes, extra);
  return new DocxViewer(container, doc, opts);
}
