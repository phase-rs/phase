import { canAttemptNativeEngine, nativeEngineKeyForCurrentOrigin, prepareNativeEngineForOffline } from "./nativeEngine.ts";
import { prepareOfflineAssets, type OfflineAssetsReadiness } from "./offlineAssets.ts";
import { loadVisualPackBackend } from "./platform.ts";
import { MANA_SYMBOL_SHARDS } from "./scryfall.ts";
import type { VisualPackBackend } from "./visualPacks/backend.ts";
import { cardBackCandidate, manaSymbolCandidate } from "./visualPacks/candidateKeys.ts";
import {
  packId,
  type CatalogStatus,
  type OperationId,
  type PackId,
  type ResolutionKey,
} from "./visualPacks/types.ts";
import { checkAppShellReadiness, type AppShellReadiness } from "../pwa/registerServiceWorker.ts";
import { getEffectiveOffline } from "../stores/connectivityStore.ts";

const CORE_PACK = packId("core");

/**
 * How many objects a core install writes: the card back plus one SVG per mana
 * shard, matching `coreDescriptors()` in `visualPacks/browser/scryfallBulk.ts`.
 *
 * Only a progress hint — the install derives the real membership itself, and
 * nothing validates this figure. It is computed rather than written as a
 * literal so the symbol half cannot drift; `estimateInstall` is deliberately
 * NOT the source, because every count it returns is a pre-rendered display
 * string that can legitimately read "unknown".
 */
const CORE_OBJECT_ESTIMATE = 1 + MANA_SYMBOL_SHARDS.length;

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
  | "coreVisuals"
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

/**
 * The state of the installed CARD ART, which is reported but never required.
 *
 * Deliberately excludes the `core` pack, which is a required capability of its
 * own above: core is chrome every game screen draws, whereas art is a
 * preference — a deck plays correctly with no art installed, it just renders
 * without it. Folding the two together is what let this panel report "ready"
 * while nothing at all was cached.
 */
export interface OfflinePreparationVisualCapability {
  readonly status: "ready" | "not-installed" | "warning";
  readonly issueKinds?: readonly string[];
  /** The art packs on disk, so the panel can name what is cached rather than
   *  only whether something is. Never includes `core`. */
  readonly installedPacks?: readonly PackId[];
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
    coreVisuals: { status: "not-ready" },
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
  Exclude<OfflinePreparationCapabilityName, "appShell" | "coreVisuals" | "nativeEngine">,
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

/** The installed packs that carry card art. `core` is excluded: it is chrome,
 *  and it is reported by its own required capability. */
function cardArtPacks(catalog: CatalogStatus): readonly PackId[] {
  if (catalog.status !== "ready") return [];
  return catalog.summary.installedPacks
    .map((pack) => pack.packId)
    .filter((pack) => pack !== CORE_PACK);
}

/**
 * How this platform's visual-pack backend answered when asked for.
 *
 * `absent` and `unavailable` are deliberately separate: a platform with no
 * backend at all is not a gap a user can close, while one whose backend failed
 * to load is an error that a retry may fix. Collapsing them would either
 * mark healthy platforms permanently unready or hide a real failure.
 */
type VisualPackAccess =
  | { readonly kind: "backend"; readonly backend: VisualPackBackend }
  | { readonly kind: "absent" }
  | { readonly kind: "unavailable" };

async function accessVisualPacks(): Promise<VisualPackAccess> {
  try {
    const backend = await loadVisualPackBackend();
    return backend ? { kind: "backend", backend } : { kind: "absent" };
  } catch {
    return { kind: "unavailable" };
  }
}

async function prepareVisualPacks(access: VisualPackAccess): Promise<OfflinePreparationVisualCapability> {
  if (access.kind === "unavailable") return { status: "warning", issueKinds: ["unavailable"] };
  if (access.kind === "absent") return { status: "not-installed", installedPacks: [] };
  const backend = access.backend;
  try {
    const catalog = await backend.catalogStatus();
    if (catalog.status === "invalid") return { status: "warning", issueKinds: ["invalid-catalog"] };
    const installedPacks = cardArtPacks(catalog);
    if (installedPacks.length === 0) return { status: "not-installed", installedPacks: [] };
    const verification = await backend.verify("full");
    return verification.issues.length === 0
      ? { status: "ready", installedPacks }
      : { status: "warning", issueKinds: verification.issues.map((issue) => issue.kind), installedPacks };
  } catch {
    return { status: "warning", issueKinds: ["unavailable"] };
  }
}

/** Every candidate key a board draws from the core pack, in the same form
 *  `useManaSymbolImage` and the card-back hook look them up by. */
function coreCandidateKeys(): ResolutionKey[] {
  return [
    { kind: "candidate", key: cardBackCandidate() },
    ...MANA_SYMBOL_SHARDS.map((shard) => ({ kind: "candidate" as const, key: manaSymbolCandidate(shard) })),
  ];
}

/**
 * The core assets a board would fail to draw right now.
 *
 * `catalogStatus()` reports the pack RECEIPT, which survives Cache Storage
 * eviction and corruption — so a receipt alone cannot say the pips are
 * actually there. `resolve()` can: it admits an object only when
 * `cache.match(object.path)` succeeds, and each match carries its `packId`,
 * which is the pack-scoped integrity answer `verify()` does not give.
 */
async function missingCoreAssets(backend: VisualPackBackend): Promise<number> {
  const keys = coreCandidateKeys();
  const resolution = await backend.resolve(keys);
  const resolved = new Set(
    resolution.entries
      .filter((entry) => entry.matches.some((match) => match.packId === CORE_PACK))
      .map((entry) => entry.key.key),
  );
  return keys.length - resolved.size;
}

/**
 * Drives a core install or repair to a terminal state.
 *
 * The subscription is opened BEFORE `start()`, so no event of this operation
 * can be emitted before there is a listener for it; events belonging to other
 * operations (a concurrent deck-library reconcile) are filtered by id. The
 * single `operationStatus` read after `start()` closes the remaining window —
 * `start()` launches `run()` without awaiting it, so an install that finished
 * between the two would otherwise leave this waiting on an event already sent.
 */
async function runCoreInstall(backend: VisualPackBackend): Promise<void> {
  let operation: OperationId | null = null;
  let settle: () => void = () => undefined;
  const settled = new Promise<void>((resolve) => { settle = resolve; });
  const unsubscribe = await backend.subscribeProgress((event) => {
    if (operation === null || event.operation.operationId !== operation) return;
    if (event.phase === "completed" || event.phase === "failed" || event.phase === "cancelled") settle();
  });
  try {
    const response = await backend.start({
      kind: "install",
      selector: { kind: "core" },
      objectEstimate: CORE_OBJECT_ESTIMATE,
    });
    if (response.status === "healthy") return;
    operation = response.operationId;
    const current = await backend.operationStatus(operation);
    if (current.state === "completed" || current.state === "cancelled") return;
    await settled;
  } finally {
    unsubscribe();
  }
}

/**
 * Ensures the core visual pack — the card back and every mana symbol — is on
 * disk, and reports whether it is.
 *
 * The one capability here that INSTALLS a visual pack rather than only
 * inspecting one, and required rather than advisory, because core is what
 * every game screen draws regardless of deck or set: a mana cost with no pips
 * is unreadable, and no other pack carries them. It is also small enough
 * (~85 files) that routing the user to the Visual Packs panel to choose it
 * would be ceremony rather than consent — the same judgement `prepareOffline`
 * already makes for the native engine and the deck-library pack.
 *
 * The verdict is decided by `missingCoreAssets`, NOT by the pack receipt, and
 * both before and after any work: a receipt outlives the bytes it describes,
 * so "installed" and "drawable offline" are different questions.
 */
async function prepareCoreVisuals(access: VisualPackAccess): Promise<OfflinePreparationCapability> {
  // No backend at all is not a gap the user can close, so it must not hold
  // offline preparation permanently red. A backend that failed to LOAD is a
  // different answer: core is genuinely absent and a retry may fix it.
  if (access.kind === "absent") return { status: "not-applicable" };
  if (access.kind === "unavailable") return { status: "not-ready" };
  const backend = access.backend;
  try {
    if (await missingCoreAssets(backend) > 0) {
      const catalog = await backend.catalogStatus();
      const receipt = catalog.status === "ready"
        && catalog.summary.installedPacks.some((pack) => pack.packId === CORE_PACK);
      // A receipt whose bytes are gone cannot be refilled through `start()`:
      // BOTH an install and a repair key on that receipt standing at the
      // current root, so the install short-circuits to `healthy` and the
      // repair filters its own selector out — see the `cacheContains` note in
      // scryfallBackend.ts, which records this as MEASURED. Dropping the
      // receipt first is the recovery that note prescribes, and it costs the
      // same ~85 small files the install downloads anyway. Nothing declares a
      // dependency on `core`, so the removal cannot be rejected.
      if (receipt) await backend.remove({ kind: "packs", packIds: [CORE_PACK] }, "reject_dependents");
      await runCoreInstall(backend);
    }
    return await missingCoreAssets(backend) === 0 ? { status: "ready" } : { status: "not-ready" };
  } catch {
    return { status: "not-ready" };
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
 *
 * "Prepare" means prepare: like the native engine and the deck-library pack,
 * the core visual pack is INSTALLED here when missing rather than merely
 * reported, so that a green checklist means the next offline session actually
 * works. Card art is the one thing left to the user, and it is now named
 * rather than silently omitted.
 */
export async function prepareForOffline({ nativeEngineEnabled }: { nativeEngineEnabled: boolean }): Promise<OfflinePreparationResult> {
  if (getEffectiveOffline()) {
    return {
      status: "reconnect-required",
      capabilities: unavailableCapabilities(),
      visualPacks: { status: "not-installed", installedPacks: [] },
      requiredGaps: [],
    };
  }

  const initialShell = shellResult(await checkAppShellReadiness());
  if (initialShell) return initialShell;

  const assets = await prepareOfflineAssets();
  // One backend for both, and core FIRST: the art inventory must be read after
  // the core install, or a first preparation reports the art state of a catalog
  // it is about to change.
  const visualPackAccess = await accessVisualPacks();
  const coreVisuals = await prepareCoreVisuals(visualPackAccess);
  const visualPacks = await prepareVisualPacks(visualPackAccess);
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
    coreVisuals,
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
