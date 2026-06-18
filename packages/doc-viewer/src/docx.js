// DOCX frontend — our own Rust flow-layout engine renders the document through
// the shared geba and we blit the resulting page to a canvas.

import { init } from "./index.js";
import { render_docx } from "../wasm/dv_wasm.js";

/**
 * Render a DOCX into `container` as a single continuous page.
 * @param {HTMLElement} container
 * @param {Uint8Array} bytes
 * @param {{ fontUrl?: string, cjkFallbackFontUrl?: string }} [opts]
 * @returns {Promise<{ width: number, height: number, destroy: () => void }>}
 */
export async function renderDocxInto(container, bytes, opts = {}) {
  await init();

  const fontUrl = opts.fontUrl || opts.cjkFallbackFontUrl;
  if (!fontUrl) {
    throw new Error("renderDocxInto: provide opts.fontUrl (a CJK-capable font, e.g. Noto Sans TC).");
  }
  const fontBytes = new Uint8Array(await (await fetch(fontUrl)).arrayBuffer());

  const img = render_docx(bytes, fontBytes);
  const width = img.width;
  const height = img.height;
  const data = img.takeData();
  img.free();

  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  canvas.style.display = "block";
  canvas.style.maxWidth = "100%";
  canvas.getContext("2d").putImageData(new ImageData(new Uint8ClampedArray(data), width, height), 0, 0);
  container.replaceChildren(canvas);

  return { width, height, destroy: () => container.replaceChildren() };
}
