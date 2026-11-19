import { useTranslation } from "react-i18next";

import type { GameObject } from "../../adapter/types.ts";
import { useCardHover } from "../../hooks/useCardHover.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { CardImage } from "../card/CardImage.tsx";

/**
 * One active scheme. Hover (or long-press on touch) opens the standard
 * in-game card preview, which is how the reader sees the full card image
 * and the current scheme's effect text. A child component so each thumbnail
 * owns its `useCardHover` call — hooks can't go in the map below.
 */
function SchemeThumbnail({
  scheme,
  thumbClassName,
  imageSize,
}: {
  scheme: GameObject;
  thumbClassName: string;
  imageSize: "small" | "normal";
}) {
  const { handlers } = useCardHover(scheme.id);
  return (
    <div
      {...handlers}
      className={`cursor-pointer rounded-md ring-purple-400/0 transition-shadow hover:ring-2 hover:ring-purple-400/70 ${thumbClassName}`}
    >
      <CardImage
        cardName={scheme.name}
        oracleId={scheme.printed_ref?.oracle_id ?? undefined}
        faceName={scheme.printed_ref?.face_name ?? undefined}
        ownerPrinting={scheme.printing}
        faceDown={scheme.face_down}
        size={imageSize}
      />
    </div>
  );
}

export function ArchenemyPanel() {
  const { t } = useTranslation("game");
  const gameState = useGameStore((s) => s.gameState);
  const archenemy = gameState?.derived?.archenemy;
  const activeSchemes = archenemy?.active_scheme_ids?.flatMap((id) => {
    const scheme = gameState?.objects[id];
    return scheme ? [scheme] : [];
  }) ?? [];

  if (!archenemy) return null;

  const [primaryScheme, ...additionalSchemes] = activeSchemes;

  return (
    <div className="pointer-events-auto absolute right-2 top-2 z-30 flex max-w-[min(92vw,30rem,calc(100%-1rem))] items-start gap-3 rounded-md border border-slate-700 bg-slate-950/90 px-3 py-2 text-slate-100 shadow-lg backdrop-blur">
      <div className="h-[120px] w-[84px] shrink-0 [--card-h:120px] [--card-w:84px]">
        {primaryScheme ? (
          <SchemeThumbnail scheme={primaryScheme} thumbClassName="h-full w-full" imageSize="normal" />
        ) : (
          <div className="flex h-full w-full items-center justify-center rounded-md border border-slate-700 bg-slate-900 p-1 text-center text-xs leading-tight text-slate-400">
            {t("archenemy.noScheme")}
          </div>
        )}
      </div>
      <div className="flex min-w-0 flex-col gap-2 text-sm">
        <div>
          <div className="text-xs uppercase text-slate-400">{t("archenemy.active")}</div>
          <div className="line-clamp-2 break-words font-semibold">
            {primaryScheme?.name ?? t("archenemy.none")}
          </div>
        </div>
        {additionalSchemes.length > 0 && (
          <div className="flex flex-wrap gap-1.5">
            {additionalSchemes.map((scheme) => (
              <SchemeThumbnail
                key={scheme.id}
                scheme={scheme}
                thumbClassName="h-[56px] w-[40px] shrink-0 [--card-h:56px] [--card-w:40px]"
                imageSize="small"
              />
            ))}
          </div>
        )}
        <div className="flex flex-wrap items-center gap-x-4 gap-y-0.5 text-xs text-slate-300">
          <span>{t("archenemy.deckCount", { count: archenemy.scheme_deck_count })}</span>
          <span>{t("archenemy.heroCount", { count: archenemy.hero_player_ids?.length ?? 0 })}</span>
        </div>
      </div>
    </div>
  );
}
