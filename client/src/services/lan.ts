import { invoke } from "@tauri-apps/api/core";

import { isDesktopTauri } from "./platform";
import { nativeEngineKeyForCurrentOrigin, type NativeEngineKey } from "./nativeEngine";
import { getEffectiveOffline } from "../stores/connectivityStore";

export interface LanServerStatus {
  running: boolean;
  key: NativeEngineKey | null;
  addresses: string[];
}

export interface DiscoveredLanServer {
  name: string;
  url: string;
  channel: string | null;
}

let capability: Promise<boolean> | undefined;
let supported = false;

/** Read-only probe: old shells and mobile never receive lifecycle commands. */
export function initializeLanCapabilities(): Promise<boolean> {
  if (!isDesktopTauri()) return Promise.resolve(false);
  capability ??= invoke<{ supported: boolean }>("lan_capabilities")
    .then((result) => { supported = result?.supported === true; return supported; })
    .catch(() => false);
  return capability;
}

/** The store's URL serializer omits :80; restore it for the native boundary.
 * Numeric IPv4 parsing remains strict: no DNS or legacy URL host aliases. */
export function normalizeLanEndpoint(value: string): string | null {
  const match = /^ws:\/\/((?:[0-9]{1,3}\.){3}[0-9]{1,3})(?::([0-9]+))?\/ws$/.exec(value);
  if (!match) return null;
  const octets = match[1].split(".");
  if (octets.some((part) => String(Number(part)) !== part || Number(part) > 255)) return null;
  const port = match[2] === undefined ? 80 : Number(match[2]);
  if (port < 1 || port > 65535) return null;
  const [a, b] = octets.map(Number);
  if (!(a === 10 || a === 127 || (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168))) return null;
  return `ws://${match[1]}:${port}/ws`;
}

export function isLanEndpoint(value: string): boolean {
  return normalizeLanEndpoint(value) !== null;
}

export function canUseLanBridge(url: string): boolean {
  return isDesktopTauri() && supported && isLanEndpoint(url)
    && ["https://phase-rs.dev", "https://app.phase-rs.dev", "https://preview.phase-rs.dev"].includes(window.location.origin);
}

async function invokeLan<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (!await initializeLanCapabilities()) throw new Error("LAN commands unavailable");
  return invoke<T>(command, args);
}

export function startLanServer(): Promise<LanServerStatus> {
  const key = nativeEngineKeyForCurrentOrigin();
  if (!key) return Promise.reject(new Error("LAN engine unavailable for this origin"));
  return invokeLan("start_lan_server", {
    key, intent: getEffectiveOffline() ? "start_offline" : "start_online",
  });
}

export function getLanServerStatus(): Promise<LanServerStatus> {
  return invokeLan("lan_server_status");
}

export function stopLanServer(): Promise<void> {
  return invokeLan("stop_lan_server");
}

export async function discoverLanServers(): Promise<DiscoveredLanServer[]> {
  const results = await invokeLan<DiscoveredLanServer[]>("discover_lan_servers");
  return [...new Map(results.filter((server) => isLanEndpoint(server.url))
    .map((server) => [new URL(server.url).href, server])).values()];
}

export function authorizeLanServer(url: string): Promise<void> {
  return invokeLan("authorize_lan_server", { url: normalizeLanEndpoint(url) ?? url });
}
