// PDFium render worker. Keeps one document open and renders pages on demand,
// streaming each back as a transferable ImageBitmap — so virtualized scrolling
// and zoom never touch the main thread's render loop. Reuses the same engine
// wrapper as the main thread (openDocument uses only fetch/ImageData/
// createImageBitmap, all available in workers).
//
// Workers don't honour the document's import map, so the caller passes
// `embedpdfUrl` (+ absolute `pdfiumWasmUrl` / `cjkFallbackFontUrl`). Bundlers
// resolve the worker and its bare `@embedpdf/pdfium` import automatically.

import { openDocument } from "./pdf.js";

let _doc = null;

self.onmessage = async (e) => {
  const m = e.data;
  if (!m || typeof m.id === "undefined") return;
  try {
    if (m.type === "open") {
      _doc?.destroy();
      _doc = await openDocument(new Uint8Array(m.bytes), m.opts || {});
      const sizes = [];
      for (let i = 0; i < _doc.pageCount; i++) sizes.push(_doc.pageSize(i));
      self.postMessage({ id: m.id, type: "opened", pageCount: _doc.pageCount, sizes });
    } else if (m.type === "render") {
      const { imageData, width, height } = _doc.renderPageToImageData(m.index, m.scale);
      const bitmap = await createImageBitmap(imageData);
      self.postMessage({ id: m.id, type: "rendered", index: m.index, width, height, bitmap }, [bitmap]);
    } else if (m.type === "close") {
      _doc?.destroy();
      _doc = null;
      self.postMessage({ id: m.id, type: "closed" });
    }
  } catch (err) {
    self.postMessage({
      id: m.id,
      type: "error",
      message: String((err && err.message) || err),
      code: err && err.code,
    });
  }
};
