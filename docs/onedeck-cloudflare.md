# OneDeck Cloudflare free-tier profile

This profile deploys a playable web shell to Cloudflare Pages while keeping the
large, immutable runtime inputs in an operator-owned R2 bucket. It is separate
from the phase.rs production lobby and TURN credentials:

```text
Pages (shell, JS/CSS, small images)
        │
        ├── OneDeck R2 (gzip card data + gzip engine/draft WASM)
        └── OneDeck Worker (lobby/signaling + short-lived TURN mint)
```

The Worker uses the existing `lobby-worker/src` implementation, but
`lobby-worker/wrangler.onedeck.toml` gives it a different Worker name and a new
SQLite-backed Durable Object namespace. No official lobby rooms, directory, or
TURN token is shared.

## Operator setup

1. Create a private R2 bucket and a public custom domain (or `r2.dev` URL) for
   it. The URL passed as `r2_public_url` must be the bucket prefix used only by
   this profile.
2. Configure R2 CORS for `GET`/`HEAD` from the exact Pages origin. The objects
   are public immutable game inputs; no player deck, save, or match state is
   uploaded.
3. Create a Cloudflare Realtime TURN key dedicated to OneDeck. Add these GitHub
   Actions secrets: `CLOUDFLARE_API_TOKEN`, `CLOUDFLARE_ACCOUNT_ID`,
   `ONEDECK_TURN_KEY_ID`, and `ONEDECK_TURN_API_TOKEN`.
4. Run **Deploy OneDeck Cloudflare profile** manually from the protected
   `main` branch. The workflow fails closed on other refs because it handles
   Cloudflare credentials and executes repository-controlled build code. Supply
   the Pages project/origin, Worker HTTPS URL, R2 public URL, and R2 bucket. The
   workflow deploys the Worker first, then builds and deploys Pages.

The Worker CORS allowlist is injected from `pages_origin`; the checked-in
`https://onedeck-play.pages.dev` value is only a safe default for a newly created
project. The workflow accepts only an HTTPS origin (scheme, host, and optional
port), normalizes it before both deployments, and rejects paths, queries,
fragments, userinfo, whitespace, and a trailing slash. Do not replace it with
`*` in a production deployment.

This Stage 1 profile is intended for a personal or trusted-user deployment.
`/turn-credentials` uses the exact Pages-origin CORS allowlist and a per-IP
Cloudflare rate limit, but CORS and IP throttling are not authentication. This
profile therefore remains intended for a personal or trusted-user deployment;
add an authenticated session boundary before opening the Worker to an untrusted
public audience.

## Build boundary

`scripts/build-onedeck-cloudflare.sh` performs the following in order:

- generates card data and the release WASM when requested by CI;
- computes SHA-256 names from the uncompressed card, engine-WASM, and draft-WASM bytes;
- creates deterministic `gzip -9 -n` objects, stages them for the isolated R2
  upload step, and uploads them with
  `Content-Encoding: gzip`, the correct MIME type, and immutable cache headers;
- builds Vite with the dedicated Worker/R2 URLs and empty Supabase/telemetry
  settings;
- removes every entry from `data-files.json`, the content-addressed card JSON,
  and both WASM binaries from `client/dist`;
- copies the Cloudflare `_headers` file and the SPA fallback;
- fails if any Pages file exceeds 25 MiB, if generated runtime data/WASM or an
  environment file remains, or if credential markers appear in the artifact;
- optionally checks remote gzip headers and card/WASM round trips with
  `curl --compressed`.

The browser automatically decompresses the R2 response before JSON parsing or
WASM instantiation. The script therefore hashes the raw bytes, not the gzip
container, so a deployment cannot pair a WASM build with a different card
schema.

## Play flow and authority boundary

`/setup` now offers both **AIとプレイ** and **対人プレイ**. The AI button keeps
the existing solo path. The multiplayer button enters the existing
`/multiplayer?view=host-setup` screen, whose default Commander table has two
seats and still allows the supported 2–6 player P2P range.

For this free-tier profile, solo runs the Phase WASM engine in the browser and
multiplayer uses the existing host-authoritative P2P adapter. The dedicated
Worker brokers room/signaling and mints TURN credentials; it does not run the
Phase game engine or persist authoritative match state. A server-authoritative
Durable Object is deliberately a separate feasibility project because loading
the full card corpus and native server dependencies into a 128 MiB Worker is a
different architecture.

## Acceptance evidence

After a preview deployment, verify:

```bash
curl -I "$CARD_DATA_URL"    # Content-Encoding: gzip
curl -I "$ENGINE_WASM_URL"  # Content-Encoding: gzip
curl -I "$DRAFT_WASM_URL"   # Content-Encoding: gzip
curl --compressed -fsS "$CARD_DATA_URL" | jq empty
find client/dist -type f -size +25M -print   # no output
```

From the deployed Pages origin, confirm that the R2 bucket returns the expected
`Access-Control-Allow-Origin` value for both `GET` and `HEAD`; a local `curl`
alone does not prove browser CORS behavior.

In two browser profiles, open `/setup`, start a solo match through the first
priority action, then host and join a two-player room through the same entry
point. Browser network logs must show the OneDeck Worker/R2 hosts and no
`lobby.phase-rs.dev` or `phase-rs.dev/turn-credentials` request.
