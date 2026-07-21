import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const EXPECTED_PROTOCOL_VERSION = 20;

function extractVersion(source, pattern, label) {
  const match = source.match(pattern);
  if (!match) {
    throw new Error(`Could not find protocol version in ${label}`);
  }
  return Number(match[1]);
}

function requirePattern(source, pattern, label) {
  if (!pattern.test(source)) {
    throw new Error(`Protocol floor does not match expectation in ${label}`);
  }
}

const rustSource = readFileSync(
  resolve(root, "crates/lobby-broker/src/protocol.rs"),
  "utf8",
);
const serverCoreSource = readFileSync(
  resolve(root, "crates/server-core/src/protocol.rs"),
  "utf8",
);
const clientSource = readFileSync(
  resolve(root, "client/src/adapter/ws-adapter.ts"),
  "utf8",
);
const workerHelloGateSource = readFileSync(
  resolve(root, "lobby-worker/src/hello-gate.ts"),
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

requirePattern(
  rustSource,
  /pub\s+const\s+MIN_SUPPORTED_PROTOCOL\s*:\s*u32\s*=\s*PROTOCOL_VERSION\.saturating_sub\(1\)\s*;/,
  "crates/lobby-broker/src/protocol.rs",
);
requirePattern(
  serverCoreSource,
  /pub\s+const\s+MIN_SUPPORTED_PROTOCOL\s*:\s*u32\s*=\s*PROTOCOL_VERSION\s*;/,
  "crates/server-core/src/protocol.rs",
);
requirePattern(
  clientSource,
  /export\s+const\s+MIN_SUPPORTED_SERVER_PROTOCOL\s*=\s*PROTOCOL_VERSION\s*;/,
  "client/src/adapter/ws-adapter.ts",
);
requirePattern(
  workerHelloGateSource,
  /const\s+minSupportedProtocol\s*=\s*Math\.max\(0,\s*serverProtocolVersion\s*-\s*1\)\s*;/,
  "lobby-worker/src/hello-gate.ts",
);

if (rustVersion !== clientVersion) {
  console.error(
    `Protocol version mismatch: Rust=${rustVersion}, client=${clientVersion}`,
  );
  process.exit(1);
}

if (
  rustVersion !== EXPECTED_PROTOCOL_VERSION ||
  clientVersion !== EXPECTED_PROTOCOL_VERSION
) {
  console.error(
    `Protocol version must remain ${EXPECTED_PROTOCOL_VERSION}: Rust=${rustVersion}, client=${clientVersion}`,
  );
  process.exit(1);
}
