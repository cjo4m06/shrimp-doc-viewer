// Plain-text / Markdown frontend. Parsed (markdown by default — a superset that
// also renders plain prose well) into a paginated rich-text flow and shown in the
// same page viewer as DOCX, with rasterization in the render Worker.

import { init, DEFAULT_FONT_URL } from "./index.js";
import { DocxViewer, resolveFontMap } from "./docx.js";
import { WorkerDoc } from "./worker-doc.js";

/**
 * Mount a Markdown / plain-text viewer into `container`.
 * @param {{ fontUrl?: string, fonts?: Record<string,string|Uint8Array|ArrayBuffer>,
 *   plain?: boolean, zoom?: number }} [opts]  pass `plain: true` to disable markdown.
 */
export async function renderTextInto(container, bytes, opts = {}) {
  await init();
  const fontUrl = opts.fontUrl || opts.cjkFallbackFontUrl || DEFAULT_FONT_URL;
  const fontBytes = new Uint8Array(await (await fetch(fontUrl)).arrayBuffer());
  const extra = await resolveFontMap(opts.fonts);
  const doc = await WorkerDoc.open(opts.plain ? "text" : "markdown", bytes, fontBytes, extra);
  return new DocxViewer(container, doc, opts);
}
