// A page document that lives in a render Worker. Exposes a small async API the
// DocxViewer uses: pageCount()/pageSize() (resolved at open) + renderPage() which
// returns a Promise<ImageBitmap>. One worker per document; terminated on destroy.

export class WorkerDoc {
  /**
   * @param {"docx"|"markdown"|"text"|"rtf"|"odt"|"odp"} kind
   * @param {Uint8Array} bytes  (transferred to the worker)
   * @param {Uint8Array} font   (transferred to the worker)
   * @param {Array<[string, Uint8Array]>} extra  caller fonts
   */
  static open(kind, bytes, font, extra = []) {
    return new Promise((resolve, reject) => {
      const worker = new Worker(new URL("./render-worker.js", import.meta.url), { type: "module" });
      const self = new WorkerDoc(worker);
      worker.onmessage = (e) => {
        const m = e.data;
        if (m.type === "opened") {
          self._pageCount = m.pageCount;
          self._pw = m.pw;
          self._ph = m.ph;
          resolve(self);
        } else if (m.type === "rendered") {
          const cb = self._pending.get(m.reqId);
          if (cb) {
            self._pending.delete(m.reqId);
            cb.resolve(m);
          }
        } else if (m.type === "error") {
          if (m.reqId != null && self._pending.has(m.reqId)) {
            self._pending.get(m.reqId).reject(new Error(m.message));
            self._pending.delete(m.reqId);
          } else {
            worker.terminate();
            reject(new Error(m.message));
          }
        }
      };
      worker.onerror = (ev) => reject(new Error("render worker failed: " + ev.message));
      worker.postMessage({ type: "open", kind, bytes, font, extra }, [bytes.buffer, font.buffer]);
    });
  }

  constructor(worker) {
    this.worker = worker;
    this._pending = new Map();
    this._req = 0;
  }

  pageCount() {
    return this._pageCount;
  }
  pageSize() {
    return [this._pw, this._ph];
  }

  /** @returns {Promise<{ bitmap: ImageBitmap, w: number, h: number, page: number }>} */
  renderPage(page, scale) {
    const reqId = ++this._req;
    return new Promise((resolve, reject) => {
      this._pending.set(reqId, { resolve, reject });
      this.worker.postMessage({ type: "render", reqId, page, scale });
    });
  }

  destroy() {
    this.worker.terminate();
    this._pending.clear();
  }
}
