import { createRoot } from "react-dom/client";
// Self-hosted variable webfonts (served from node_modules by Vite — no Google
// CDN). Newsreader = serif display; JetBrains Mono = codes / tabular numbers.
import "@fontsource-variable/newsreader";
import "@fontsource-variable/jetbrains-mono";
// Mana/loyalty/counter iconography (Andrew Gioia's mana-font). The vendored
// mana.css ships legacy eot/woff/ttf/svg faces for "Mana" plus an unused
// "MPlantin" serif; the `trimManaFont` Vite plugin rewrites this import at build
// time to a single woff2-only "Mana" @font-face (see vite.config.ts) so only one
// 187 KB font is bundled here and in the Tauri app.
import "mana-font/css/mana.css";
import "./index.css";
import "./i18n"; // initialize i18next before any component renders
import { App } from "./App";
import { registerServiceWorker } from "./pwa/registerServiceWorker";
import { registerTauriUpdater } from "./pwa/tauriUpdater";
import { installChunkReloadHandler } from "./pwa/chunkReloadHandler";
import { installTauriExternalLinkHandler } from "./services/externalLinks";
import { installTelemetry } from "./services/telemetryEvents";

// StrictMode is scoped inside App.tsx instead of wrapping the root. P2P game
// sessions own PeerJS resources whose cleanup is intentionally destructive, so
// those routes opt out of dev-only StrictMode double-mounting.
createRoot(document.getElementById("root")!).render(<App />);

registerServiceWorker();
registerTauriUpdater();
installChunkReloadHandler();
installTauriExternalLinkHandler();
installTelemetry();
