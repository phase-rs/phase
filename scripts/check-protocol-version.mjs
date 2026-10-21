import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { checkProtocolHistory, checkProtocolVersions } from "./protocol-versions.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
if (args.length !== 0 && (args.length !== 2 || args[0] !== "--base" || args[1].trim().length === 0)) {
  throw new Error("Usage: node scripts/check-protocol-version.mjs [--base <ref>]");
}
if (args.length) checkProtocolHistory(root, args[1]);
const versions = checkProtocolVersions(root);
const read = (path) => readFileSync(resolve(root, path), "utf8");

function requirePattern(source, pattern, label) {
  if (!pattern.test(source)) throw new Error(`Required protocol ownership/policy not found in ${label}`);
}

function extractVersion(source, pattern, label) {
  const match = source.match(pattern);
  if (!match) throw new Error(`Could not find constant in ${label}`);
  return Number(match[1]);
}

const rustSource = read("crates/lobby-broker/src/protocol.rs");
const serverCoreSource = read("crates/server-core/src/protocol.rs");
const clientSource = read("client/src/adapter/ws-adapter.ts");
const draftProtocolSource = read("client/src/network/draftProtocol.ts");
const draftCoreTypesSource = read("crates/draft-core/src/types.rs");

requirePattern(read("crates/lobby-broker/src/lib.rs"),
  /mod\s+protocol_versions\s*\{\s*include!\(concat!\(env!\("OUT_DIR"\),\s*"\/protocol_versions\.rs"\)\);\s*\}/,
  "broker private generated module");
for (const name of ["PROTOCOL_VERSION", "LOBBY_PROTOCOL_VERSION", "MIN_SUPPORTED_LOBBY_PROTOCOL", "DIRECTORY_VERSION"]) {
  const source = name === "DIRECTORY_VERSION" ? read("crates/lobby-broker/src/directory.rs") : rustSource;
  requirePattern(source, new RegExp(`pub\\s+const\\s+${name}\\s*:\\s*u32\\s*=\\s*crate::protocol_versions::${name}\\s*;`), `broker ${name}`);
}
requirePattern(serverCoreSource, /pub\s+const\s+PROTOCOL_VERSION\s*:\s*u32\s*=\s*lobby_broker::PROTOCOL_VERSION\s*;/, "server-core full alias");
for (const name of ["LOBBY_PROTOCOL_VERSION", "MIN_SUPPORTED_LOBBY_PROTOCOL"]) {
  requirePattern(serverCoreSource, new RegExp(`pub\\s+const\\s+${name}\\s*:\\s*u32\\s*=\\s*lobby_broker::${name}\\s*;`), `server-core ${name}`);
}

const clientExports = [
  ["client/src/adapter/ws-adapter.ts", "../generated/protocol-versions", ["PROTOCOL_VERSION", "LOBBY_PROTOCOL_VERSION", "MIN_SUPPORTED_LOBBY_PROTOCOL as MIN_SUPPORTED_SERVER_LOBBY_PROTOCOL"]],
  ["client/src/network/protocol.ts", "../generated/protocol-versions", ["WIRE_PROTOCOL_VERSION"]],
  ["client/src/network/draftProtocol.ts", "../generated/protocol-versions", ["DRAFT_PROTOCOL_VERSION"]],
  ["client/src/services/serverDirectory.ts", "../generated/protocol-versions", ["DIRECTORY_VERSION"]],
  ["scripts/card-bot/servers.ts", "../../client/src/generated/protocol-versions.ts", ["DIRECTORY_VERSION"]],
];
for (const [path, module, bindings] of clientExports) {
  const source = read(path);
  const imports = [...source.matchAll(/import\s*\{([^}]+)\}\s*from\s*"([^"]+)"/g)]
    .filter((match) => match[2] === module)
    .flatMap((match) => match[1].split(",").map((binding) => binding.trim().replace(/\s+/g, " ")));
  for (const binding of bindings) {
    if (!imports.includes(binding)) throw new Error(`${path}: missing generated import ${binding}`);
    const name = binding.split(" as ").at(-1);
    requirePattern(source, new RegExp(`export\\s*\\{\\s*${name}\\s*\\}\\s*;`), `${path} export ${name}`);
  }
}

requirePattern(rustSource, /pub\s+const\s+MIN_SUPPORTED_PROTOCOL\s*:\s*u32\s*=\s*PROTOCOL_VERSION\.saturating_sub\(1\)\s*;/, "broker legacy floor");
requirePattern(serverCoreSource, /pub\s+const\s+MIN_SUPPORTED_PROTOCOL\s*:\s*u32\s*=\s*PROTOCOL_VERSION\s*;/, "server-core full floor");
requirePattern(serverCoreSource, /pub\s+const\s+LOBBY_MIN_SUPPORTED_PROTOCOL\s*:\s*u32\s*=\s*lobby_broker::MIN_SUPPORTED_PROTOCOL\s*;/, "server-core legacy floor");
requirePattern(clientSource, /export\s+const\s+MIN_SUPPORTED_SERVER_PROTOCOL\s*=\s*PROTOCOL_VERSION\s*;/, "client full floor");
requirePattern(read("lobby-worker/src/hello-gate.ts"), /const\s+legacyMin\s*=\s*Math\.max\(0,\s*policy\.serverProtocolVersion\s*-\s*1\)\s*;/, "Worker legacy floor");

// Capability floors are historical introduction versions, not current versions.
const frozenFloors = {
  MIN_LOBBY_PROTOCOL_FOR_TOURNAMENT_ACK: 5,
  MIN_LOBBY_PROTOCOL_FOR_DEFAULT_SCORING: 6,
  MIN_LOBBY_PROTOCOL_FOR_MATCH_TYPE: 8,
  MIN_LOBBY_PROTOCOL_FOR_RECOVERABLE_ROTATION: 9,
  MIN_LOBBY_PROTOCOL_FOR_FREEFORM_FORMATS: 11,
  MIN_LOBBY_PROTOCOL_FOR_DANDAN: 16,
};
for (const [name, expected] of Object.entries(frozenFloors)) {
  const actual = extractVersion(clientSource, new RegExp(`export\\s+const\\s+${name}\\s*=\\s*(\\d+)\\s*;`), name);
  if (actual !== expected || actual > versions.lobby) throw new Error(`${name}: invalid frozen capability floor`);
}
requirePattern(read("scripts/card-bot/servers.ts"), /export\s+const\s+REQUESTED_CODE_LOBBY_PROTOCOL\s*=\s*10\s*;/, "bot requested-code floor");

// Current versions and derived policies cannot become authored numeric mirrors.
const declarations = /(?:pub(?:\([^)]+\))?\s+|export\s+)?(?:const|static)\s+([A-Z][A-Z0-9_]*)\s*(?::\s*[^=;]+)?\s*=\s*([^;]+);/g;
const integer = /^\d[\d_]*(_?[iu]\d+)?(\s+(as|satisfies)\s+[^=;]+)?$/;
for (const path of ["crates/lobby-broker/src/protocol.rs", "crates/server-core/src/protocol.rs", ...clientExports.map(([path]) => path)]) {
  for (const [, name, rhs] of read(path).matchAll(declarations)) {
    if (/PROTOCOL|MIN_SUPPORTED/.test(name) && integer.test(rhs.trim())
      && !Object.hasOwn(frozenFloors, name) && name !== "REQUESTED_CODE_LOBBY_PROTOCOL") {
      throw new Error(`${path}: ${name} must derive from the registry or policy`);
    }
  }
}

const DRAFT_PAYLOAD_BOUNDS = [
  "MAX_CARDS_PER_PICK",
  "MAX_COMMANDER_DESIGNATIONS",
  "MAX_SHARED_STACK_PILES",
];

for (const name of DRAFT_PAYLOAD_BOUNDS) {
  // Both sides require a bare integer right-hand side, for the same reason the
  // version regexes do: re-deriving either mirror from the other would defeat
  // the comparison, and an expression trips "Could not find" instead.
  const rustBound = extractVersion(
    draftCoreTypesSource,
    new RegExp(`pub\\s+const\\s+${name}\\s*:\\s*usize\\s*=\\s*(\\d+)\\s*;`),
    `crates/draft-core/src/types.rs ${name}`,
  );
  const clientBound = extractVersion(
    draftProtocolSource,
    new RegExp(`const\\s+${name}\\s*=\\s*(\\d+)\\s*;`),
    `client/src/network/draftProtocol.ts ${name}`,
  );
  if (rustBound !== clientBound) {
    console.error(
      `Draft payload bound mismatch for ${name}: Rust=${rustBound}, client=${clientBound}. ` +
        "The engine constant is derived from the procedure table; the transport mirror must move with it, " +
        "or the client will refuse payloads the reducer accepts.",
    );
    process.exit(1);
  }
}
