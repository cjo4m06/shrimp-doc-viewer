// Off-main-thread renderer for the paged formats (DOCX + markdown/txt/rtf/odt/odp
// via FlowDoc). A plain module Worker — single-threaded WASM, no SharedArrayBuffer,
// so it needs NO COOP/COEP headers and does not restrict the page's external loads.
// The page document lives here; the main thread posts render requests and gets back
// transferable ImageBitmaps to blit. This keeps rasterization off the UI thread.

import init, { DocxDoc, FlowDoc } from "../wasm/dv_wasm.js";

const ready = init();
let doc = null;

function open(kind, bytes, font, extra) {
  switch (kind) {
    case "docx":
      return new DocxDoc(bytes, font, extra);
    case "rtf":
      return FlowDoc.fromRtf(bytes, font, extra);
    case "odt":
      return FlowDoc.fromOdt(bytes, font, extra);
    case "odp":
      return FlowDoc.fromOdp(bytes, font, extra);
    case "text":
      return FlowDoc.fromText(bytes, font, extra);
    default: // markdown
      return FlowDoc.fromMarkdown(bytes, font, extra);
  }
}

self.onmessage = async (e) => {
  const m = e.data;
  try {
    await ready;
    if (m.type === "open") {
      doc = open(m.kind, m.bytes, m.font, m.extra || []);
      const [pw, ph] = doc.pageSize();
      self.postMessage({ type: "opened", pageCount: doc.pageCount(), pw, ph });
    } else if (m.type === "render") {
      if (!doc) return;
      const img = doc.renderPage(m.page, m.scale);
      const w = img.width, h = img.height;
      const data = img.takeData();
      img.free();
      const bitmap = await createImageBitmap(new ImageData(new Uint8ClampedArray(data), w, h));
      self.postMessage({ type: "rendered", reqId: m.reqId, page: m.page, w, h, bitmap }, [bitmap]);
    }
  } catch (err) {
    self.postMessage({ type: "error", reqId: m.reqId, message: String(err && err.message ? err.message : err) });
  }
};
