import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./index.css";
import { App } from "./App";
import { registerServiceWorker } from "./pwa/registerServiceWorker";
import { registerTauriUpdater } from "./pwa/tauriUpdater";
import { installTauriExternalLinkHandler } from "./services/externalLinks";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);

registerServiceWorker();
registerTauriUpdater();
installTauriExternalLinkHandler();
