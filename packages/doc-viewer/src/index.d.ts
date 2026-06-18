// Public type surface for doc-viewer.

export interface InitOptions {
  /** Override the location of the `.wasm` binary. */
  wasmUrl?: string | URL;
}

export interface InitResult {
  /** Semantic version of the WASM core. */
  version: string;
}

/** Load & instantiate the WASM core (idempotent). */
export function init(options?: InitOptions): Promise<InitResult>;

export interface RenderDemoOptions {
  /** URL of a TTF/OTF font to fetch and shape with. */
  fontUrl: string;
  /** Text to render (Latin and/or 繁體中文). */
  text: string;
  /** Em size in px. */
  size?: number;
  /** Left pen origin in px. */
  x?: number;
  /** Baseline y in px (defaults to ~62% of canvas height). */
  baseline?: number;
}

/** M1 geba demo: shape `text` and paint it onto `canvas`. */
export function renderToCanvas(canvas: HTMLCanvasElement, options: RenderDemoOptions): Promise<void>;

export type DocFormat = "pdf" | "ooxml" | "ole" | "unknown";

/** Detect a document's format from its magic bytes. */
export function sniffFormat(bytes: Uint8Array): DocFormat;

export interface MountOptions {
  /** Initial zoom (1 = 100%). */
  zoom?: number;
  /** Scroll container for virtualization; defaults to the viewport. */
  scrollRoot?: Element | null;
  /** Called when zoom changes. */
  onZoom?: (zoom: number) => void;
  /** URL of `pdfium.wasm`; defaults to @embedpdf/pdfium's CDN copy. */
  pdfiumWasmUrl?: string;
  /**
   * URL of a CJK fallback font (e.g. Noto Sans TC) installed into PDFium via
   * FPDF_SetSystemFontInfo. Without it, PDFs that reference a non-embedded CJK
   * font render those glyphs blank. Applied once at engine init (singleton).
   */
  cjkFallbackFontUrl?: string;
  /**
   * Explicit URL of the `@embedpdf/pdfium` ESM module. Needed only without a
   * bundler (e.g. the Worker path, which can't use the document's import map).
   */
  embedpdfUrl?: string;
  /** Password for an encrypted PDF. */
  password?: string;
  /**
   * Render in a Web Worker (default true when available; falls back to the main
   * thread on failure). Pass `false` to force main-thread rendering, `true` to
   * require the worker and surface its errors.
   */
  useWorker?: boolean;
  onProgress?: (rendered: number, total: number) => void;
}

export interface Viewer {
  pageCount: number;
  /** Current zoom factor (1 = 100%). */
  readonly zoom: number;
  /** Set zoom (clamped). Re-renders only the visible pages. */
  setZoom(zoom: number): void;
  zoomIn(): void;
  zoomOut(): void;
  /** Fit the widest page to the container width. */
  fitWidth(): void;
  destroy(): void;
}

/**
 * Render a document into `target`. PDF is wired (PDFium); Office (XLSX/DOCX/PPTX)
 * and legacy formats are detected but throw until their frontends land (M3+).
 */
export function mount(
  target: HTMLElement,
  source: Blob | ArrayBuffer | Uint8Array | string | URL,
  options?: MountOptions,
): Promise<Viewer>;

/** Semantic version of the WASM core. */
export function coreVersion(): string;
