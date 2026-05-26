// Cloudflare Workers import `.wasm` files as a compiled `WebAssembly.Module`
// (wrangler bundles any `*.wasm` import). The wasm-bindgen web-target glue is
// then initialized with that module via `initSync({ module })`.
//
// The wasm-bindgen build also emits a `broker_bg.wasm.d.ts` that types the raw
// wasm *exports* (for bundler-target consumption) and would shadow this
// default-import declaration — the build script (scripts/build-broker-wasm.sh)
// deletes it so this applies.
declare module "*.wasm" {
  const module: WebAssembly.Module;
  export default module;
}
