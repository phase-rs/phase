// Module-customization hooks that stand in for the generated Rust broker glue.
//
// `src/index.ts` -> `src/lobby-do.ts` imports `../broker-wasm-pkg/broker.js` and
// `broker_bg.wasm` and calls `initSync` at module scope. That package is a
// gitignored build artifact, absent from the plain checkout CI tests (`npm ci`,
// never a wasm build). The relay tests drive the Worker's REAL default export, so
// the glue is replaced with inert exports; nothing that reaches the broker is
// exercised, and a request that did reach the Durable Object is observed through
// the fake `LOBBY` binding instead.

let names = [];

export function initialize(data) {
  names = data.names;
}

const BROKER_JS = /broker-wasm-pkg\/broker\.js$/;
const BROKER_WASM = /broker-wasm-pkg\/broker_bg\.wasm$/;

export async function resolve(specifier, context, next) {
  if (BROKER_JS.test(specifier)) {
    return { url: "stub-broker:glue", shortCircuit: true };
  }
  if (BROKER_WASM.test(specifier)) {
    return { url: "stub-broker:wasm", shortCircuit: true };
  }
  return next(specifier, context);
}

export async function load(url, context, next) {
  if (url === "stub-broker:glue") {
    // Inert, except the one export that lobby-do.ts iterates at module scope.
    const exports = names
      .map((name) =>
        name === "directory_rtt_bucket_edges_ms"
          ? `export const ${name} = () => [];`
          : `export const ${name} = () => {};`,
      )
      .join("\n");
    return { format: "module", source: `${exports}\n`, shortCircuit: true };
  }
  if (url === "stub-broker:wasm") {
    return { format: "module", source: "export default {};\n", shortCircuit: true };
  }
  return next(url, context);
}
