// Plain-text / Markdown frontend. The bytes are parsed (markdown by default — a
// superset that also renders plain prose well) into a paginated rich-text flow and
// shown in the same page viewer as DOCX.

import { init } from "./index.js";
import { FlowDoc } from "../wasm/dv_wasm.js";
import { DocxViewer } from "./docx.js";

async function fontParts(opts) {
  const fontUrl = opts.fontUrl || opts.cjkFallbackFontUrl;
  if (!fontUrl) throw new Error("provide opts.fontUrl (a CJK-capable font, e.g. Noto Sans TC).");
  const fontBytes = new Uint8Array(await (await fetch(fontUrl)).arrayBuffer());
  const extra = [];
  for (const [name, src] of Object.entries(opts.fonts || {})) {
    let u8;
    if (src instanceof Uint8Array) u8 = src;
    else if (src instanceof ArrayBuffer) u8 = new Uint8Array(src);
    else u8 = new Uint8Array(await (await fetch(src)).arrayBuffer());
    extra.push([name, u8]);
  }
  return { fontBytes, extra };
}

/**
 * Mount a Markdown / plain-text viewer into `container`.
 * @param {{ fontUrl?: string, fonts?: Record<string,string|Uint8Array|ArrayBuffer>,
 *   plain?: boolean, zoom?: number }} [opts]  pass `plain: true` to disable markdown.
 */
export async function renderTextInto(container, bytes, opts = {}) {
  await init();
  const { fontBytes, extra } = await fontParts(opts);
  const doc = opts.plain ? FlowDoc.fromText(bytes, fontBytes, extra) : FlowDoc.fromMarkdown(bytes, fontBytes, extra);
  return new DocxViewer(container, doc, opts);
}
