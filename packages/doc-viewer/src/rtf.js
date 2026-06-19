// RTF frontend — parsed into a paginated rich-text flow, rendered in the worker.

import { init, DEFAULT_FONT_URL } from "./index.js";
import { DocxViewer, resolveFontMap } from "./docx.js";
import { WorkerDoc } from "./worker-doc.js";

/**
 * Mount an RTF viewer into `container`.
 * @param {{ fontUrl?: string, fonts?: Record<string,string|Uint8Array|ArrayBuffer>, zoom?: number }} [opts]
 */
export async function renderRtfInto(container, bytes, opts = {}) {
  await init();
  const fontUrl = opts.fontUrl || opts.cjkFallbackFontUrl || DEFAULT_FONT_URL;
  const fontBytes = new Uint8Array(await (await fetch(fontUrl)).arrayBuffer());
  const extra = await resolveFontMap(opts.fonts);
  const doc = await WorkerDoc.open("rtf", bytes, fontBytes, extra);
  return new DocxViewer(container, doc, opts);
}
