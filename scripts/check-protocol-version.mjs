import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const EXPECTED_PROTOCOL_VERSION = 55;
// The LOBBY message-set version. Deliberately separate from the full-game
// number above and deliberately NOT derived from it: a GameState-only bump must
// not move the lobby's compatibility window. See the assertions at the bottom.
const EXPECTED_LOBBY_PROTOCOL_VERSION = 5;
// The capability FLOOR for correlated tournament settlement — a different kind
// of number from the three above, and the reason it is pinned separately. The
// others track a surface's current version; this one is frozen at the version
// that INTRODUCED the ack and must never be bumped alongside
// EXPECTED_LOBBY_PROTOCOL_VERSION. Raising it would refuse every newer broker
// that answers the ack perfectly well, silently disabling all four organizer
// actions.
const EXPECTED_MIN_LOBBY_PROTOCOL_FOR_TOURNAMENT_ACK = 5;
// The P2P wire version. A THIRD independent surface: host/guest first-contact
// frames carry it, and the same GameState shape change that moves
// EXPECTED_PROTOCOL_VERSION must move this one too. It was previously ungated
// here, so a full-game bump could ship with an unbumped P2P version and CI
// stayed green — a v(n-1) host and a v(n) guest would then complete a
// handshake and only fail when the incompatible payload arrived.
const EXPECTED_WIRE_PROTOCOL_VERSION = 40;

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
const clientTournamentAckFloor = extractVersion(
  clientSource,
  /export\s+const\s+MIN_LOBBY_PROTOCOL_FOR_TOURNAMENT_ACK\s*=\s*(\d+)\s*;/,
  "client/src/adapter/ws-adapter.ts",
);

// The structural invariant, and the reason this block exists. Each of the five
// regexes above requires a bare integer literal on the right-hand side, so a
// future edit to `LOBBY_PROTOCOL_VERSION = PROTOCOL_VERSION - 1` (or any other
// expression) fails to match and trips "Could not find protocol version"
// rather than silently re-coupling the two surfaces.
//
// That device is doing MORE work for the ack floor than for the other four. The
// plausible "improvement" to a frozen floor is to re-derive it from the current
// version — `= LOBBY_PROTOCOL_VERSION` — which reads like removing a magic
// number and is in fact a latent bug: at the next lobby bump every v5 broker,
// which does mint the ack, would be refused as unsupported. The bare-integer
// regex is what turns that edit into a failed check here instead.

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

if (
  clientTournamentAckFloor !== EXPECTED_MIN_LOBBY_PROTOCOL_FOR_TOURNAMENT_ACK
) {
  console.error(
    `MIN_LOBBY_PROTOCOL_FOR_TOURNAMENT_ACK must remain ${EXPECTED_MIN_LOBBY_PROTOCOL_FOR_TOURNAMENT_ACK}: got ${clientTournamentAckFloor}. ` +
      `It is FROZEN at the lobby version that introduced TournamentActionAck — not a moving target. ` +
      `Do NOT bump it with LOBBY_PROTOCOL_VERSION: a newer broker still answers the ack, and raising this ` +
      `floor would refuse every one of them and silently disable all four organizer actions.`,
  );
  process.exit(1);
}

if (clientTournamentAckFloor > rustLobbyVersion) {
  console.error(
    `The tournament-ack floor ${clientTournamentAckFloor} exceeds the lobby version ${rustLobbyVersion}: ` +
      `no broker could ever answer a gated tournament action.`,
  );
  process.exit(1);
}

if (rustLobbyFloor > rustLobbyVersion) {
  console.error(
    `Lobby floor ${rustLobbyFloor} exceeds the lobby version ${rustLobbyVersion}: no client could connect.`,
  );
  process.exit(1);
}
