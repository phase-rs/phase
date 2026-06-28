# phase-lobby (Cloudflare Worker + Durable Object)

The official phase.rs lobby broker, running as a single global Cloudflare
Durable Object. The DO body (`src/lobby-do.ts`) is a thin imperative shell
around the compiled Rust `lobby-broker` core (`broker-wasm` →
`src/broker-wasm-pkg`) — the SAME code `phase-server` runs natively, so the two
deployments behave identically by construction. See
`.planning/lobby-failover-federation-plan.md`.

- **Single global lobby:** every connection routes to one DO instance
  (`idFromName("global")`) — no regional fragmentation.
- **P2P-broker-only:** the DO never runs game logic; it brokers matchmaking +
  WebRTC signaling handoff. The engine still owns all MTG rules.
- **Rust core, thin TS shell:** protocol parsing, dispatch, seat reservations,
  capacity caps, build-commit gating, and the staleness reaper all live in the
  Rust `lobby-broker` crate (compiled to WASM). The TS shell owns only the
  WebSocket lifecycle, DO storage snapshots, and edge name/lobby moderation.

## Prerequisites

- Node 18+ and a Cloudflare account.
- `npm install` here (pulls `wrangler`, `@cloudflare/workers-types`, `typescript`, `tsx`).
- `npm test` runs the `.mjs` suite against `src/*.ts` via `tsx` (Node’s test runner cannot load `.ts` directly).

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

## Enable TURN relay (ephemeral credentials)

The Worker mints short-lived Cloudflare Realtime TURN credentials at
`GET /turn-credentials`, so the client never ships static TURN creds. Until this
is configured, the endpoint returns 503 and the client falls back to STUN-only
(direct connections work; symmetric-NAT/CGNAT peers can't relay).

1. In the Cloudflare dashboard → **Realtime → TURN**, create a TURN key. Note
   the **Key ID** and the **API token**.
2. Put the Key ID in `wrangler.toml` under `[vars]` → `TURN_KEY_ID`.
3. Set the API token as a secret (never commit it):
   ```bash
   npx wrangler secret put TURN_KEY_API_TOKEN
   ```
4. Redeploy: `npm run deploy`.
5. Verify:
   ```bash
   curl https://lobby.phase-rs.dev/turn-credentials
   # → {"iceServers":[{"urls":[...]},{"urls":[...],"username":"...","credential":"..."}]}
   ```

The client (`client/src/network/connection.ts`) fetches this from
`TURN_CREDENTIALS_URL` and caches it for 6h. **Do CF TURN setup before deploying
the client change**, or relay degrades to STUN-only until the endpoint is live.
Free tier: 1,000 GB/mo relayed (≫ the prior Metered 20 GB).

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
# → {"mode":"LobbyOnly","protocol_version":11,"server_version":"lobby-rs"}
```

This `/version` response is also what a release-time protocol-version gate
asserts against (plan §4c). `protocol_version` is exported from the Rust core
(`broker-wasm` → `protocol_version()`), so there is no TS constant to keep in
sync.

### Live logs

```bash
npm run tail        # wrangler tail — streams DO logs
```

## Cutover (when validated)

The Rust broker is deployed here now. Once it's validated in production, switch
the default by changing `DEFAULT_SERVER` / `SERVER_PRESETS[0].url` in
`client/src/services/serverDetection.ts` to the DO URL. Until then, keep the
existing `phase-server` as the default.
