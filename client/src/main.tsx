import { createRoot } from "react-dom/client";
import "./index.css";
import "./i18n"; // initialize i18next before any component renders
import { App } from "./App";
import { registerServiceWorker } from "./pwa/registerServiceWorker";
import { registerTauriUpdater } from "./pwa/tauriUpdater";
import { installChunkReloadHandler } from "./pwa/chunkReloadHandler";
import { installTauriExternalLinkHandler } from "./services/externalLinks";

// StrictMode is scoped inside App.tsx instead of wrapping the root. P2P game
// sessions own PeerJS resources whose cleanup is intentionally destructive, so
// those routes opt out of dev-only StrictMode double-mounting.
createRoot(document.getElementById("root")!).render(<App />);

registerServiceWorker();
registerTauriUpdater();
installChunkReloadHandler();
installTauriExternalLinkHandler();
