# phase-lobby (Cloudflare Worker + Durable Object)

The official phase.rs lobby broker, running as a single global Cloudflare
Durable Object. **This is currently a stub** whose only purpose is to validate
the Cloudflare plumbing end-to-end before the real broker lands. See
`.planning/lobby-failover-federation-plan.md`.

- **Single global lobby:** every connection routes to one DO instance
  (`idFromName("global")`) — no regional fragmentation.
- **P2P-broker-only:** the DO never runs game logic; it brokers matchmaking +
  WebRTC signaling handoff. The engine still owns all MTG rules.
- **Stub scope:** handshake, lobby list, player count, P2P host/join happy-path.
  **No** rate limiting, entry cap, seat reservations, or expiry reaper yet —
  those arrive when the compiled Rust `lobby-broker` crate replaces the DO body.

## Prerequisites

- Node 18+ and a Cloudflare account.
- `npm install` here (pulls `wrangler`, `@cloudflare/workers-types`, `typescript`).

## Deploy (you run these — they need interactive CF auth)

```bash
cd lobby-worker
npm install
npx wrangler login          # opens a browser to authorize your CF account
npm run typecheck           # optional: tsc --noEmit
npm run deploy              # wrangler deploy → prints your workers.dev URL
```

`deploy` prints a URL like `https://phase-lobby.<your-subdomain>.workers.dev`.
The WebSocket endpoint is that host with `/ws`:

```
wss://phase-lobby.<your-subdomain>.workers.dev/ws
```

## Test against the live app WITHOUT touching the production server

The existing `phase-server` stays the default — this is exercised only via the
custom-server field, so there is zero risk to live multiplayer:

1. Open the app → **Multiplayer**.
2. Click the server chip → **Server** dialog → **Self-hosted** field.
3. Paste `wss://phase-lobby.<your-subdomain>.workers.dev/ws` → **Test** (should
   say "Connected") → **Use**.
4. You should see the lobby load and an online count appear.
5. Host a P2P game in one browser/tab; from a second browser/profile (also
   pointed at the same URL), the room should appear and you should be able to
   join and connect peer-to-peer.

### Smoke check (no app needed)

```bash
curl https://phase-lobby.<your-subdomain>.workers.dev/
# → {"mode":"LobbyOnly","protocol_version":6,"server_version":"lobby-stub"}
```

This `/version` response is also what a future release-time protocol-version
gate would assert against (plan §4c).

### Live logs

```bash
npm run tail        # wrangler tail — streams DO logs
```

## Cutover (later, NOT now)

When the real Rust broker is in and validated, switch the default by changing
`DEFAULT_SERVER` / `SERVER_PRESETS[0].url` in
`client/src/services/serverDetection.ts` to the DO URL. Until then, keep the
existing `phase-server` as the default.

## ⚠️ The TS protocol mirror is throwaway

`src/protocol.ts` + `src/lobby-do.ts` hand-mirror the Rust wire protocol. That
duplication is the exact drift hazard the WASM-shared-crate plan eliminates:
`PROTOCOL_VERSION` here (currently **6**) must track
`crates/server-core/src/protocol.rs`. When the `lobby-broker` crate is compiled
to WASM and loaded into the DO, delete this mirror.
