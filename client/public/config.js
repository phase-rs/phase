// Per-deployment configuration, served at /config.js and read before the app
// bundle. This copy is an empty placeholder: official builds keep the defaults
// compiled into the bundle.
//
// Self-hosting? Replace this file (the phase-server helm chart renders it from
// `web.defaultMultiplayerServerUrl`) — the bundle needs no rebuild:
//
//   window.__PHASE_CONFIG__ = { multiplayerServerUrl: "wss://your-host/ws" };
window.__PHASE_CONFIG__ = {};
