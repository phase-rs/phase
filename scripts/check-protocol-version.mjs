import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");

function extractVersion(source, pattern, label) {
  const match = source.match(pattern);
  if (!match) {
    throw new Error(`Could not find PROTOCOL_VERSION in ${label}`);
  }
  return Number(match[1]);
}

const rustSource = readFileSync(
  resolve(root, "crates/lobby-broker/src/protocol.rs"),
  "utf8",
);
const clientSource = readFileSync(
  resolve(root, "client/src/adapter/ws-adapter.ts"),
  "utf8",
);

const rustVersion = extractVersion(
  rustSource,
  /pub\s+const\s+PROTOCOL_VERSION\s*:\s*u32\s*=\s*(\d+)\s*;/,
  "crates/lobby-broker/src/protocol.rs",
);
const clientVersion = extractVersion(
  clientSource,
  /export\s+const\s+PROTOCOL_VERSION\s*=\s*(\d+)\s*;/,
  "client/src/adapter/ws-adapter.ts",
);

if (rustVersion !== clientVersion) {
  console.error(
    `Protocol version mismatch: Rust=${rustVersion}, client=${clientVersion}`,
  );
  process.exit(1);
}
