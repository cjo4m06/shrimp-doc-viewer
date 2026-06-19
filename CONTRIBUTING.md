# Contributing to ShrimpDocViewer

Thanks for your interest! ShrimpDocViewer is a **browser, viewer-only** multi-format
document viewer (Rust → WebAssembly core, pure-JS API). Contributions are welcome —
this guide covers how to build, test, and propose changes.

## Scope & principles

Keep changes aligned with the project's design constraints:

1. **Viewer-only** — render documents faithfully; no editing/authoring.
2. **Runs in the browser, client-side** — no server round-trips.
3. **Lean** — dependencies must be healthy, permissively licensed, and pull their
   weight; prefer self-implementing over a heavy dependency. Watch payload size.
4. **Fast & responsive** — rasterization stays off the main thread (render Worker);
   viewers virtualize pages/cells.

New formats lower into the shared `dv-ir` display list and are painted by the same
text/raster stack — please follow that pattern rather than bolting on a parallel one.

## Project layout

```
crates/            Rust workspace (compiles to the wasm core)
  dv-ir            display-list IR
  dv-text          shaping (rustybuzz) + outlines (skrifa) + multi-font selection
  dv-render        tiny-skia CPU rasterizer
  dv-flow          shared rich-text flow layout (md/txt/rtf/odt/odp)
  dv-xlsx          grid model (xlsx/csv/ods)
  dv-docx/-pptx    OOXML renderers
  dv-md/-rtf/-odf  parsers
  dv-wasm          wasm-bindgen entry point (cdylib)
packages/doc-viewer  the published npm package (JS API + wasm/ + fonts/)
examples/browser     runnable demos (open the *.html over a static server)
```

## Dev setup

Prerequisites: Rust (stable; see `rust-toolchain.toml`), the
`wasm32-unknown-unknown` target, `wasm-bindgen-cli` matching the pinned
`wasm-bindgen` (currently `0.2.125`), and optionally `wasm-opt` (binaryen).

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.125 --locked

./scripts/build-wasm.sh           # build wasm core + JS glue -> packages/doc-viewer/wasm
python3 -m http.server 8123       # then open http://localhost:8123/examples/browser/
```

Native smoke tests (render a page to PNG without a browser):

```bash
cargo run -p dv-docx --example docx_demo -- <file.docx> <font.ttf>
cargo run -p dv-pptx --example pptx_demo -- <file.pptx> <font.ttf>
```

## Before you open a PR

- `cargo build --workspace --exclude dv-wasm` and `cargo test --workspace --exclude dv-wasm` pass.
- `cargo build --release --target wasm32-unknown-unknown -p dv-wasm` compiles.
- For **rendering changes, include before/after screenshots** — fidelity is the whole
  point here, so visual evidence is expected. Compare against a reference, don't
  eyeball it.
- `cargo fmt --all` and `cargo clippy --workspace --exclude dv-wasm --all-targets -- -D warnings`
  are clean (CI enforces both).
- Keep PRs focused; explain the *why*, not just the *what*.

## Do not commit

- **Real/private documents.** Test with synthetic fixtures (see
  `examples/assets/make_*.py`). Personal files are gitignored as `realtest*` — keep
  it that way.
- Large binary assets beyond the one bundled default font.

## CI

Every PR runs the workflow in `.github/workflows/ci.yml`: native build + tests, a
wasm-target compile check, and fmt + clippy — all required. They must be green
before merge.

## Licensing of contributions

By contributing, you agree your contributions are licensed under the project's
**MIT OR Apache-2.0** dual license (inbound = outbound). Don't add dependencies under
copyleft licenses (GPL/LGPL/AGPL/MPL/…) or anything that isn't permissive — it would
break the project's "ship a compiled, permissively-licensed core" guarantee. If you
add or update a Rust dependency, regenerate `THIRD-PARTY-NOTICES.txt`.

Questions? Open an issue. Thanks for helping!
