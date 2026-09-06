import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { AnimatePresence, motion } from "framer-motion";

import { useCardImage } from "../../hooks/useCardImage";
import { useDeckCardData } from "../../hooks/useDeckCardData";
import { useLongPress } from "../../hooks/useLongPress";
import { BASIC_LAND_NAMES } from "../../constants/game";
import { useDraftStore } from "../../stores/draftStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { formatMetadata } from "../../data/formatRegistry";
import {
  commanderPartnerCandidates,
  isCardCommanderEligibleForFormat,
} from "../../services/engineRuntime";
import { menuButtonClass } from "../menu/buttonStyles";
import { PopoverMenu } from "../menu/PopoverMenu";
import { CommanderPanel } from "../deck-builder/CommanderPanel";
import { getCardImageSrcSetProps } from "../card/cardImageSrcSet.ts";
import type { GameFormat } from "../../adapter/types";
import type { DeckEntry } from "../../services/deckParser";
import type { ParsedDeck } from "../../services/deckParser";
import {
  evaluateDeckCompatibility,
  type DeckCompatibilityResult,
} from "../../services/deckCompatibility";
import type {
  DraftCardInstance,
  DraftPlayerView,
  DraftPoolGroupKind,
} from "../../adapter/draft-adapter";
import { EMPTY_DRAFT_POOL_GROUPS } from "../../adapter/draft-adapter";
import {
  axisKinds,
  EMPTY_POOL_FILTER,
  fetchPoolFilterOptions,
  filterPoolListing,
  poolFilterActive,
  toggleKind,
} from "../../viewmodel/limitedPoolFilter";
import type { PoolFilter, PoolFilterOptions } from "../../adapter/draft-adapter";
import { POOL_GROUP_LABEL_KEYS } from "./poolGroupLabels";
import type { CardHoverInfo } from "../card/CardPreview";
import { HoverCardPreview } from "../card/HoverCardPreview";
import { ManaCurve } from "./ManaCurve";
import { AverageManaCost, DeckStatistics } from "./DeckStatistics";
import { DraftWorkspace } from "./workspace/DraftWorkspace";
import { useDraftWorkspaceDrag } from "./workspace/useDraftWorkspaceDrag";
import type { DraftWorkspaceState } from "./workspace/types";
import {
  type DraftWorkspacePreferences,
  type ResponsiveDraftLayout,
} from "./workspace/workspacePreferences";
import {
  countProjectedNames,
  projectDeckNames,
} from "./workspace/workspaceProjection";

// Shared enter/exit for cards moving between the pool and the deck.
const CARD_MOTION = {
  layout: true,
  initial: { opacity: 0, scale: 0.85 },
  animate: { opacity: 1, scale: 1 },
  exit: { opacity: 0, scale: 0.85 },
  transition: { duration: 0.18, ease: "easeOut" as const },
};

// ── Constants ───────────────────────────────────────────────────────────

const BASIC_LANDS = [
  { name: "Plains", color: "W", colorClass: "bg-yellow-200" },
  { name: "Island", color: "U", colorClass: "bg-blue-400" },
  { name: "Swamp", color: "B", colorClass: "bg-slate-400" },
  { name: "Mountain", color: "R", colorClass: "bg-red-500" },
  { name: "Forest", color: "G", colorClass: "bg-green-500" },
] as const;

const LAND_COLOR_CLASSES: Record<string, string> = {
  Plains: "bg-yellow-200",
  Island: "bg-blue-400",
  Swamp: "bg-slate-400",
  Mountain: "bg-red-500",
  Forest: "bg-green-500",
  Wastes: "bg-neutral-300",
};

// Stable empty defaults so the controlled path's memo dependencies do not
// churn on every render when a caller omits a facade prop.
const EMPTY_MAIN_DECK: string[] = [];
const EMPTY_LAND_COUNTS: Record<string, number> = {};
const DECKBUILDING_INTERACTION = {
  interactionGeneration: 0,
  pickInteractionLocked: false,
  pendingPickIntent: null,
} as const;

// ── Card image tile ─────────────────────────────────────────────────────

interface CardTileProps {
  card: DraftCardInstance;
  count?: number;
  dimmed?: boolean;
  onClick: () => void;
  onHover: (info: CardHoverInfo | null) => void;
}

function CardTile({ card, count, dimmed, onClick, onHover }: CardTileProps) {
  const cardName = card.name ?? "";
  const { src, isLoading, rungs, advanceFailedSource } = useCardImage(cardName, {
    size: "normal",
    sourcePrinting: { setCode: card.set_code, collectorNumber: card.collector_number },
  });
  const hoverInfo = {
    name: cardName,
    sourcePrinting: { setCode: card.set_code, collectorNumber: card.collector_number },
  };
  const { handlers, firedRef } = useLongPress(() => onHover(hoverInfo));

  const handleClick = () => {
    if (firedRef.current) {
      firedRef.current = false;
      return;
    }
    onClick();
  };

  return (
    <button
      onClick={handleClick}
      onMouseEnter={() => onHover(hoverInfo)}
      onMouseLeave={() => onHover(null)}
      {...handlers}
      className={`relative cursor-pointer overflow-hidden rounded-[14px] ring-1 ring-white/10 transition-all duration-150 hover:scale-[1.02] hover:ring-white/20
        ${dimmed ? "opacity-70 hover:opacity-90" : ""}`}
    >
      {isLoading ? (
        <div className="flex aspect-[488/680] animate-pulse items-center justify-center bg-white/5">
          <span className="px-2 text-center text-xs text-white/40">{cardName}</span>
        </div>
      ) : !src ? (
        <div
          className="flex aspect-[488/680] items-center justify-center bg-white/5"
          role="img"
          aria-label={cardName}
        >
          <span className="px-2 text-center text-xs text-white/40">{cardName}</span>
        </div>
      ) : (
        <img
          src={src}
          {...getCardImageSrcSetProps(src, rungs)}
          alt={cardName}
          draggable={false}
          onError={() => advanceFailedSource?.(src)}
          className="aspect-[488/680] w-full object-cover"
        />
      )}
      <div className="absolute inset-x-0 bottom-0 bg-gradient-to-t from-black/80 to-transparent px-1.5 py-1">
        <span className="line-clamp-1 text-[10px] leading-tight text-white/80">
          {cardName}
        </span>
      </div>
      {count !== undefined && count > 1 && (
        <div className="absolute right-1 top-1 flex h-5 w-5 items-center justify-center rounded-full bg-black/70 text-[10px] font-bold text-white">
          {count}
        </div>
      )}
    </button>
  );
}

// ── Land row ────────────────────────────────────────────────────────────

interface LandRowProps {
  name: string;
  colorClass: string;
  count: number;
  onDecrement: () => void;
  onIncrement: () => void;
}

function LandRow({ name, colorClass, count, onDecrement, onIncrement }: LandRowProps) {
  const { t } = useTranslation("draft");
  return (
    <div className="flex items-center gap-2">
      <div className={`h-3 w-3 shrink-0 rounded-full ${colorClass}`} />
      <span className="flex-1 text-sm text-white/60">{name}</span>
      <button
        type="button"
        onClick={onDecrement}
        disabled={count <= 0}
        aria-label={t("limitedDeck.removeCard", { name })}
        className={menuButtonClass({ tone: "neutral", size: "icon", disabled: count <= 0, className: "font-bold" })}
      >
        -
      </button>
      <span className="w-6 text-center text-sm tabular-nums text-white">{count}</span>
      <button
        type="button"
        onClick={onIncrement}
        aria-label={t("limitedDeck.addCard", { name })}
        className={menuButtonClass({ tone: "neutral", size: "icon", className: "font-bold" })}
      >
        +
      </button>
    </div>
  );
}

// ── Helpers ─────────────────────────────────────────────────────────────

function groupByName(
  cards: DraftCardInstance[],
  nameList: string[],
): { card: DraftCardInstance; count: number }[] {
  const countMap = new Map<string, number>();
  for (const name of nameList) {
    countMap.set(name, (countMap.get(name) ?? 0) + 1);
  }

  const seen = new Set<string>();
  const groups: { card: DraftCardInstance; count: number }[] = [];
  for (const card of cards) {
    if (!seen.has(card.name) && countMap.has(card.name)) {
      seen.add(card.name);
      groups.push({ card, count: countMap.get(card.name)! });
    }
  }

  return groups;
}

function computeRemainingPool(
  pool: DraftCardInstance[],
  mainDeck: string[],
): DraftCardInstance[] {
  const deckCounts = new Map<string, number>();
  for (const name of mainDeck) {
    deckCounts.set(name, (deckCounts.get(name) ?? 0) + 1);
  }

  const remaining: DraftCardInstance[] = [];
  const used = new Map<string, number>();
  for (const card of pool) {
    const usedCount = used.get(card.name) ?? 0;
    const deckCount = deckCounts.get(card.name) ?? 0;
    if (usedCount < deckCount) {
      used.set(card.name, usedCount + 1);
    } else {
      remaining.push(card);
    }
  }
  return remaining;
}

function sameOrderedNames(left: readonly string[], right: readonly string[]): boolean {
  return left.length === right.length && left.every((name, index) => name === right[index]);
}

function namesAreBacked(names: readonly string[], counts: ReadonlyMap<string, number>): boolean {
  const remaining = new Map(counts);
  return names.every((name) => {
    const count = remaining.get(name) ?? 0;
    if (count <= 0) return false;
    remaining.set(name, count - 1);
    return true;
  });
}

function useCommanderDesignation({
  commandersRequired,
  deckFormat,
  deckEntries,
  draftSetCodes,
}: {
  commandersRequired: number;
  deckFormat: GameFormat | null;
  deckEntries: DeckEntry[];
  draftSetCodes: readonly string[];
}) {
  const [commanders, setCommanders] = useState<string[]>([]);
  const [commanderEligibleNames, setCommanderEligibleNames] = useState<Set<string> | null>(null);
  const [eligibilityFailed, setEligibilityFailed] = useState(false);
  const commandersRef = useRef(commanders);
  const requestGenerationRef = useRef(0);
  const deckCounts = useMemo(
    () => new Map(deckEntries.map((entry) => [entry.name, entry.count])),
    [deckEntries],
  );
  const deckCountsRef = useRef(deckCounts);
  commandersRef.current = commanders;

  useEffect(() => {
    return () => {
      requestGenerationRef.current += 1;
    };
  }, [commandersRequired, deckFormat]);

  useLayoutEffect(() => {
    deckCountsRef.current = deckCounts;
  }, [deckCounts]);

  useEffect(() => {
    if (commandersRequired === 0 || !deckFormat) {
      requestGenerationRef.current += 1;
      commandersRef.current = [];
      setCommanders((current) => current.length === 0 ? current : []);
      setCommanderEligibleNames(null);
      setEligibilityFailed(false);
      return;
    }
    let cancelled = false;
    const names = [...new Set(deckEntries.map((entry) => entry.name))];
    Promise.all(names.map(async (name) => (
      [name, await isCardCommanderEligibleForFormat(name, deckFormat)] as const
    )))
      .then((results) => {
        if (cancelled) return;
        setCommanderEligibleNames(
          new Set(results.filter(([, eligible]) => eligible).map(([name]) => name)),
        );
        setEligibilityFailed(false);
      })
      .catch(() => {
        if (cancelled) return;
        setCommanderEligibleNames(null);
        setEligibilityFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [commandersRequired, deckFormat, deckEntries]);

  const isCommanderEligible = useCallback(
    (name: string) => commanderEligibleNames?.has(name) ?? false,
    [commanderEligibleNames],
  );

  const handleSetCommander = useCallback((cardName: string) => {
    if (!commanderEligibleNames?.has(cardName)) return;
    const premise = [...commandersRef.current];
    const generation = ++requestGenerationRef.current;
    void (async () => {
      let pairs = false;
      if (premise.length === 1) {
        try {
          pairs = (
            await commanderPartnerCandidates(premise[0], [cardName], draftSetCodes)
          ).includes(cardName);
        } catch {
          return;
        }
      }
      if (
        generation !== requestGenerationRef.current
        || !sameOrderedNames(commandersRef.current, premise)
      ) return;

      // CR 702.124g: partner can never produce more than two commanders.
      const next = pairs && premise.length === 1 ? [...premise, cardName] : [cardName];
      if (next.length > 2 || !namesAreBacked(next, deckCountsRef.current)) return;
      setCommanders((current) => {
        if (
          generation !== requestGenerationRef.current
          || !sameOrderedNames(current, premise)
          || !namesAreBacked(next, deckCountsRef.current)
        ) return current;
        commandersRef.current = next;
        return next;
      });
    })();
  }, [commanderEligibleNames, draftSetCodes]);

  const handleRemoveCommander = useCallback((cardName: string) => {
    requestGenerationRef.current += 1;
    setCommanders((current) => {
      const index = current.indexOf(cardName);
      if (index < 0) return current;
      const next = [...current.slice(0, index), ...current.slice(index + 1)];
      commandersRef.current = next;
      return next;
    });
  }, []);

  useEffect(() => {
    setCommanders((current) => {
      const remaining = new Map(deckCounts);
      const next = current.filter((name) => {
        const count = remaining.get(name) ?? 0;
        if (count <= 0) return false;
        remaining.set(name, count - 1);
        return true;
      });
      if (next.length === current.length) return current;
      requestGenerationRef.current += 1;
      commandersRef.current = next;
      return next;
    });
  }, [deckCounts]);

  return {
    commanders,
    eligibilityFailed,
    isCommanderEligible,
    handleSetCommander,
    handleRemoveCommander,
    designationSatisfied: commanders.length >= commandersRequired,
  };
}

type CommanderDraftCompatibilityState =
  | { key: string; status: "pending" }
  | { key: string; status: "resolved"; result: DeckCompatibilityResult }
  | { key: string; status: "error" };

function useCommanderDraftCompatibility({
  enforceCompatibility,
  selectedFormat,
  main,
  commanders,
}: {
  enforceCompatibility: boolean;
  selectedFormat: "CommanderDraft" | null;
  main: DeckEntry[];
  commanders: string[];
}) {
  const request = useMemo<ParsedDeck>(() => ({
    main,
    sideboard: [],
    commander: commanders,
  }), [main, commanders]);
  const key = useMemo(() => JSON.stringify({
    selectedFormat,
    main,
    sideboard: [],
    commander: commanders,
    planarDeck: [],
    schemeDeck: [],
    signatureSpell: [],
    companion: null,
  }), [selectedFormat, main, commanders]);
  const [state, setState] = useState<CommanderDraftCompatibilityState | null>(null);
  const generationRef = useRef(0);

  useEffect(() => {
    const generation = ++generationRef.current;
    setState({ key, status: "pending" });
    evaluateDeckCompatibility(request, { selectedFormat })
      .then((result) => {
        if (generation === generationRef.current) {
          setState({ key, status: "resolved", result });
        }
      })
      .catch(() => {
        if (generation === generationRef.current) setState({ key, status: "error" });
      });
  }, [key, request]);

  const currentState = state?.key === key ? state : null;
  const result = currentState?.status === "resolved" ? currentState.result : null;
  return {
    compatible: !enforceCompatibility || result?.selected_format_compatible === true,
    reasons: result?.selected_format_reasons ?? [],
    pending: enforceCompatibility && currentState?.status === "pending",
    unavailable: enforceCompatibility && currentState?.status === "error",
    colorDistribution: result?.color_distribution ?? [],
  };
}

// ── Main component ──────────────────────────────────────────────────────

interface LimitedDeckBuilderProps {
  view?: DraftPlayerView | null;
  mainDeck?: string[];
  landCounts?: Record<string, number>;
  onAddToDeck?: (cardName: string) => void;
  onRemoveFromDeck?: (cardName: string) => void;
  onSetLandCount?: (landName: string, count: number) => void;
  /**
   * Receives the designated commanders (CR 903.3 / CR 702.124h).
   * The argument now reaches `DraftAction::SubmitDeck.commanders`: through
   * `multiplayerDraftStore.submitDeck` on the P2P transport, and explicitly as
   * `[]` on the local wasm path, where `LocalDraftKind` is "Quick" | "Sealed"
   * and CR 903.1 scopes the designation to the Commander variant (D6).
   */
  onSubmitDeck?: (commanders: string[]) => Promise<void> | void;
  submissionError?: string | null;
  showSuggestions?: boolean;
  local?: WorkspaceDeckBuilderController;
  responsiveLayout?: ResponsiveDraftLayout;
  responsiveHeightMode?: "viewport" | "container";
}

// Local (Quick/Cube/Sealed) deckbuilding drives one authoritative workspace
// instance. The store owns all mutation and identity; this controller only
// forwards intent and the derived board back to the store.
interface WorkspaceDeckBuilderControllerBase {
  view: DraftPlayerView;
  workspace: DraftWorkspaceState;
  preferences: DraftWorkspacePreferences;
  interactionLocked: boolean;
  onWorkspaceChange: (next: DraftWorkspaceState) => void;
  onPreferencesChange: (next: DraftWorkspacePreferences) => void;
  onSubmitDeck: (commanders?: string[]) => void | Promise<void>;
  onCardHover?: (info: CardHoverInfo | null) => void;
}

export type LocalDeckBuilderController = WorkspaceDeckBuilderControllerBase & {
  capabilities?: { kind: "editable-pool"; suggestions: boolean };
  onAddBasicLand: (name: string) => void;
  onRemoveBasicLand: (name: string) => void;
  onAutoSuggestDeck?: () => void | Promise<void>;
  onAutoSuggestLands?: () => void | Promise<void>;
};

export type WorkspaceDeckBuilderController = LocalDeckBuilderController
  | (WorkspaceDeckBuilderControllerBase & {
      capabilities: { kind: "fixed-pool" };
    });

function isEditableWorkspaceController(
  controller: WorkspaceDeckBuilderController,
): controller is LocalDeckBuilderController {
  return controller.capabilities?.kind !== "fixed-pool";
}

export function LimitedDeckBuilder(props: LimitedDeckBuilderProps = {}) {
  if (props.local) {
    return (
      <WorkspaceDeckBuilder
        controller={props.local}
        submissionError={props.submissionError ?? null}
        showSuggestions={props.showSuggestions ?? true}
        responsiveLayout={props.responsiveLayout ?? "desktop"}
        responsiveHeightMode={props.responsiveHeightMode ?? "viewport"}
      />
    );
  }
  return <ControlledDeckBuilder {...props} />;
}

// Name-count controlled path retained unchanged for Pod/Traditional callers
// that still supply their own deck/land facades and mutation callbacks.
function ControlledDeckBuilder({
  view: viewOverride,
  mainDeck: mainDeckOverride,
  landCounts: landCountsOverride,
  onAddToDeck,
  onRemoveFromDeck,
  onSetLandCount,
  onSubmitDeck,
  submissionError = null,
  showSuggestions = true,
}: LimitedDeckBuilderProps = {}) {
  const { t } = useTranslation("draft");
  const quickView = useDraftStore((s) => s.view);
  const autoSuggestDeck = useDraftStore((s) => s.autoSuggestDeck);
  const autoSuggestLands = useDraftStore((s) => s.autoSuggestLands);
  const quickSubmitDeck = useDraftStore((s) => s.submitDeck);
  const draftCardPreviewMode = usePreferencesStore((s) => s.draftCardPreviewMode);

  const view = viewOverride !== undefined ? viewOverride : quickView;
  const mainDeck = mainDeckOverride ?? EMPTY_MAIN_DECK;
  const landCounts = landCountsOverride ?? EMPTY_LAND_COUNTS;
  const addToDeck: (cardName: string) => void = onAddToDeck ?? (() => {});
  const removeFromDeck: (cardName: string) => void = onRemoveFromDeck ?? (() => {});
  const setLandCount: (landName: string, count: number) => void = onSetLandCount ?? (() => {});
  const submitDeck = onSubmitDeck ?? quickSubmitDeck;

  const [hoveredCard, setHoveredCard] = useState<CardHoverInfo | null>(null);
  const [addableQuery, setAddableQuery] = useState("");
  const [poolFilter, setPoolFilter] = useState<PoolFilter>(EMPTY_POOL_FILTER);
  const [keptInstanceIds, setKeptInstanceIds] = useState<string[] | null>(null);
  const [poolFilterFailed, setPoolFilterFailed] = useState(false);
  const [legacyFilterOptions, setLegacyFilterOptions] =
    useState<PoolFilterOptions | null>(null);
  const [localSubmissionError, setLocalSubmissionError] = useState<string | null>(null);
  const [isSubmitting, setIsSubmitting] = useState(false);

  const pool = useMemo(() => view?.pool ?? [], [view?.pool]);

  // ── CR 903.3 commander designation ────────────────────────────────────
  // Every hook below stays ABOVE the `if (!view) return null` guard.
  const commandersRequired = view?.commanders_required ?? 0;
  const deckFormat = commandersRequired > 0 ? "CommanderDraft" : null;
  const deckFormatConfig = deckFormat ? formatMetadata(deckFormat)?.default_config : undefined;
  const designationRequired = commandersRequired > 0;
  const draftSetCodes = useMemo(() => view?.draft_set_codes ?? [], [view?.draft_set_codes]);
  const fillers = useMemo(
    () => view?.grantable_commander_fillers ?? [],
    [view?.grantable_commander_fillers],
  );

  const remainingPool = useMemo(
    () => computeRemainingPool(pool, mainDeck),
    [pool, mainDeck],
  );

  // #7507 + #7546 review: the ENGINE is the single filtering authority. The
  // display sends the listing, the engine-delivered groups and the typed
  // filter; it renders exactly the returned instance ids. React holds only
  // the presentation state (which chips are pressed, the query text).
  const poolGroups = view?.pool_groups ?? EMPTY_DRAFT_POOL_GROUPS;
  useEffect(() => {
    if (!poolFilterActive(poolFilter)) {
      setKeptInstanceIds(null);
      setPoolFilterFailed(false);
      return;
    }
    let stale = false;
    filterPoolListing(remainingPool, poolFilter)
      .then((ids) => {
        if (stale) return;
        setKeptInstanceIds(ids);
        setPoolFilterFailed(false);
      })
      .catch(() => {
        if (stale) return;
        // Engine unavailable: show the unfiltered listing rather than an
        // empty grid — the display must not interpret the data itself — and
        // SAY so, so the grid cannot silently contradict the active controls
        // (review round 3).
        setKeptInstanceIds(null);
        setPoolFilterFailed(true);
      });
    return () => {
      stale = true;
    };
  }, [remainingPool, poolFilter]);
  // Review round 5: a view that predates the option fields (legacy) gets its
  // control options from the ENGINE's stateless path — never from the lossy
  // exclusive presentation buckets, and never reconstructed here. A v11 view
  // with a non-empty pool always carries non-empty option lists
  // (classification is total), so empty lists + a non-empty pool identify
  // the legacy shape. While the call is pending or failed, the axes stay
  // hidden rather than mis-offered.
  const isLegacyView =
    pool.length > 0 &&
    poolGroups.type_filter_options.length === 0 &&
    poolGroups.color_filter_options.length === 0;
  useEffect(() => {
    if (!isLegacyView) {
      setLegacyFilterOptions(null);
      return;
    }
    // Do not display the prior legacy pool's engine-owned options while this
    // pool's stateless request is pending.
    setLegacyFilterOptions(null);
    let stale = false;
    fetchPoolFilterOptions(pool)
      .then((options) => {
        if (!stale) setLegacyFilterOptions(options);
      })
      .catch(() => {
        if (!stale) setLegacyFilterOptions(null);
      });
    return () => {
      stale = true;
    };
  }, [isLegacyView, pool]);
  const typeChipKinds = isLegacyView
    ? (legacyFilterOptions?.types ?? [])
    : poolGroups.type_filter_options;
  const colorChipKinds = isLegacyView
    ? (legacyFilterOptions?.colors ?? [])
    : poolGroups.color_filter_options;
  const rarityChipKinds = isLegacyView
    ? (legacyFilterOptions?.rarities ?? [])
    : axisKinds(poolGroups.rarity_groups);

  const displayedPool = useMemo(() => {
    if (keptInstanceIds === null) return remainingPool;
    const kept = new Set(keptInstanceIds);
    return remainingPool.filter((card) => kept.has(card.instance_id));
  }, [remainingPool, keptInstanceIds]);

  const deckGroups = useMemo(
    () => groupByName(pool, mainDeck),
    [pool, mainDeck],
  );

  const totalLands = useMemo(
    () => Object.values(landCounts).reduce((sum, n) => sum + n, 0),
    [landCounts],
  );

  // The designation candidates: every card actually IN the deck, including
  // addable-card copies, so a granted CR 903.13e filler can be designated.
  //
  // Merged BY NAME, because the two sources are disjoint but their names are
  // not: `deckGroups` is the drafted pool moved into the main deck, while
  // `landCounts` holds the addable rows -- which is where the CR 903.13e
  // granted filler is offered. A player who drafts a copy of *The Prismatic
  // Piper* AND takes the granted one is named by both. Left unmerged, that
  // renders the candidate twice under one React key and, worse, makes the
  // prune effect's `new Map(...)` keep only the LAST entry, so it believes one
  // copy exists while the deck holds two. Counts SUM: `validate_limited_deck`
  // step 5 is `designated <= in_deck`, a quantity over copies, so the count
  // must be faithful rather than merely non-zero.
  const commanderDeckEntries = useMemo<DeckEntry[]>(() => {
    const byName = new Map<string, number>();
    for (const { card, count } of deckGroups) {
      byName.set(card.name, (byName.get(card.name) ?? 0) + count);
    }
    for (const [name, count] of Object.entries(landCounts)) {
      if (count > 0) byName.set(name, (byName.get(name) ?? 0) + count);
    }
    return [...byName].map(([name, count]) => ({ name, count }));
  }, [deckGroups, landCounts]);

  const {
    commanders,
    eligibilityFailed,
    isCommanderEligible,
    handleSetCommander,
    handleRemoveCommander,
    designationSatisfied,
  } = useCommanderDesignation({
    commandersRequired,
    deckFormat,
    deckEntries: commanderDeckEntries,
    draftSetCodes,
  });
  const commanderDraftCompatibilityActive = deckFormat === "CommanderDraft" && designationRequired;
  const compatibility = useCommanderDraftCompatibility({
    enforceCompatibility: commanderDraftCompatibilityActive,
    selectedFormat: deckFormat,
    main: commanderDeckEntries,
    commanders,
  });
  const { cardDataCache } = useDeckCardData(
    commanderDeckEntries.map((entry) => entry.name),
  );
  const colorDistribution = compatibility.colorDistribution;

  const totalCards = mainDeck.length + totalLands;
  const minDeckSize = view?.min_deck_size ?? 40;
  const addableCards = useMemo(() => {
    const base = view?.addable_cards ?? BASIC_LANDS.map((land) => land.name);
    // CR 903.13e: a filler is NOT `addable_cards` (which means unlimited) — it
    // is capped and commander-conditioned, and BOTH halves are the engine's:
    // `FillerExceedsGrant` fires on `added > max_copies` (commanders-independent,
    // so it already fires today) and `FillerNotUsedAsCommander` fires on
    // `added > designated`. Nothing is capped client-side.
    //
    // The submission channel now carries the designation to
    // `apply_submit_deck`, so `designated` is the player's real count at
    // `validate_limited_deck` and the affordance is live: designate the added
    // copies and they are accepted, leave them undesignated and
    // `FillerNotUsedAsCommander` refuses them. Capping or hiding it
    // client-side instead would be client legality, which the engine owns.
    //
    // Every granted name is offered: a mixed-set draft concedes one per
    // contained set, and the engine caps each on its own name.
    return [...base, ...fillers.map((granted) => granted.card_name)];
  }, [view?.addable_cards, fillers]);
  const filteredAddableCards = useMemo(() => {
    const query = addableQuery.trim().toLowerCase();
    return query
      ? addableCards.filter((name) => name.toLowerCase().includes(query))
      : addableCards;
  }, [addableCards, addableQuery]);
  // CR 903.3: a Commander deck needs a designated commander. The UPPER bound
  // (CR 702.124g, at most two) is the engine's — `MAX_COMMANDER_DESIGNATIONS` in
  // draft-core and `TooManyCommanders`. This surface additionally cannot submit
  // a third: `handleSetCommander`'s pair arm appends only inside a functional
  // updater that re-checks `prev.length === 1` against live state, so
  // concurrent in-flight partner queries cannot stack designations. Every other
  // `setCommanders` writer here only shrinks the list.
  const deckValid = totalCards >= minDeckSize
    && designationSatisfied
    && compatibility.compatible;
  const displayedSubmissionError = submissionError ?? localSubmissionError;

  useEffect(() => {
    setLocalSubmissionError(null);
  }, [mainDeck, landCounts]);

  const handleSubmit = async () => {
    if (isSubmitting) return;
    setLocalSubmissionError(null);
    setIsSubmitting(true);
    try {
      await submitDeck(commandersRequired > 0 ? commanders : []);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setLocalSubmissionError(message || t("limitedDeck.submitFailed"));
    } finally {
      setIsSubmitting(false);
    }
  };

  if (!view) return null;

  return (
    <div className="flex h-full flex-col gap-4">
      <HoverCardPreview
        card={hoveredCard}
        mode={draftCardPreviewMode}
        hoverDelayMs={0}
        mobileLayout="compact"
        onDismiss={() => setHoveredCard(null)}
      />
      <DeckStatus spells={mainDeck.length} lands={totalLands} min={minDeckSize} />

      <div className="flex min-h-0 flex-1 gap-6">
        {/* Left column: Pool + Main Deck */}
        <div className="flex min-w-0 flex-[7] flex-col gap-6 overflow-y-auto">
          {/* Pool section */}
          <section>
            <h3 className="mb-3 text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
              {t("limitedDeck.poolHeading", { count: remainingPool.length })}
            </h3>
            <div className="mb-3 flex flex-col gap-2">
              <input
                type="search"
                value={poolFilter.query}
                onChange={(event) =>
                  setPoolFilter((prev) => ({ ...prev, query: event.target.value }))
                }
                placeholder={t("limitedDeck.searchPool")}
                aria-label={t("limitedDeck.searchPool")}
                className="w-full rounded-lg border border-white/10 bg-black/30 px-3 py-2 text-sm text-white outline-none placeholder:text-white/25 focus:border-emerald-400/50"
              />
              <PoolFilterChips
                kinds={typeChipKinds}
                selected={poolFilter.types}
                onToggle={(kind) =>
                  setPoolFilter((prev) => ({ ...prev, types: toggleKind(prev.types, kind) }))
                }
              />
              <PoolFilterChips
                kinds={colorChipKinds}
                selected={poolFilter.colors}
                onToggle={(kind) =>
                  setPoolFilter((prev) => ({ ...prev, colors: toggleKind(prev.colors, kind) }))
                }
              />
              <PoolFilterChips
                kinds={rarityChipKinds}
                selected={poolFilter.rarities}
                onToggle={(kind) =>
                  setPoolFilter((prev) => ({
                    ...prev,
                    rarities: toggleKind(prev.rarities, kind),
                  }))
                }
              />
              {poolFilterFailed && (
                <p role="alert" className="text-xs text-amber-300/80">
                  {t("limitedDeck.filterUnavailable")}
                </p>
              )}
            </div>
            <div className="grid grid-cols-3 gap-2 sm:grid-cols-4 md:grid-cols-5 lg:grid-cols-6 xl:grid-cols-7">
              <AnimatePresence mode="popLayout" initial={false}>
                {displayedPool.map((card) => (
                  <motion.div key={card.instance_id} {...CARD_MOTION}>
                    <CardTile
                      card={card}
                      dimmed
                      onClick={() => addToDeck(card.name)}
                      onHover={setHoveredCard}
                    />
                  </motion.div>
                ))}
              </AnimatePresence>
            </div>
            {remainingPool.length === 0 ? (
              <p className="py-4 text-sm text-white/30">{t("limitedDeck.allAdded")}</p>
            ) : (
              displayedPool.length === 0 &&
              poolFilterActive(poolFilter) && (
                <p className="py-4 text-sm text-white/30">
                  {t("limitedDeck.noFilterMatches")}
                </p>
              )
            )}
          </section>

          {/* Main deck section */}
          <section>
            <h3 className="mb-3 text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
              {t("limitedDeck.mainDeck")}
            </h3>
            <div className="grid grid-cols-3 gap-2 sm:grid-cols-4 md:grid-cols-5 lg:grid-cols-6 xl:grid-cols-7">
              <AnimatePresence mode="popLayout" initial={false}>
                {deckGroups.map(({ card, count }) => (
                  <motion.div key={card.instance_id} {...CARD_MOTION}>
                    <CardTile
                      card={card}
                      count={count}
                      onClick={() => removeFromDeck(card.name)}
                      onHover={setHoveredCard}
                    />
                  </motion.div>
                ))}
              </AnimatePresence>
            </div>
            {mainDeck.length === 0 && (
              <p className="py-4 text-sm text-white/30">
                {t("limitedDeck.emptyDeckHint")}
              </p>
            )}
          </section>
        </div>

        {/* Right column: Lands, Mana Curve, Actions */}
        <div className="flex min-w-[220px] flex-[3] flex-col gap-6 overflow-y-auto">
          {/* Land counts */}
          <section>
            <div className="mb-3 flex items-center justify-between">
              <h3 className="text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
                {t("limitedDeck.addableCards")}
              </h3>
              {showSuggestions && (
                <button
                  type="button"
                  onClick={autoSuggestLands}
                  className={menuButtonClass({ tone: "neutral", size: "xs", ghost: true })}
                >
                  {t("limitedDeck.autoLands")}
                </button>
              )}
            </div>
            <input
              type="search"
              value={addableQuery}
              onChange={(event) => setAddableQuery(event.target.value)}
              placeholder={t("limitedDeck.searchAddableCards")}
              aria-label={t("limitedDeck.searchAddableCards")}
              className="mb-3 w-full rounded-lg border border-white/10 bg-black/30 px-3 py-2 text-sm text-white outline-none placeholder:text-white/25 focus:border-emerald-400/50"
            />
            <div className="flex flex-col gap-2">
              {filteredAddableCards.map((name) => (
                <LandRow
                  key={name}
                  name={name}
                  colorClass={LAND_COLOR_CLASSES[name] ?? "bg-cyan-300"}
                  count={landCounts[name] ?? 0}
                  onDecrement={() => setLandCount(name, (landCounts[name] ?? 0) - 1)}
                  onIncrement={() => setLandCount(name, (landCounts[name] ?? 0) + 1)}
                />
              ))}
            </div>
          </section>

          {/* CR 903.3 commander designation */}
          {designationRequired && deckFormatConfig && (
            <section className="flex flex-col gap-2">
              {eligibilityFailed && (
                <p role="alert" className="text-xs text-amber-300/80">
                  {t("limitedDeck.commanderUnavailable")}
                </p>
              )}
              {fillers.map((granted) => (
                <p key={granted.card_name} className="text-xs text-white/45">
                  {t("limitedDeck.grantedFiller", {
                    name: granted.card_name,
                    maximum: granted.max_copies,
                  })}
                </p>
              ))}
              {/* CR 903.5a: `commanderDeckEntries` still contains every designated
                  card — the designation is a label on a deck card, not an extra
                  card beside the deck — so the panel is told not to add the
                  commanders back. */}
              <CommanderPanel
                commanders={commanders}
                deck={commanderDeckEntries}
                deckComposition="commanders-inside"
                cardDataCache={cardDataCache}
                deckSizeRule={deckFormatConfig.deck_size}
                isCommanderEligible={isCommanderEligible}
                onSetCommander={handleSetCommander}
                onRemoveCommander={handleRemoveCommander}
                onCardHover={setHoveredCard}
                formatValidationReasons={compatibility.reasons}
              />
              {compatibility.unavailable && (
                <p role="alert" className="text-xs text-amber-300/80">
                  {t("limitedDeck.compatibilityUnavailable")}
                </p>
              )}
              {!designationSatisfied && (
                <p className="text-xs text-white/55">{t("limitedDeck.commanderRequired")}</p>
              )}
            </section>
          )}

          {/* Mana curve */}
          <section>
            <ManaCurve pool={pool} cards={mainDeck} colorDistribution={colorDistribution} />
          </section>

          {/* Actions */}
          <section className="flex flex-col gap-3">
            {showSuggestions && (
              <button
                type="button"
                onClick={autoSuggestDeck}
                className={menuButtonClass({ tone: "neutral", size: "sm", className: "w-full" })}
              >
                {t("limitedDeck.suggestDeck")}
              </button>
            )}

            <button
              type="button"
              onClick={() => void handleSubmit()}
              disabled={!deckValid || isSubmitting}
              className={menuButtonClass({
                tone: "emerald",
                size: "md",
                disabled: !deckValid || isSubmitting,
                className: "w-full",
              })}
            >
              {t("limitedDeck.submitDeck")}
            </button>
            {displayedSubmissionError && (
              <p
                role="alert"
                className="rounded-lg border border-red-400/40 bg-red-500/10 px-3 py-2 text-sm text-red-100"
              >
                <span className="font-medium">{t("limitedDeck.validationTitle")}: </span>
                {displayedSubmissionError}
              </p>
            )}
          </section>
        </div>
      </div>
    </div>
  );
}

// ── Workspace-backed local deckbuilder ──────────────────────────────────

function WorkspaceDeckBuilder({
  controller,
  submissionError,
  showSuggestions,
  responsiveLayout,
  responsiveHeightMode,
}: {
  controller: WorkspaceDeckBuilderController;
  submissionError: string | null;
  showSuggestions: boolean;
  responsiveLayout: ResponsiveDraftLayout;
  responsiveHeightMode: "viewport" | "container";
}) {
  const { t } = useTranslation("draft");
  const { t: tDeckBuilder } = useTranslation("deck-builder");
  const draftCardPreviewMode = usePreferencesStore((s) => s.draftCardPreviewMode);
  const {
    view,
    workspace,
    preferences,
    interactionLocked,
    onWorkspaceChange,
    onPreferencesChange,
    onSubmitDeck,
    onCardHover,
  } = controller;
  const editableController = isEditableWorkspaceController(controller) ? controller : null;
  const poolChangesEnabled = editableController !== null;
  const suggestionsEnabled = showSuggestions && editableController !== null
    && (editableController.capabilities?.suggestions ?? true);

  const [hoveredCard, setHoveredCard] = useState<CardHoverInfo | null>(null);
  const [localSubmissionError, setLocalSubmissionError] = useState<string | null>(null);
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [snowCovered, setSnowCovered] = useState(false);
  const [requestedLands, setRequestedLands] = useState<Record<string, number>>({});

  const pool = view.pool;
  // Deck status, curve, land controls and validation all read the one
  // authoritative instance workspace — never a separate name-count facade.
  const deckCards = useMemo(
    () => pool.filter((card) => workspace.placements[card.instance_id]?.zone === "deck"),
    [pool, workspace],
  );
  const deckVirtualBasics = useMemo(
    () => workspace.virtualBasics.filter(
      (card) => workspace.placements[card.instanceId]?.zone === "deck",
    ),
    [workspace],
  );
  const spellNames = useMemo(() => [
    ...deckCards.filter((card) => !/\bland\b/i.test(card.type_line)).map((card) => card.name),
    ...deckVirtualBasics.filter((card) => !BASIC_LAND_NAMES.has(card.name)).map((card) => card.name),
  ], [deckCards, deckVirtualBasics]);
  const deckNames = useMemo(() => projectDeckNames(workspace, pool), [workspace, pool]);
  const commanderDeckEntries = useMemo(() => countProjectedNames(deckNames), [deckNames]);
  const commandersRequired = view.commanders_required;
  const deckFormat = commandersRequired > 0 ? "CommanderDraft" : null;
  const deckFormatConfig = deckFormat ? formatMetadata(deckFormat)?.default_config : undefined;
  const designationRequired = commandersRequired > 0;
  const draftSetCodes = useMemo(() => view.draft_set_codes ?? [], [view.draft_set_codes]);
  const fillers = useMemo(
    () => view.grantable_commander_fillers ?? [],
    [view.grantable_commander_fillers],
  );
  const {
    commanders,
    eligibilityFailed,
    isCommanderEligible,
    handleSetCommander,
    handleRemoveCommander,
    designationSatisfied,
  } = useCommanderDesignation({
    commandersRequired,
    deckFormat,
    deckEntries: commanderDeckEntries,
    draftSetCodes,
  });
  const commanderDraftCompatibilityActive = deckFormat === "CommanderDraft";
  const compatibility = useCommanderDraftCompatibility({
    enforceCompatibility: commanderDraftCompatibilityActive,
    selectedFormat: deckFormat,
    main: commanderDeckEntries,
    commanders,
  });
  const { cardDataCache } = useDeckCardData(
    commanderDeckEntries.map((entry) => entry.name),
  );
  const colorDistribution = compatibility.colorDistribution;
  const totalLands = useMemo(
    () => deckCards.filter((card) => /\bland\b/i.test(card.type_line)).length
      + deckVirtualBasics.filter((card) => BASIC_LAND_NAMES.has(card.name)).length,
    [deckCards, deckVirtualBasics],
  );
  const minDeckSize = view.min_deck_size ?? 40;
  const deckValid = deckNames.length >= minDeckSize
    && designationSatisfied
    && compatibility.compatible;
  const phoneLayout = responsiveLayout === "phone-portrait" || responsiveLayout === "phone-landscape";
  const tabletLayout = responsiveLayout === "tablet-portrait" || responsiveLayout === "tablet-landscape";
  const tabletLandscapeLayout = responsiveLayout === "tablet-landscape";

  const displayedSubmissionError = submissionError ?? localSubmissionError;
  const dragController = useDraftWorkspaceDrag({
    enabled: !interactionLocked,
    readPickInteraction: () => DECKBUILDING_INTERACTION,
    subscribePickInteraction: () => () => {},
    onDrop: (request) => ({
      requestToken: request.requestToken,
      interactionGeneration: request.interactionGeneration,
      outcome: Promise.resolve({ status: "rejected", reason: "invalid-request" }),
    }),
    resolveCollapsedSideboardColumn: (instanceId) => Math.min(
      preferences.sideboard.columnCount - 1,
      Math.max(0, workspace.placements[instanceId]?.column ?? 0),
    ),
  });

  useEffect(() => {
    setLocalSubmissionError(null);
  }, [workspace]);

  const handleHover = (info: CardHoverInfo | null) => {
    setHoveredCard(info);
    onCardHover?.(info);
  };

  const handleSubmit = async () => {
    if (isSubmitting) return;
    setLocalSubmissionError(null);
    setIsSubmitting(true);
    try {
      await onSubmitDeck(commandersRequired > 0 ? commanders : []);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setLocalSubmissionError(message || t("limitedDeck.submitFailed"));
    } finally {
      setIsSubmitting(false);
    }
  };

  const requestedLandTotal = Object.values(requestedLands).reduce((sum, count) => sum + count, 0);
  const handleAddLands = (close: () => void) => {
    if (editableController === null || requestedLandTotal === 0) return;
    for (const land of BASIC_LANDS) {
      const count = requestedLands[land.name] ?? 0;
      const name = snowCovered ? `Snow-Covered ${land.name}` : land.name;
      for (let index = 0; index < count; index += 1) {
        editableController.onAddBasicLand(name);
      }
    }
    setRequestedLands({});
    close();
  };

  const landPicker = (label: string, compactTouchTarget = false) => poolChangesEnabled ? (
    <PopoverMenu
      ariaLabel={t("limitedDeck.addLands")}
      variant="dialog"
      menuWidthPx={288}
      renderTrigger={({ ref, open, toggle }) => (
        <button
          ref={ref}
          type="button"
          aria-expanded={open}
          aria-haspopup="dialog"
          onClick={toggle}
          className={menuButtonClass({
            tone: "neutral",
            size: "xs",
            className: compactTouchTarget ? "min-h-11" : undefined,
          })}
        >
          {label}
        </button>
      )}
    >
      {(close) => (
        <div className="p-4">
          <div className="flex flex-col gap-2">
            {BASIC_LANDS.map((land) => (
              <LandRow
                key={land.name}
                name={land.name}
                colorClass={land.colorClass}
                count={requestedLands[land.name] ?? 0}
                onDecrement={() => setRequestedLands((counts) => ({
                  ...counts,
                  [land.name]: Math.max(0, (counts[land.name] ?? 0) - 1),
                }))}
                onIncrement={() => setRequestedLands((counts) => ({
                  ...counts,
                  [land.name]: (counts[land.name] ?? 0) + 1,
                }))}
              />
            ))}
          </div>
          <div className="mt-4 flex items-center justify-between gap-3">
            <label className="flex min-h-9 items-center gap-2 text-sm text-white/70">
              <input
                type="checkbox"
                checked={snowCovered}
                onChange={(event) => setSnowCovered(event.target.checked)}
                className="accent-jade"
              />
              {t("limitedDeck.snowCovered")}
            </label>
            {suggestionsEnabled && editableController?.onAutoSuggestLands && (
              <button
                type="button"
                onClick={() => void editableController.onAutoSuggestLands?.()}
                className={menuButtonClass({
                  tone: "neutral",
                  size: "xs",
                  className: compactTouchTarget ? "min-h-11" : undefined,
                })}
              >
                {t("limitedDeck.autoLands")}
              </button>
            )}
          </div>
          <button
            type="button"
            disabled={requestedLandTotal === 0}
            onClick={() => handleAddLands(close)}
            className={menuButtonClass({
              tone: "emerald",
              size: "sm",
              disabled: requestedLandTotal === 0,
              className: "mt-3 w-full",
            })}
          >
            {t("limitedDeck.addLands")}
          </button>
        </div>
      )}
    </PopoverMenu>
  ) : null;

  const landControls = poolChangesEnabled ? (
    landPicker(t("limitedDeck.addLands"))
  ) : null;

  const compactLandControls = landPicker(
    (phoneLayout || tabletLayout) ? t("limitedDeck.addLands") : t("limitedDeck.lands"),
    phoneLayout || tabletLayout,
  );
  const suggestDeckAvailable = suggestionsEnabled
    && editableController?.onAutoSuggestDeck !== undefined
    && !interactionLocked;
  const submissionAlert = displayedSubmissionError && (
    <p
      role="alert"
      className="rounded-lg border border-red-400/40 bg-red-500/10 px-3 py-2 text-sm text-red-100"
    >
      <span className="font-medium">{t("limitedDeck.validationTitle")}: </span>
      {displayedSubmissionError}
    </p>
  );
  const workspaceBoard = (
    <DraftWorkspace
      pool={pool}
      poolGroups={view.pool_groups}
      workspace={workspace}
      preferences={preferences}
      interactionLocked={interactionLocked}
      dragController={dragController}
      deckControls={landControls}
      compactDeckControls={compactLandControls}
      responsiveLayout={responsiveLayout}
      responsiveContext="builder"
      onWorkspaceChange={onWorkspaceChange}
      onPreferencesChange={onPreferencesChange}
      onCardHover={handleHover}
    />
  );
  const commanderControls = designationRequired && deckFormatConfig ? (
    <section className="flex flex-col gap-2">
      {eligibilityFailed && (
        <p role="alert" className="text-xs text-amber-300/80">
          {t("limitedDeck.commanderUnavailable")}
        </p>
      )}
      {fillers.map((granted) => (
        <div key={granted.card_name} className="flex flex-col gap-1">
          <p className="text-xs text-white/45">
            {t("limitedDeck.grantedFiller", {
              name: granted.card_name,
              maximum: granted.max_copies,
            })}
          </p>
          {editableController && (
            <LandRow
              name={granted.card_name}
              colorClass="bg-cyan-300"
              count={deckVirtualBasics.filter((card) => card.name === granted.card_name).length}
              onDecrement={() => editableController.onRemoveBasicLand(granted.card_name)}
              onIncrement={() => editableController.onAddBasicLand(granted.card_name)}
            />
          )}
        </div>
      ))}
      <CommanderPanel
        commanders={commanders}
        deck={commanderDeckEntries}
        deckComposition="commanders-inside"
        cardDataCache={cardDataCache}
        deckSizeRule={deckFormatConfig.deck_size}
        isCommanderEligible={isCommanderEligible}
        onSetCommander={handleSetCommander}
        onRemoveCommander={handleRemoveCommander}
        onCardHover={handleHover}
        formatValidationReasons={compatibility.reasons}
      />
      {compatibility.unavailable && (
        <p role="alert" className="text-xs text-amber-300/80">
          {t("limitedDeck.compatibilityUnavailable")}
        </p>
      )}
      {!designationSatisfied && (
        <p className="text-xs text-white/55">{t("limitedDeck.commanderRequired")}</p>
      )}
    </section>
  ) : null;
  const compactCommanderControls = commanderControls ? (
    <PopoverMenu
      ariaLabel={tDeckBuilder("commanderPanel.heading")}
      variant="dialog"
      menuWidthPx={320}
      renderTrigger={({ ref, open, toggle }) => (
        <button
          ref={ref}
          type="button"
          aria-expanded={open}
          aria-haspopup="dialog"
          onClick={toggle}
          className={menuButtonClass({
            tone: "neutral",
            size: "sm",
            className: "min-h-11 shrink-0",
          })}
        >
          {tDeckBuilder("commanderPanel.heading")}
        </button>
      )}
    >
      {() => <div className="overflow-y-auto p-3">{commanderControls}</div>}
    </PopoverMenu>
  ) : null;

  return (
    <div
      data-responsive-builder-layout={responsiveLayout}
      className={phoneLayout
        ? "flex h-[calc(100dvh_-_4rem)] min-h-0 flex-col gap-4 overflow-hidden pb-[67px]"
        : tabletLayout
          ? responsiveHeightMode === "container"
            ? "flex h-full min-h-0 flex-col gap-1 overflow-hidden"
            : "flex h-[calc(100dvh_-_4rem)] min-h-0 flex-col gap-1 overflow-hidden"
          : "flex h-full flex-col gap-4"}
    >
      <HoverCardPreview
        card={hoveredCard}
        mode={draftCardPreviewMode}
        hoverDelayMs={0}
        mobileLayout="compact"
        onDismiss={() => handleHover(null)}
      />
      {!phoneLayout && (
        <div className="flex items-center gap-2">
          <div className="min-w-0 flex-1">
            <DeckStatus spells={spellNames.length} lands={totalLands} min={minDeckSize} />
          </div>
          {tabletLayout && compactCommanderControls}
        </div>
      )}

      {tabletLayout ? (
        <>
          {/* The board gets every remaining pixel; the dock never scrolls away. */}
          <div
            data-tablet-builder-board
            data-tablet-landscape-builder-board={tabletLandscapeLayout ? "" : undefined}
            className="flex min-h-0 w-full min-w-0 flex-1 flex-col overflow-hidden"
          >
            {workspaceBoard}
          </div>
          <aside
            data-tablet-builder-dock
            data-tablet-landscape-builder-dock={tabletLandscapeLayout ? "" : undefined}
            className="shrink-0 border-t border-white/10 bg-slate-950/92 px-3 py-2 shadow-[0_-12px_30px_rgba(0,0,0,0.32)]"
          >
            {tabletLandscapeLayout ? (
              <>
                <div
                  data-tablet-landscape-builder-row
                  className="grid grid-cols-[minmax(0,45fr)_minmax(0,15fr)_minmax(0,20fr)_minmax(0,20fr)] gap-2"
                >
                  <div data-tablet-landscape-builder-slot="curve" className="min-w-0">
                    <ManaCurve
                      pool={pool}
                      cards={spellNames}
                      colorDistribution={colorDistribution}
                      presentation="compact"
                    />
                  </div>
                  <div
                    data-tablet-landscape-builder-slot="average"
                    className="flex min-w-0 items-center justify-center"
                  >
                    <AverageManaCost cards={deckCards} />
                  </div>
                  <div data-tablet-landscape-builder-slot="suggest" className="min-w-0">
                    <button
                      type="button"
                      disabled={!suggestDeckAvailable}
                      onClick={suggestDeckAvailable
                        ? () => void editableController?.onAutoSuggestDeck?.()
                        : undefined}
                      className={menuButtonClass({
                        tone: "neutral",
                        size: "sm",
                        disabled: !suggestDeckAvailable,
                        className: "w-full",
                      })}
                    >
                      {t("limitedDeck.suggestDeck")}
                    </button>
                  </div>
                  <div data-tablet-landscape-builder-slot="submit" className="min-w-0">
                    <button
                      type="button"
                      onClick={() => void handleSubmit()}
                      disabled={!deckValid || isSubmitting}
                      className={menuButtonClass({
                        tone: "emerald",
                        size: "sm",
                        disabled: !deckValid || isSubmitting,
                        className: "w-full",
                      })}
                    >
                      {t("limitedDeck.submitDeck")}
                    </button>
                  </div>
                </div>
                {submissionAlert && <div className="mt-2">{submissionAlert}</div>}
              </>
            ) : (
              <>
                <div data-tablet-builder-summary className="grid grid-cols-4 gap-3 overflow-hidden">
                  <section className="col-span-3 min-w-0">
                    <ManaCurve pool={pool} cards={spellNames} colorDistribution={colorDistribution} />
                  </section>
                  <section className="col-span-1 flex min-w-0 items-center justify-center">
                    <AverageManaCost cards={deckCards} />
                  </section>
                </div>
                {submissionAlert && <div className="mt-2">{submissionAlert}</div>}
                <div data-tablet-builder-actions className="mt-2 grid grid-cols-2 gap-2">
                  <button
                    type="button"
                    disabled={!suggestDeckAvailable}
                    onClick={suggestDeckAvailable
                      ? () => void editableController?.onAutoSuggestDeck?.()
                      : undefined}
                    className={menuButtonClass({
                      tone: "neutral",
                      size: "sm",
                      disabled: !suggestDeckAvailable,
                      className: "w-full",
                    })}
                  >
                    {t("limitedDeck.suggestDeck")}
                  </button>
                  <button
                    type="button"
                    onClick={() => void handleSubmit()}
                    disabled={!deckValid || isSubmitting}
                    className={menuButtonClass({
                      tone: "emerald",
                      size: "md",
                      disabled: !deckValid || isSubmitting,
                      className: "w-full",
                    })}
                  >
                    {t("limitedDeck.submitDeck")}
                  </button>
                </div>
              </>
            )}
          </aside>
        </>
      ) : (
        <div
          data-responsive-workspace-layout
          className="flex min-h-0 flex-1 flex-col gap-6 xl:flex-row"
        >
          {/* Primary board: exact-instance deck/sideboard placement authority. */}
          <div className="flex min-h-0 w-full min-w-0 flex-[7] flex-col overflow-hidden">
            {workspaceBoard}
          </div>

          {phoneLayout && (
            <aside
              data-mobile-builder-analysis
              className="shrink-0 space-y-3 border-t border-white/10 pt-3"
            >
              <ManaCurve
                pool={pool}
                cards={spellNames}
                colorDistribution={colorDistribution}
                presentation="compact"
              />
            </aside>
          )}

          {!phoneLayout && (
          <div
            data-desktop-builder-analysis
            className="flex w-full min-w-[220px] flex-[1.25] flex-col gap-6 overflow-y-auto xl:w-auto"
          >
            {commanderControls}
            {suggestionsEnabled && editableController?.onAutoSuggestDeck && (
              <section>
                <button
                  type="button"
                  onClick={() => void editableController.onAutoSuggestDeck?.()}
                  className={menuButtonClass({ tone: "neutral", size: "sm", className: "w-full" })}
                >
                  {t("limitedDeck.suggestDeck")}
                </button>
              </section>
            )}

            <section>
              <ManaCurve pool={pool} cards={spellNames} colorDistribution={colorDistribution} />
            </section>

            <section className="flex flex-col gap-4">
              <DeckStatistics
                cards={deckCards}
                virtualCardNames={deckVirtualBasics.map((card) => card.name)}
              />
              <button
                type="button"
                onClick={() => void handleSubmit()}
                disabled={!deckValid || isSubmitting}
                className={menuButtonClass({
                  tone: "emerald",
                  size: "md",
                  disabled: !deckValid || isSubmitting,
                  className: "w-full",
                })}
              >
                {t("limitedDeck.submitDeck")}
              </button>
              {submissionAlert}
            </section>
          </div>
          )}
        </div>
      )}
      {phoneLayout && (
        <div
          data-mobile-builder-submit-dock
          className="fixed inset-x-[9px] bottom-0 z-40 flex min-h-[calc(59px_+_env(safe-area-inset-bottom))] items-center gap-2.5 border-t border-jade/30 bg-slate-950 px-2.5 py-[7px] shadow-[0_-12px_30px_rgba(0,0,0,0.42)]"
        >
          <div className="flex min-w-0 flex-1 items-baseline gap-2">
            <strong className="shrink-0 text-xs text-fg">{t("workspace.count.deck", { count: deckNames.length })}</strong>
            <span data-mobile-deck-remaining className="truncate text-[9px] text-fg-muted">
              {deckValid
                ? t("limitedDeck.readyToSubmit")
                : deckNames.length < minDeckSize
                  ? t("limitedDeck.moreNeeded", { count: minDeckSize - deckNames.length })
                  : !designationSatisfied
                    ? t("limitedDeck.commanderRequired")
                    : compatibility.reasons[0]
                      ?? (compatibility.pending
                        ? t("limitedDeck.compatibilityPending")
                        : compatibility.unavailable
                          ? t("limitedDeck.compatibilityUnavailable")
                          : t("limitedDeck.commanderRequired"))
              }
            </span>
          </div>
          {compactCommanderControls}
          <button
            type="button"
            onClick={() => void handleSubmit()}
            disabled={!deckValid || isSubmitting}
            className={menuButtonClass({
              tone: "emerald",
              size: "md",
              disabled: !deckValid || isSubmitting,
            })}
          >
            {t("limitedDeck.submitDeck")}
          </button>
        </div>
      )}
    </div>
  );
}

// ── Deck status bar ─────────────────────────────────────────────────────

function DeckStatus({ spells, lands, min }: { spells: number; lands: number; min: number }) {
  const { t } = useTranslation("draft");
  const total = spells + lands;
  const valid = total >= min;
  const remaining = Math.max(0, min - total);
  const pct = Math.min(100, (total / min) * 100);

  return (
    <div data-deck-status className="rounded-[16px] border border-white/10 bg-black/18 px-4 py-3 backdrop-blur-md">
      <div className="flex items-baseline justify-between">
        <span className="text-sm font-medium text-white">
          {total} <span className="text-white/40">{t("limitedDeck.cardCount", { min })}</span>
        </span>
        <span className="text-xs text-white/45">
          {t("limitedDeck.spellCount", { count: spells })} · {t("limitedDeck.landCount", { count: lands })}
          {valid ? (
            <span className="ml-2 font-medium text-emerald-300">{t("limitedDeck.readyToSubmit")}</span>
          ) : (
            <span className="ml-2 text-white/55">{t("limitedDeck.moreNeeded", { count: remaining })}</span>
          )}
        </span>
      </div>
      <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-white/8">
        <div
          className={`h-full rounded-full transition-all duration-300 ${valid ? "bg-emerald-400/80" : "bg-white/30"}`}
          style={{ width: `${pct}%` }}
        />
      </div>
    </div>
  );
}

// ── Pool filter chips (#7507) ───────────────────────────────────────────

/**
 * One filter axis as toggle chips. The offered kinds are exactly the groups
 * the engine delivered for this pool (`axisKinds`), in engine order; labels
 * come from the shared engine-kind label map. An axis with fewer than two
 * groups offers no narrowing and renders nothing.
 */
function PoolFilterChips({
  kinds,
  selected,
  onToggle,
}: {
  kinds: DraftPoolGroupKind[];
  selected: DraftPoolGroupKind[];
  onToggle: (kind: DraftPoolGroupKind) => void;
}) {
  const { t } = useTranslation("draft");
  if (kinds.length < 2) return null;
  return (
    <div className="flex flex-wrap gap-1">
      {kinds.map((kind) => (
        <button
          key={kind}
          type="button"
          aria-pressed={selected.includes(kind)}
          onClick={() => onToggle(kind)}
          // 44pt coarse-pointer floor in BOTH dimensions (index.css),
          // relaxed only for fine pointers — a wide touch device keeps the
          // full target (review round 4).
          className={`min-h-[44px] min-w-[44px] rounded-full px-3 py-1 text-xs transition-colors pointer-fine:min-h-0 pointer-fine:min-w-0 pointer-fine:px-2.5 ${
            selected.includes(kind)
              ? "bg-white/10 text-white"
              : "text-white/40 hover:bg-white/5 hover:text-white/70"
          }`}
        >
          {t(POOL_GROUP_LABEL_KEYS[kind])}
        </button>
      ))}
    </div>
  );
}
