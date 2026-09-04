import { canAttemptNativeEngine, nativeEngineKeyForCurrentOrigin, prepareNativeEngineForOffline } from "./nativeEngine.ts";
import { prepareOfflineAssets, type OfflineAssetsReadiness } from "./offlineAssets.ts";
import { loadVisualPackBackend } from "./platform.ts";
import { checkAppShellReadiness, type AppShellReadiness } from "../pwa/registerServiceWorker.ts";
import { getEffectiveOffline } from "../stores/connectivityStore.ts";

export type OfflinePreparationStatus =
  | "ready"
  | "failed"
  | "reconnect-required"
  | "reload-or-relaunch-required";

export type OfflinePreparationCapabilityName =
  | "appShell"
  | "browserEngine"
  | "scryfallSearch"
  | "preconCatalog"
  | "bundledAiCatalog"
  | "deckLibrary"
  | "nativeEngine";

export type OfflinePreparationCapabilityStatus =
  | "ready"
  | "not-ready"
  | "reload-required"
  | "not-applicable"
  | "not-installed";

export interface OfflinePreparationCapability {
  readonly status: OfflinePreparationCapabilityStatus;
}

export interface OfflinePreparationVisualCapability {
  readonly status: "ready" | "not-installed" | "warning";
  readonly issueKinds?: readonly string[];
}

export interface OfflinePreparationResult {
  readonly status: OfflinePreparationStatus;
  readonly capabilities: Readonly<Record<OfflinePreparationCapabilityName, OfflinePreparationCapability>>;
  readonly visualPacks: OfflinePreparationVisualCapability;
  /** Required capabilities that are incomplete in this fresh preparation. */
  readonly requiredGaps: readonly OfflinePreparationCapabilityName[];
}

function unavailableCapabilities(): Record<OfflinePreparationCapabilityName, OfflinePreparationCapability> {
  return {
    appShell: { status: "not-ready" },
    browserEngine: { status: "not-ready" },
    scryfallSearch: { status: "not-ready" },
    preconCatalog: { status: "not-ready" },
    bundledAiCatalog: { status: "not-ready" },
    deckLibrary: { status: "not-ready" },
    nativeEngine: { status: "not-applicable" },
  };
}

function shellResult(readiness: AppShellReadiness, final = false): OfflinePreparationResult | null {
  if (readiness.status === "ready") return null;
  const capabilities = unavailableCapabilities();
  capabilities.appShell = { status: readiness.status === "reload-required" ? "reload-required" : "not-ready" };
  const reload = readiness.status === "reload-required"
    || readiness.reason === "update-in-progress"
    || (final && readiness.reason === "lifecycle-changed");
  return {
    status: reload ? "reload-or-relaunch-required" : "failed",
    capabilities,
    visualPacks: { status: "not-installed" },
    requiredGaps: ["appShell"],
  };
}

function assetCapabilities(readiness: OfflineAssetsReadiness): Record<
  Exclude<OfflinePreparationCapabilityName, "appShell" | "nativeEngine">,
  OfflinePreparationCapability
> {
  return {
    browserEngine: { status: readiness.capabilities.engine.status },
    scryfallSearch: { status: readiness.capabilities.scryfallSearch.status },
    preconCatalog: { status: readiness.capabilities.preconCatalog.status },
    bundledAiCatalog: { status: readiness.capabilities.bundledAiCatalog.status },
    deckLibrary: { status: readiness.capabilities.deckLibrary.status },
  };
}

async function prepareVisualPacks(): Promise<OfflinePreparationVisualCapability> {
  try {
    const backend = await loadVisualPackBackend();
    if (!backend) return { status: "not-installed" };
    const catalog = await backend.catalogStatus();
    if (catalog.status === "empty") return { status: "not-installed" };
    if (catalog.status === "invalid") return { status: "warning", issueKinds: ["invalid-catalog"] };
    if (catalog.summary.installedPacks.length === 0) return { status: "not-installed" };
    const verification = await backend.verify("full");
    return verification.issues.length === 0
      ? { status: "ready" }
      : { status: "warning", issueKinds: verification.issues.map((issue) => issue.kind) };
  } catch {
    return { status: "warning", issueKinds: ["unavailable"] };
  }
}

function requiredGaps(
  capabilities: Record<OfflinePreparationCapabilityName, OfflinePreparationCapability>,
): OfflinePreparationCapabilityName[] {
  return (Object.keys(capabilities) as OfflinePreparationCapabilityName[]).filter((name) => {
    const status = capabilities[name].status;
    return status === "not-ready" || status === "reload-required";
  });
}

/**
 * Runs the existing, release-specific preparation authorities once and reports
 * their fresh result. It deliberately never writes connectivity policy.
 */
export async function prepareForOffline({ nativeEngineEnabled }: { nativeEngineEnabled: boolean }): Promise<OfflinePreparationResult> {
  if (getEffectiveOffline()) {
    return {
      status: "reconnect-required",
      capabilities: unavailableCapabilities(),
      visualPacks: { status: "not-installed" },
      requiredGaps: [],
    };
  }

  const initialShell = shellResult(await checkAppShellReadiness());
  if (initialShell) return initialShell;

  const assets = await prepareOfflineAssets();
  const visualPacks = await prepareVisualPacks();
  let nativeEngine: OfflinePreparationCapability = (() => {
    if (!nativeEngineEnabled || !canAttemptNativeEngine(true)) return { status: "not-applicable" };
    return { status: "not-ready" };
  })();
  if (nativeEngine.status === "not-ready") {
    const key = nativeEngineKeyForCurrentOrigin();
    if (key) {
      try {
        await prepareNativeEngineForOffline(key);
        nativeEngine = { status: "ready" };
      } catch {
        // The named capability is the user-facing diagnostic; transport owns details.
      }
    } else {
      nativeEngine = { status: "not-applicable" };
    }
  }

  const capabilities: Record<OfflinePreparationCapabilityName, OfflinePreparationCapability> = {
    appShell: { status: "ready" },
    ...assetCapabilities(assets),
    nativeEngine,
  };
  const finalShell = await checkAppShellReadiness();
  const finalShellResult = shellResult(finalShell, true);
  if (finalShellResult) {
    const mergedCapabilities = { ...capabilities, appShell: finalShellResult.capabilities.appShell };
    return {
      ...finalShellResult,
      capabilities: mergedCapabilities,
      visualPacks,
      requiredGaps: requiredGaps(mergedCapabilities),
    };
  }

  const gaps = requiredGaps(capabilities);
  return {
    status: gaps.length === 0 ? "ready" : capabilities.browserEngine.status === "reload-required"
      ? "reload-or-relaunch-required"
      : "failed",
    capabilities,
    visualPacks,
    requiredGaps: gaps,
  };
}
