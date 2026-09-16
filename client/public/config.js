// Per-deployment configuration, served at /config.js and read before the app
// bundle. This copy is an empty placeholder: official builds keep the defaults
// compiled into the bundle.
//
// Self-hosting? Replace this file (the phase-server helm chart renders it from
// `web.defaultMultiplayerServerUrl` and `web.previewSiteUrl`) — the bundle needs
// no rebuild. Every key is optional, and the app ignores a malformed value:
//
//   window.__PHASE_CONFIG__ = {
//     multiplayerServerUrl: "wss://your-host/ws",
//     // Where a release build's "Try Preview" badge points (http or https).
//     previewSiteUrl: "https://preview.your-host/",
//   };
window.__PHASE_CONFIG__ = {};
