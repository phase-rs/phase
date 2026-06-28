import { useTranslation } from "react-i18next";

import { useGameStore } from "../../stores/gameStore.ts";
import { CardImage } from "../card/CardImage.tsx";

export function ArchenemyPanel() {
  const { t } = useTranslation("game");
  const gameState = useGameStore((s) => s.gameState);
  const archenemy = gameState?.derived?.archenemy;
  const activeSchemes = archenemy?.active_scheme_ids?.flatMap((id) => {
    const scheme = gameState?.objects[id];
    return scheme ? [scheme] : [];
  }) ?? [];

  if (!archenemy) return null;

  const primaryScheme = activeSchemes[0];

  return (
    <div className="pointer-events-auto absolute right-2 top-2 z-30 flex max-w-[min(92vw,30rem)] items-center gap-3 rounded-md border border-slate-700 bg-slate-950/90 px-3 py-2 text-slate-100 shadow-lg backdrop-blur">
      <div className="h-[112px] w-[80px] shrink-0 [--card-h:112px] [--card-w:80px]">
        {primaryScheme ? (
          <CardImage
            cardName={primaryScheme.name}
            oracleId={primaryScheme.printed_ref?.oracle_id ?? undefined}
            faceName={primaryScheme.printed_ref?.face_name ?? undefined}
            faceDown={primaryScheme.face_down}
            size="small"
          />
        ) : (
          <div className="flex h-full w-full items-center justify-center rounded-md border border-slate-700 bg-slate-900 text-xs text-slate-400">
            {t("archenemy.noScheme")}
          </div>
        )}
      </div>
      <div className="flex min-w-0 flex-col gap-2 text-sm">
        <div>
          <div className="text-xs uppercase text-slate-400">{t("archenemy.active")}</div>
          <div className="max-w-64 truncate font-semibold">
            {primaryScheme?.name ?? t("archenemy.none")}
          </div>
        </div>
        {activeSchemes.length > 1 && (
          <div className="max-w-64 truncate text-xs text-slate-300">
            {activeSchemes.slice(1).map((scheme) => scheme.name).join(", ")}
          </div>
        )}
        <div className="flex items-center justify-between gap-4 text-xs text-slate-300">
          <span>{t("archenemy.deckCount", { count: archenemy.scheme_deck_count })}</span>
          <span>{t("archenemy.heroCount", { count: archenemy.hero_player_ids?.length ?? 0 })}</span>
        </div>
      </div>
    </div>
  );
}
