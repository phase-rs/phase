import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const EXPECTED_PROTOCOL_VERSION = 57;
// The LOBBY message-set version. Deliberately separate from the full-game
// number above and deliberately NOT derived from it: a GameState-only bump must
// not move the lobby's compatibility window. See the assertions at the bottom.
const EXPECTED_LOBBY_PROTOCOL_VERSION = 4;
// The P2P wire version. A THIRD independent surface: host/guest first-contact
// frames carry it, and the same GameState shape change that moves
// EXPECTED_PROTOCOL_VERSION must move this one too. It was previously ungated
// here, so a full-game bump could ship with an unbumped P2P version and CI
// stayed green — a v(n-1) host and a v(n) guest would then complete a
// handshake and only fail when the incompatible payload arrived.
const EXPECTED_WIRE_PROTOCOL_VERSION = 41;

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
const p2pProtocolSource = readFileSync(
  resolve(root, "client/src/network/protocol.ts"),
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
  /const\s+legacyMin\s*=\s*Math\.max\(0,\s*policy\.serverProtocolVersion\s*-\s*1\)\s*;/,
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

// ── P2P wire protocol: the third surface ───────────────────────────────────
//
// Pinned here for the same reason the full-game number is: a `GameState` shape
// change crosses BOTH the WebSocket full-game wire and the P2P host/guest wire,
// and bumping only one leaves the other pairing to fail at payload-decode time
// instead of at the handshake. Gating both in one place makes "I bumped the
// protocol" mean all of it.

const wireProtocolVersion = extractVersion(
  p2pProtocolSource,
  /export\s+const\s+WIRE_PROTOCOL_VERSION\s*=\s*(\d+)\s*as\s+const\s*;/,
  "client/src/network/protocol.ts",
);

if (wireProtocolVersion !== EXPECTED_WIRE_PROTOCOL_VERSION) {
  console.error(
    `P2P wire protocol version must remain ${EXPECTED_WIRE_PROTOCOL_VERSION}: got ${wireProtocolVersion}. ` +
      `A GameState shape change must bump this alongside PROTOCOL_VERSION, not instead of it.`,
  );
  process.exit(1);
}

// ── Lobby protocol: a SEPARATE surface with its own version ────────────────
//
// `PROTOCOL_VERSION` versions the full-game GameState/GameAction wire surface.
// The lobby broker parses none of that, yet its accept-window used to be
// derived from that same number — so a GameState-only bump slid the lobby
// window and stranded every already-deployed client. These assertions keep the
// two surfaces genuinely independent.

const rustLobbyVersion = extractVersion(
  rustSource,
  /pub\s+const\s+LOBBY_PROTOCOL_VERSION\s*:\s*u32\s*=\s*(\d+)\s*;/,
  "crates/lobby-broker/src/protocol.rs",
);
const clientLobbyVersion = extractVersion(
  clientSource,
  /export\s+const\s+LOBBY_PROTOCOL_VERSION\s*=\s*(\d+)\s*;/,
  "client/src/adapter/ws-adapter.ts",
);
const rustLobbyFloor = extractVersion(
  rustSource,
  /pub\s+const\s+MIN_SUPPORTED_LOBBY_PROTOCOL\s*:\s*u32\s*=\s*(\d+)\s*;/,
  "crates/lobby-broker/src/protocol.rs",
);
const clientLobbyFloor = extractVersion(
  clientSource,
  /export\s+const\s+MIN_SUPPORTED_SERVER_LOBBY_PROTOCOL\s*=\s*(\d+)\s*;/,
  "client/src/adapter/ws-adapter.ts",
);

// The structural invariant, and the reason this block exists. Each of the four
// regexes above requires a bare integer literal on the right-hand side, so a
// future edit to `LOBBY_PROTOCOL_VERSION = PROTOCOL_VERSION - 1` (or any other
// expression) fails to match and trips "Could not find protocol version"
// rather than silently re-coupling the two surfaces.

if (rustLobbyVersion !== clientLobbyVersion) {
  console.error(
    `Lobby protocol version mismatch: Rust=${rustLobbyVersion}, client=${clientLobbyVersion}`,
  );
  process.exit(1);
}

if (rustLobbyFloor !== clientLobbyFloor) {
  console.error(
    `Lobby protocol floor mismatch: Rust=${rustLobbyFloor}, client=${clientLobbyFloor}`,
  );
  process.exit(1);
}

if (rustLobbyVersion !== EXPECTED_LOBBY_PROTOCOL_VERSION) {
  console.error(
    `Lobby protocol version must remain ${EXPECTED_LOBBY_PROTOCOL_VERSION}: got ${rustLobbyVersion}. ` +
      `Bump it ONLY for a LobbyClientMessage/LobbyServerMessage shape change — never for a full-game bump.`,
  );
  process.exit(1);
}

if (rustLobbyFloor > rustLobbyVersion) {
  console.error(
    `Lobby floor ${rustLobbyFloor} exceeds the lobby version ${rustLobbyVersion}: no client could connect.`,
  );
  process.exit(1);
}

// ── Directory announcement shape: a FOURTH constant surface ────────────────
//
// `DIRECTORY_VERSION` versions the ANNOUNCEMENT shape — the `POST /announce`
// body Rust sends and the `GET /servers` envelope the client reads. It is
// unrelated to all three wire protocols above: none of the lobby, full-game or
// P2P message sets appears in a directory row. It moves only when
// `RawAnnouncement` / `DirectoryRow` change shape, and both sides must move
// together or a client silently ignores every listing.

const EXPECTED_DIRECTORY_VERSION = 1;

const directorySource = readFileSync(
  resolve(root, "crates/lobby-broker/src/directory.rs"),
  "utf8",
);
const clientDirectorySource = readFileSync(
  resolve(root, "client/src/services/serverDirectory.ts"),
  "utf8",
);

// Both regexes require a bare integer literal on the right-hand side, so a
// future `DIRECTORY_VERSION = SOMETHING + 1` trips "Could not find protocol
// version" rather than silently un-pinning the surface.
const rustDirectoryVersion = extractVersion(
  directorySource,
  /pub\s+const\s+DIRECTORY_VERSION\s*:\s*u32\s*=\s*(\d+)\s*;/,
  "crates/lobby-broker/src/directory.rs",
);
const clientDirectoryVersion = extractVersion(
  clientDirectorySource,
  /export\s+const\s+DIRECTORY_VERSION\s*=\s*(\d+)\s*;/,
  "client/src/services/serverDirectory.ts",
);

if (rustDirectoryVersion !== clientDirectoryVersion) {
  console.error(
    `Directory version mismatch: Rust=${rustDirectoryVersion}, client=${clientDirectoryVersion}`,
  );
  process.exit(1);
}

// Not redundant with the mismatch check above. A COORDINATED bump of both
// sides — exactly what an auto-merge of two branches can produce silently —
// passes consistency and fails here.
if (rustDirectoryVersion !== EXPECTED_DIRECTORY_VERSION) {
  console.error(
    `Directory version must remain ${EXPECTED_DIRECTORY_VERSION}: got ${rustDirectoryVersion}. ` +
      `Bump it ONLY for a RawAnnouncement/DirectoryRow shape change.`,
  );
  process.exit(1);
}
