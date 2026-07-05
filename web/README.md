# poker-trainer web examples

A static, framework-free catalog of the trainer's human-facing examples:
equity calculator, pot-odds drill, texture drill (all the Rust crate compiled
to wasm), and a GTO strategy grid that fetches the committed starter-8
solution snapshots. Deployed to GitHub Pages by `.github/workflows/pages.yml`;
there is no server.

## Build & run locally

```sh
cd web
wasm-pack build --release --target web   # writes pkg/ (gitignored)
mkdir -p solutions
cp ../data/solutions/*.json solutions/
(cd ../data/solutions && ls *.json | jq -R . | jq -s -c .) > solutions/index.json
python3 -m http.server 8000              # http://localhost:8000
```

ES modules + wasm won't load from `file://` — serve over HTTP. Needs the
`wasm32-unknown-unknown` target and `wasm-pack` (or `wasm-bindgen-cli`
matching Cargo.lock).

`cargo test` here runs the bindings natively (rlib) — no browser needed.
