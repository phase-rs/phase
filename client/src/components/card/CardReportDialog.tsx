import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import type { GameObject, PlayerId, Zone } from "../../adapter/types.ts";
import { useCardHover } from "../../hooks/useCardHover.ts";
import { useCardImage } from "../../hooks/useCardImage.ts";
import { type CardReportContext, useCardReport } from "../../hooks/useCardReport.ts";
import { useCardParseDetails } from "../../hooks/useEngineCardData.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { getSeatColor, useSeatColor } from "../../hooks/useSeatColor.ts";
import { cardImageLookup, tokenFiltersForObject } from "../../services/cardImageLookup.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getPlayerDisplayName } from "../../stores/multiplayerStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { isObjectReportableToViewer } from "../../viewmodel/gameStateView.ts";
import { ModalPanelShell } from "../ui/ModalPanelShell.tsx";

// Most-relevant-first zone ordering. `Library` is intentionally absent — the
// visibility helper hides library cards, and top-of-library reveals are out of
// scope (plan §5).
const ZONE_ORDER: Zone[] = ["Stack", "Battlefield", "Hand", "Graveyard", "Exile", "Command"];

/** A representative object plus how many copies it stands in for. Duplicate
 *  copies sharing the report dedup key (`oracle_id || name`) — e.g. several
 *  instances of the same token — collapse into one entry. */
interface ZoneEntry {
  obj: GameObject;
  count: number;
}

interface ZoneGroup {
  zone: Zone;
  entries: ZoneEntry[];
}

/** The seat whose ownership the zone chip labels: controller for the shared
 *  public zones, otherwise the owner. Display-only — visibility is already gated
 *  by `isObjectReportableToViewer`. */
function labelSeatForZone(obj: GameObject): PlayerId {
  return obj.zone === "Battlefield" || obj.zone === "Stack" ? obj.controller : obj.owner;
}

interface ReportIdentity {
  oracleId: string;
  faceName: string;
  /** Displayed + reported name, and the key used for parse lookup. */
  name: string;
  isEmblem: boolean;
}

/**
 * The card identity a row displays, reports, and dedups on. For normal objects
 * that's the printed card; for emblems it's the SOURCE card. CR 114.5: an emblem
 * isn't represented by a card and the engine names every emblem literally
 * "Emblem" (`create_emblem.rs`), so keying on `obj.name` would collapse all
 * emblems into one meaningless row and fail the parse lookup. Instead we key on
 * `emblem_source` — the planeswalker whose ultimate made it — which carries a
 * real name (and usually a real `oracle_id`) that resolves for parse coverage.
 * `oracleId` stays empty for emblems so their reports don't merge with reports
 * of the source card itself.
 */
function reportIdentity(obj: GameObject): ReportIdentity {
  if (obj.is_emblem) {
    const source = obj.emblem_source;
    return { oracleId: "", faceName: "", name: source?.name ?? obj.name, isEmblem: true };
  }
  return {
    oracleId: obj.printed_ref?.oracle_id ?? "",
    faceName: obj.printed_ref?.face_name ?? "",
    name: obj.name,
    isEmblem: false,
  };
}

/** Dedup / ✓-state key — identical to `useCardReport`'s (`oracleId || name`). */
function reportKey(identity: ReportIdentity): string {
  return identity.oracleId || identity.name;
}

/**
 * Player-facing "Report a card problem" picker. Lists the current game's cards
 * grouped by zone, each row carrying a live parse-coverage fraction and a
 * one-click report action, so the player picks the offending card from a stable
 * list instead of chasing the hover preview. Reads engine-provided state only
 * (objects + reveal sets + parse details) and formats it — no game logic.
 */
export function CardReportDialog() {
  const { t } = useTranslation("game");
  const open = useUiStore((s) => s.cardReportDialogOpen);
  const close = useUiStore((s) => s.closeCardReportDialog);
  const gameState = useGameStore((s) => s.gameState);
  const viewerId = usePlayerId();
  const seatOrder = gameState?.seat_order;
  const [search, setSearch] = useState("");
  // null = show every seat's cards; otherwise restrict to one seat's cards.
  const [playerFilter, setPlayerFilter] = useState<PlayerId | null>(null);
  // null = show every zone's cards; otherwise restrict to one zone.
  const [zoneFilter, setZoneFilter] = useState<Zone | null>(null);

  const { groups, seats, zones } = useMemo(() => {
    if (!gameState) {
      return { groups: [] as ZoneGroup[], seats: [] as PlayerId[], zones: [] as Zone[] };
    }
    const query = search.trim().toLowerCase();
    const seatSet = new Set<PlayerId>();
    const zoneSet = new Set<Zone>();
    const byZone = new Map<Zone, GameObject[]>();
    for (const obj of Object.values(gameState.objects)) {
      if (!ZONE_ORDER.includes(obj.zone)) continue; // excludes Library
      if (!isObjectReportableToViewer(gameState, obj, viewerId)) continue;
      // Basic lands are vanilla (CR 305.6) — nothing to parse or misbehave — and
      // usually the most numerous cards on the board, so they're pure noise in a
      // problem report. The `Basic` supertype marks every basic land generally
      // (incl. Snow-Covered), so this never name-matches the five basics.
      if (obj.card_types.supertypes.includes("Basic")) continue;
      // Drop objects with nothing to key a report on. Kept: printed cards, tokens
      // (reported with an empty oracle_id), and emblems (reported under their
      // source card via `reportIdentity`).
      if (!obj.printed_ref && obj.display_source !== "Token" && !obj.is_emblem) continue;
      // Record the owning/controlling seat and the zone for the filter chips
      // BEFORE applying the active player/zone/search filters, so selecting one
      // filter never removes the chips for the others.
      const seat = labelSeatForZone(obj);
      seatSet.add(seat);
      zoneSet.add(obj.zone);
      if (playerFilter != null && seat !== playerFilter) continue;
      if (zoneFilter != null && obj.zone !== zoneFilter) continue;
      if (query && !reportIdentity(obj).name.toLowerCase().includes(query)) continue;
      const list = byZone.get(obj.zone) ?? [];
      list.push(obj);
      byZone.set(obj.zone, list);
    }
    const groups = ZONE_ORDER.flatMap((zone) => {
      const list = byZone.get(zone);
      if (!list || list.length === 0) return [];
      list.sort((a, b) => reportIdentity(a).name.localeCompare(reportIdentity(b).name));
      // Collapse duplicates sharing the report dedup key (`oracleId || name`) —
      // e.g. several copies of the same token, or emblems from the same source —
      // into one representative row with a count. Keyed identically to the report
      // dedup, so one row maps 1:1 to one telemetry event. Insertion order
      // preserves the name sort above.
      const byKey = new Map<string, ZoneEntry>();
      for (const obj of list) {
        const key = reportKey(reportIdentity(obj));
        const entry = byKey.get(key);
        if (entry) entry.count += 1;
        else byKey.set(key, { obj, count: 1 });
      }
      return [{ zone, entries: [...byKey.values()] }];
    });
    // Chips ordered by seat_order so they read the same as board seating.
    const seats = [...seatSet].sort(
      (a, b) => (seatOrder?.indexOf(a) ?? 0) - (seatOrder?.indexOf(b) ?? 0),
    );
    // Zone chips ordered by ZONE_ORDER so they read the same as the list sections.
    const zones = ZONE_ORDER.filter((zone) => zoneSet.has(zone));
    return { groups, seats, zones };
  }, [gameState, viewerId, search, playerFilter, zoneFilter, seatOrder]);

  return (
    <ModalPanelShell
      open={open}
      title={t("cardReport.title")}
      subtitle={t("cardReport.subtitle")}
      onClose={close}
      maxWidthClassName="max-w-lg"
      bodyClassName="flex flex-col"
    >
      {/* Pinned controls: the search + player/zone filters stay put while the list scrolls. */}
      <div className="flex shrink-0 flex-col gap-2.5 px-4 pt-4 pb-3 lg:px-6">
        <input
          type="text"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder={t("cardReport.search")}
          className="w-full rounded-[12px] border border-white/10 bg-black/20 px-3 py-2 text-sm text-white placeholder:text-slate-500 focus:border-indigo-400/50 focus:outline-none"
        />
        {seats.length > 1 && (
          <div className="flex flex-wrap gap-1.5">
            <FilterChip
              active={playerFilter === null}
              label={t("cardReport.allPlayers")}
              onClick={() => setPlayerFilter(null)}
            />
            {seats.map((seat) => (
              <FilterChip
                key={seat}
                active={playerFilter === seat}
                label={getPlayerDisplayName(seat, viewerId)}
                color={getSeatColor(seat, seatOrder)}
                onClick={() => setPlayerFilter((cur) => (cur === seat ? null : seat))}
              />
            ))}
          </div>
        )}
        {zones.length > 1 && (
          <div className="flex flex-wrap gap-1.5">
            <FilterChip
              active={zoneFilter === null}
              label={t("cardReport.allZones")}
              onClick={() => setZoneFilter(null)}
            />
            {zones.map((zone) => (
              <FilterChip
                key={zone}
                active={zoneFilter === zone}
                label={t(`cardReport.zone.${zone}`)}
                onClick={() => setZoneFilter((cur) => (cur === zone ? null : zone))}
              />
            ))}
          </div>
        )}
      </div>

      {/* Scrollable card list. */}
      <div className="thin-scrollbar min-h-0 flex-1 overflow-y-auto px-4 pb-4 lg:px-6">
        {groups.length === 0 ? (
          <p className="py-8 text-center text-sm text-slate-400">{t("cardReport.empty")}</p>
        ) : (
          <div className="flex flex-col gap-4">
            {groups.map((group) => (
              <section key={group.zone} className="flex flex-col gap-1.5">
                <h3 className="text-[0.62rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
                  {t(`cardReport.zone.${group.zone}`)}
                </h3>
                <ul className="flex flex-col gap-1">
                  {group.entries.map((entry) => (
                    <CardReportRow
                      key={entry.obj.id}
                      obj={entry.obj}
                      count={entry.count}
                      viewerId={viewerId}
                    />
                  ))}
                </ul>
              </section>
            ))}
          </div>
        )}
      </div>
    </ModalPanelShell>
  );
}

/** A quick filter pill, shared by the player and zone filter rows. When `color`
 *  is given (player chips) it shows the seat color dot from the HUD so the chip
 *  reads as the same player as their board nameplate; the "All" chips and the
 *  zone chips omit `color`. */
function FilterChip({
  active,
  label,
  color,
  onClick,
}: {
  active: boolean;
  label: string;
  color?: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-xs font-medium transition ${
        active
          ? "border-white/25 bg-white/12 text-white"
          : "border-white/10 bg-black/20 text-slate-400 hover:bg-white/6 hover:text-slate-200"
      }`}
    >
      {color && (
        <span
          aria-hidden
          className="h-2 w-2 shrink-0 rounded-full"
          style={{ backgroundColor: color }}
        />
      )}
      <span className="max-w-[8rem] truncate">{label}</span>
    </button>
  );
}

/** One picker row. Fetches its own parse details (Rules of Hooks: one hook per
 *  rendered row, so this must be a component, not a `.map` callback body) and
 *  gates the report action until the parse resolves, so a transient `0/0`
 *  fraction can never be sent. The whole row is a tap target; tapping opens an
 *  inline Cancel/Report confirm, and once reported the row locks. */
function CardReportRow({
  obj,
  count,
  viewerId,
}: {
  obj: GameObject;
  count: number;
  viewerId: PlayerId;
}) {
  const { t } = useTranslation("game");
  const [confirming, setConfirming] = useState(false);
  const identity = reportIdentity(obj);
  // Parse coverage keys on the identity name (the source card for emblems), so
  // an emblem gets its source's real, loadable parse fraction rather than the
  // engine's synthetic "Emblem" name, which resolves to nothing.
  const parseItems = useCardParseDetails(identity.name);
  // Label the card with the seat that owns/controls it (per `labelSeatForZone`),
  // resolved to the same display name + seat color the HUD nameplates use, so the
  // picker agrees with the board instead of showing a generic "opponent".
  // `viewerId` gives the local seat the "You" treatment via `getPlayerDisplayName`.
  const seatId = labelSeatForZone(obj);
  const seatColor = useSeatColor(seatId);
  const seatName = getPlayerDisplayName(seatId, viewerId);
  // Tiny art thumbnail + shared hover/long-press preview, driven off the real
  // game object so hovering the icon opens the same full card preview overlay the
  // board uses (display only). `cardImageLookup` already resolves emblems (via
  // emblem_source), tokens, MDFCs, and transformed faces, so no special-casing.
  const { handlers: hoverHandlers, firedRef } = useCardHover(obj.id);
  const imageLookup = cardImageLookup(obj);
  const isToken = obj.display_source === "Token";
  const { src: artSrc } = useCardImage(imageLookup.name, {
    size: "art_crop",
    faceIndex: imageLookup.faceIndex,
    isToken,
    tokenFilters: isToken ? tokenFiltersForObject(obj) : undefined,
    tokenImageRef: isToken ? obj.token_image_ref : undefined,
    oracleId: imageLookup.oracleId,
    faceName: imageLookup.faceName,
  });

  const loaded = parseItems != null;
  const supported = (parseItems ?? []).filter((item) => item.supported).length;
  const total = (parseItems ?? []).length;
  const allSupported = total > 0 && supported === total;

  const context: CardReportContext = {
    oracleId: identity.oracleId,
    faceName: identity.faceName,
    name: identity.name,
    zone: obj.zone,
    supported,
    total,
  };
  const { sent, report } = useCardReport(context);

  // Tiny art icon; hovering it (or long-pressing on touch) opens the full
  // preview. Shared across the row's three states, like `info`.
  const thumb = (
    <span
      {...hoverHandlers}
      className="relative block h-9 w-9 shrink-0 overflow-hidden rounded-[6px] border border-white/10 bg-black/30"
    >
      {artSrc && (
        <img src={artSrc} alt="" draggable={false} className="h-full w-full object-cover" />
      )}
    </span>
  );

  // Card name + owning seat — shared across the row's three states.
  const info = (
    <div className="min-w-0 flex-1 text-left">
      <div className="flex items-baseline gap-1.5">
        <span className="truncate text-sm font-medium text-white">{identity.name}</span>
        {identity.isEmblem && (
          <span className="shrink-0 rounded-[4px] bg-amber-400/15 px-1.5 text-[9px] font-semibold uppercase tracking-[0.1em] text-amber-300">
            {t("cardReport.emblemTag")}
          </span>
        )}
        {count > 1 && (
          <span className="shrink-0 text-[11px] font-medium tabular-nums text-slate-500">
            ×{count}
          </span>
        )}
      </div>
      <div className="mt-0.5 flex items-center gap-1">
        <span
          aria-hidden
          className="h-1.5 w-1.5 shrink-0 rounded-full"
          style={{ backgroundColor: seatColor }}
        />
        <span className="truncate text-[0.65rem] font-medium" style={{ color: seatColor }}>
          {seatName}
        </span>
      </div>
    </div>
  );

  // Already reported this session — the dedup key is spent, so lock the row.
  if (sent) {
    return (
      <li className="flex items-center gap-3 rounded-[10px] border border-emerald-400/25 bg-emerald-400/[0.06] px-3 py-2">
        {thumb}
        {info}
        <span className="shrink-0 text-[11px] font-medium text-emerald-400">
          {t("preview.reported")}
        </span>
      </li>
    );
  }

  // Second tap: an explicit confirm so a stray tap can't fire a report.
  if (confirming) {
    return (
      <li className="flex items-center gap-3 rounded-[10px] border border-red-400/30 bg-red-500/[0.07] px-3 py-2">
        {thumb}
        {info}
        <div className="flex shrink-0 items-center gap-1.5">
          <button
            type="button"
            onClick={() => setConfirming(false)}
            className="rounded-[8px] px-2.5 py-1 text-[11px] font-medium text-slate-400 transition hover:text-slate-200"
          >
            {t("cardReport.cancel")}
          </button>
          <button
            type="button"
            onClick={() => {
              report();
              setConfirming(false);
            }}
            className="rounded-[8px] bg-red-500/90 px-2.5 py-1 text-[11px] font-semibold text-white transition hover:bg-red-500"
          >
            {t("preview.report")}
          </button>
        </div>
      </li>
    );
  }

  // Default: the whole row is one tap target. Disabled until parse details load
  // so a transient 0/0 fraction can never be sent.
  return (
    <li>
      <button
        type="button"
        disabled={!loaded}
        onClick={() => {
          // Suppress the tap that trails a touch long-press (which just opened
          // the sticky preview), so previewing the art never fires a report.
          if (!firedRef.current) setConfirming(true);
        }}
        className="flex w-full items-center gap-3 rounded-[10px] border border-white/8 bg-white/[0.03] px-3 py-2 transition hover:border-white/15 hover:bg-white/[0.06] disabled:cursor-not-allowed disabled:opacity-50"
      >
        {thumb}
        {info}
        {loaded && total > 0 && (
          <span
            className={`shrink-0 text-[11px] font-medium tabular-nums ${
              allSupported ? "text-emerald-400" : "text-amber-400"
            }`}
          >
            {supported}/{total}
          </span>
        )}
        <svg
          aria-hidden
          xmlns="http://www.w3.org/2000/svg"
          viewBox="0 0 20 20"
          fill="none"
          stroke="currentColor"
          strokeWidth={1.5}
          strokeLinecap="round"
          strokeLinejoin="round"
          className="h-3.5 w-3.5 shrink-0 text-slate-500"
        >
          <path d="M4 2.5v15" />
          <path d="M4 3.5h9.5l-1.6 3 1.6 3H4" />
        </svg>
      </button>
    </li>
  );
}
