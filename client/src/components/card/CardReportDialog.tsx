import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import type { GameObject, PlayerId, Zone } from "../../adapter/types.ts";
import { type CardReportContext, useCardReport } from "../../hooks/useCardReport.ts";
import { useCardParseDetails } from "../../hooks/useEngineCardData.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
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
  const [search, setSearch] = useState("");

  const groups = useMemo<ZoneGroup[]>(() => {
    if (!gameState) return [];
    const query = search.trim().toLowerCase();
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
      if (query && !reportIdentity(obj).name.toLowerCase().includes(query)) continue;
      const list = byZone.get(obj.zone) ?? [];
      list.push(obj);
      byZone.set(obj.zone, list);
    }
    return ZONE_ORDER.flatMap((zone) => {
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
  }, [gameState, viewerId, search]);

  return (
    <ModalPanelShell
      open={open}
      title={t("cardReport.title")}
      subtitle={t("cardReport.subtitle")}
      onClose={close}
      maxWidthClassName="max-w-lg"
    >
      <div className="flex flex-col gap-3 px-4 py-4 lg:px-6">
        <input
          type="text"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder={t("cardReport.search")}
          className="w-full rounded-[12px] border border-white/10 bg-black/20 px-3 py-2 text-sm text-white placeholder:text-slate-500 focus:border-indigo-400/50 focus:outline-none"
        />

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

/** One picker row. Fetches its own parse details (Rules of Hooks: one hook per
 *  rendered row, so this must be a component, not a `.map` callback body) and
 *  gates the report action until the parse resolves, so a transient `0/0`
 *  fraction can never be sent. */
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
  const identity = reportIdentity(obj);
  // Parse coverage keys on the identity name (the source card for emblems), so
  // an emblem gets its source's real, loadable parse fraction rather than the
  // engine's synthetic "Emblem" name, which resolves to nothing.
  const parseItems = useCardParseDetails(identity.name);
  const isOwn = labelSeatForZone(obj) === viewerId;

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
  const disabled = sent || !loaded;

  return (
    <li className="flex items-center gap-3 rounded-[10px] border border-white/8 bg-white/[0.03] px-3 py-2">
      <div className="min-w-0 flex-1">
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
        <div className="mt-0.5 text-[0.62rem] uppercase tracking-[0.12em] text-slate-500">
          {t(isOwn ? "cardReport.ownTag" : "cardReport.opponentTag")}
        </div>
      </div>

      {loaded && total > 0 && (
        <span
          className={`shrink-0 text-[11px] font-medium tabular-nums ${
            allSupported ? "text-emerald-400" : "text-amber-400"
          }`}
        >
          {supported}/{total}
        </span>
      )}

      <button
        type="button"
        onClick={report}
        disabled={disabled}
        className={
          sent
            ? "shrink-0 text-[11px] font-medium text-emerald-400"
            : loaded
              ? "shrink-0 text-[11px] text-indigo-300 hover:text-indigo-200"
              : "shrink-0 text-[11px] text-slate-600"
        }
      >
        {sent ? t("preview.reported") : t("preview.report")}
      </button>
    </li>
  );
}
