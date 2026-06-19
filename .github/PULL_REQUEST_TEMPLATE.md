<!-- Thanks for contributing to doc-viewer! Keep PRs focused and explain the why. -->

## What & why

<!-- What does this change, and what problem does it solve? Link any issue: Closes #123 -->

## Type

- [ ] Bug fix
- [ ] New format / feature
- [ ] Rendering fidelity improvement
- [ ] Performance
- [ ] Docs / tooling

## How it was tested

<!-- Which files/formats, which browsers, native demo, etc. -->

## Screenshots (required for any rendering change)

<!-- Before / after, compared against a reference. Fidelity is the point — show it. -->

## Checklist

- [ ] `cargo build --workspace --exclude dv-wasm` and `cargo test ...` pass
- [ ] `cargo build --release --target wasm32-unknown-unknown -p dv-wasm` compiles
- [ ] No real/private documents committed (synthetic fixtures only)
- [ ] New/updated Rust deps are permissively licensed; `THIRD-PARTY-NOTICES.txt` regenerated if deps changed
- [ ] Followed the shared `dv-ir` rendering pattern (no parallel pipeline)
