import { useEffect, useRef, useState } from "react";

import type { CardPrinting, PrintedRef } from "../adapter/types.ts";
import { matchSourcePrinting } from "../services/artSelection.ts";
import { createOwnerArtCatalogReader, deriveImageUrl, imageUrlSize } from "../services/scryfall.ts";
import type { ImageSize, OwnerArtCatalogReader, OwnerArtAuthority } from "../services/scryfall.ts";
import { cardCandidateGroups } from "../services/visualPacks/candidateKeys.ts";
import { VisualPackRepository } from "../services/visualPacks/repository.ts";
import type { VisualCandidateGroup } from "../services/visualPacks/repository.ts";
import type { CardImageSource, ImageRungs } from "../services/visualPacks/types.ts";
import { getEffectiveOffline, useEffectiveOffline } from "../stores/connectivityStore.ts";
import { usePreferencesStore } from "../stores/preferencesStore.ts";
import { nextImageSourceIndex } from "./imageSourceLadder.ts";
import { BoundedCache, useCardImage as useLegacyCardImage } from "./useLegacyCardImage.ts";

export { BoundedCache, useCardBackImage, useLocaleArt } from "./useLegacyCardImage.ts";
export type { SourcePrinting, UseCardBackImageResult } from "./useLegacyCardImage.ts";

type LegacyOptions = NonNullable<Parameters<typeof useLegacyCardImage>[1]>;
type ImageResult = ReturnType<typeof useLegacyCardImage>;

type UseCardImageOptions = LegacyOptions & {
  ownerPrinting?: CardPrinting | null;
  /** Object-owned Oracle and face; never changes the legacy lookup request. */
  ownerPrintedRef?: PrintedRef | null;
  showOwnerPrintings?: boolean;
};

interface OwnerInput {
  key: string;
  cardName: string;
  oracleId?: string;
  faceName?: string;
  faceIndex: number;
  setCode: string;
  collectorNumber: string;
  size: ImageSize;
  language: string;
  offline: boolean;
}

interface OwnerPresentation {
  key: string;
  sources: CardImageSource[];
  index: number;
  failed: Set<string>;
  remoteDone: boolean;
  settled: boolean;
  isRotated: boolean;
  isFlip: boolean;
}

// These instances never alias the normal hook's caches or repository.
const ownerPresentations = new BoundedCache<string, OwnerPresentation>(256);
let ownerRepository: VisualPackRepository | undefined;

function ownerExactGroups(id: string, faceIndex: number, language: string, size: ImageSize): VisualCandidateGroup[] {
  const build = (rung: ImageSize) => cardCandidateGroups({
    englishPrintingId: id,
    language: language === "en" ? undefined : language,
    faceIndex,
    variant: size === "art_crop" ? "art_crop" : "full_card",
    rung,
  });
  const requested = build(size);
  const small = size === "art_crop" ? [] : build("small");
  const normal = size === "art_crop" ? [] : build("normal");
  return requested.map((group, index) => ({
    requested: group.keys,
    ...(size === "art_crop" ? {} : { small: small[index]?.keys, normal: normal[index]?.keys }),
  }));
}

function admittedPrinting(authority: OwnerArtAuthority, input: OwnerInput) {
  return authority.oracleId && authority.printings
    ? matchSourcePrinting(authority.printings, input)
    : null;
}

function presentationResult(presentation: OwnerPresentation, advanceFailedSource: (src: string) => void): ImageResult {
  const source = presentation.sources[presentation.index] ?? null;
  return {
    src: source?.src ?? null,
    source,
    rungs: source?.kind === "fallback" ? undefined : source?.rungs,
    isLoading: source?.kind === "fallback" && !presentation.remoteDone,
    isRotated: presentation.isRotated,
    isFlip: presentation.isFlip,
    advanceFailedSource,
  };
}

/**
 * Revalidate generation/admission immediately before each overlay-initiated repository API call. A repository call initiated while admitted may finish its internal initialization, subscription, resolution and revision retry after admission is lost; those operations are not cancellable through the existing API. The overlay must start no further calls and must discard stale results before presentation, caching or publication.
 */
function useOwnerCardImageOverlay(input: OwnerInput | null): ImageResult | null {
  const readerRef = useRef<OwnerArtCatalogReader | null>(null);
  const latest = useRef(input);
  latest.current = input;
  const [presentation, setPresentation] = useState<OwnerPresentation | null>(null);
  const advanceRef = useRef<(key: string, src: string) => void>(() => {});
  const inputKey = input?.key ?? "";

  // Read current authority on every eligible render; no backend access before membership.
  let currentKey = "";
  let warm: OwnerPresentation | undefined;
  if (input) {
    readerRef.current ??= createOwnerArtCatalogReader(() => latest.current !== null && !getEffectiveOffline());
    const authority = readerRef.current.read(input);
    const printing = admittedPrinting(authority, input);
    if (printing) {
      const revision = ownerRepository?.currentRevision() ?? "0";
      currentKey = JSON.stringify([input.key, authority.key, printing.id, revision]);
      const cached = ownerPresentations.get(currentKey);
      if (cached?.settled) warm = cached;
    }
  }

  useEffect(() => {
    if (!inputKey) return;
    const reader = readerRef.current!;
    let disposed = false;
    let generation = 0;
    let sessionKey = "";
    let unlistenRepository: (() => void) | undefined;
    let selected = false;

    const reconcile = () => {
      const activeInput = latest.current;
      if (disposed || !activeInput || activeInput.key !== inputKey) return;
      const authority = reader.read(activeInput);
      const printing = admittedPrinting(authority, activeInput);
      if (!printing) {
        if (sessionKey) reader.release();
        generation += 1;
        sessionKey = "";
        unlistenRepository?.();
        unlistenRepository = undefined;
        advanceRef.current = () => {};
        if (selected) {
          selected = false;
          setPresentation(null);
        }
        return;
      }
      const admitted = () => {
        const current = latest.current;
        if (disposed || !current || current.key !== inputKey) return false;
        const snapshot = reader.read(current);
        return snapshot.key === authority.key && admittedPrinting(snapshot, current)?.id === printing.id;
      };
      if (!admitted()) return;
      ownerRepository ??= new VisualPackRepository();
      const repository = ownerRepository;
      if (!admitted()) return;
      const key = JSON.stringify([activeInput.key, authority.key, printing.id, repository.currentRevision()]);
      if (sessionKey === key) return;
      sessionKey = key;
      const captured = ++generation;
      unlistenRepository?.();
      unlistenRepository = undefined;
      const current = () => captured === generation && admitted();
      if (!current()) return;
      unlistenRepository = repository.subscribe(reconcile);
      const cached = ownerPresentations.get(key);
      let state: OwnerPresentation = cached ? {
        ...cached, failed: new Set(cached.failed),
      } : {
        key, sources: [], index: 0, failed: new Set(), remoteDone: activeInput.offline, settled: false,
        isRotated: authority.isRotated, isFlip: authority.isFlip,
      };
      let remoteStarted = false;
      const publish = (settled: boolean) => {
        if (!current()) return;
        state = { ...state, settled };
        ownerPresentations.set(key, { ...state, failed: new Set(state.failed) });
        selected = true;
        setPresentation(state);
      };
      const groups = ownerExactGroups(printing.id, authority.faceIndex, activeInput.language, activeInput.size);
      const continueRemote = async () => {
        if (remoteStarted || state.remoteDone || !current()) return;
        remoteStarted = true;
        const src = reader.imageUrl(printing, authority.faceIndex, activeInput.size, activeInput.language);
        let rungs: ImageRungs | undefined;
        if (src && activeInput.size !== "art_crop" && imageUrlSize(src) !== null) {
          rungs = { small: deriveImageUrl(src, "small"), normal: deriveImageUrl(src, "normal") };
        }
        if (!current()) return;
        const result = await repository.resolve({
          groups, rung: activeInput.size, allowRemote: true, remote: src ? { src, rungs } : null,
        });
        if (!current()) return;
        state.sources = result.sources.filter((source) => !source.src || !state.failed.has(source.src));
        state.index = 0;
        state.remoteDone = true;
        publish(true);
      };
      advanceRef.current = (failedKey, failedSrc) => {
        if (!current() || failedKey !== key) return;
        const next = nextImageSourceIndex(state.sources, state.index, state.failed, failedSrc);
        if (next === null) return;
        state.index = next;
        if (state.sources[next]?.kind === "fallback" && !state.remoteDone) {
          publish(false);
          void continueRemote();
        } else {
          publish(true);
        }
      };
      if (cached) {
        publish(cached.settled);
        if (!cached.settled) void continueRemote();
      } else {
        if (selected) {
          selected = false;
          setPresentation(null);
        }
        void (async () => {
          if (!current()) return;
          const result = await repository.resolve({ groups, rung: activeInput.size, allowRemote: false });
          if (!current()) return;
          state.sources = result.sources;
          state.index = 0;
          if (state.sources[0]?.kind === "installed" || activeInput.offline) {
            publish(true);
          } else {
            await continueRemote();
          }
        })();
      }
      // Locale preparation is isolated too, and starts only after membership.
      if (current()) void reader.prepareLocale(activeInput.language, activeInput.offline);
    };

    const unlistenReadiness = reader.subscribe(reconcile);
    reconcile();
    const activeInput = latest.current;
    if (activeInput) void reader.prepare(activeInput);
    return () => {
      disposed = true;
      generation += 1;
      unlistenReadiness();
      reader.release();
      unlistenRepository?.();
      advanceRef.current = () => {};
    };
  }, [inputKey]);

  const selected = presentation?.key === currentKey ? presentation : warm;
  return selected ? presentationResult(selected, (src) => advanceRef.current(selected.key, src)) : null;
}

/**
 * Normal rendering runs unchanged, including while owner art is pending or displayed.
 * An admitted exact owner printing replaces it when ready. Missing admitted art is
 * terminal null/nonloading, never a substitution from another printing. Toggle off
 * immediately reveals the still-live normal result; pending/rejected owners do not
 * blank it or add a loading flag. Owner admission requires identity delivered with
 * its carrier (`ownerPrintedRef`, or the caller's `oracleId`); never a name lookup.
 */
export function useCardImage(cardName: string, options?: UseCardImageOptions): ImageResult {
  let legacyOptions: Parameters<typeof useLegacyCardImage>[1] = options;
  if (options && ("ownerPrinting" in options || "ownerPrintedRef" in options || "showOwnerPrintings" in options)) {
    const { ownerPrinting: _owner, ownerPrintedRef: _ref, showOwnerPrintings: _show, ...ordinary } = options;
    legacyOptions = ordinary;
  }
  const normal = useLegacyCardImage(cardName, legacyOptions);
  const owner = options?.ownerPrinting;
  const eligible = typeof owner?.set_code === "string" && owner.set_code.length > 0
    && typeof owner.collector_number === "string" && owner.collector_number.length > 0;
  const enabled = usePreferencesStore((state) =>
    eligible ? options?.showOwnerPrintings ?? state.showOwnerPrintings : false);
  const offline = useEffectiveOffline();
  const language = usePreferencesStore((state) => eligible && enabled ? state.language : "en");
  const oracleId = options?.ownerPrintedRef?.oracle_id ?? options?.oracleId;
  let input: OwnerInput | null = null;
  if (eligible && enabled && oracleId) {
    const setCode = owner.set_code.toLowerCase();
    const collectorNumber = owner.collector_number;
    const faceName = options?.ownerPrintedRef?.face_name ?? options?.faceName;
    const faceIndex = options?.faceIndex ?? 0;
    const size = options?.size ?? "normal";
    input = {
      key: JSON.stringify([cardName, oracleId, faceName, faceIndex, setCode, collectorNumber, size, language, offline]),
      cardName, oracleId, faceName,
      faceIndex, setCode, collectorNumber, size, language, offline,
    };
  }
  const overlay = useOwnerCardImageOverlay(input);
  return overlay ?? normal;
}
