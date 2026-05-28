import { useTranslation } from "react-i18next";

import type { DungeonId } from "../../adapter/types.ts";

interface StatusBadgeProps {
  label: string;
  value?: number | string;
  tone?: "neutral" | "amber";
}

export function StatusBadge({
  label,
  value,
  tone = "neutral",
}: StatusBadgeProps) {
  return (
    <span
      className={`inline-flex items-center gap-1 rounded-full px-2 py-1 text-[10px] font-semibold tracking-[0.16em] uppercase ${
        tone === "amber"
          ? "bg-amber-400/16 text-amber-100 ring-1 ring-amber-300/30"
          : "bg-white/7 text-slate-200 ring-1 ring-white/10"
      }`}
    >
      <span>{label}</span>
      {value != null ? <span className="tabular-nums text-white">{value}</span> : null}
    </span>
  );
}

export function MonarchBadge() {
  const { t } = useTranslation("game");
  return (
    <span
      role="img"
      aria-label={t("badges.monarch")}
      title={t("badges.monarchTooltip")}
      className="relative inline-flex h-6 min-w-6 shrink-0 items-center justify-center overflow-hidden rounded-full px-1 text-[12px] leading-none ring-1 bg-amber-400 ring-amber-200/80 shadow-[0_0_14px_rgba(251,191,36,0.55)]"
    >
      <span
        aria-hidden
        className="absolute inset-0 rounded-full bg-[radial-gradient(circle_at_30%_24%,rgba(255,255,255,0.95)_0_10%,transparent_12%),radial-gradient(circle_at_68%_30%,rgba(254,243,199,0.95)_0_7%,transparent_9%),radial-gradient(circle_at_38%_74%,rgba(180,83,9,0.7)_0_11%,transparent_13%),linear-gradient(135deg,#fffbeb_0%,#fcd34d_36%,#b45309_72%,#451a03_100%)]"
      />
      <span
        aria-hidden
        className="absolute -bottom-1 left-1/2 h-3 w-5 -translate-x-1/2 rounded-[45%] bg-amber-950/30 blur-[1px]"
      />
      <span className="relative drop-shadow-[0_1px_1px_rgba(0,0,0,0.45)]">👑</span>
    </span>
  );
}

export function InitiativeBadge() {
  const { t } = useTranslation("game");
  return (
    <span
      role="img"
      aria-label={t("badges.initiative")}
      title={t("badges.initiativeTooltip")}
      className="relative inline-flex h-6 min-w-6 shrink-0 items-center justify-center overflow-hidden rounded-full px-1 text-[12px] leading-none ring-1 bg-cyan-500 ring-cyan-200/80 shadow-[0_0_14px_rgba(34,211,238,0.55)]"
    >
      <span
        aria-hidden
        className="absolute inset-0 rounded-full bg-[radial-gradient(circle_at_30%_24%,rgba(255,255,255,0.95)_0_10%,transparent_12%),radial-gradient(circle_at_68%_30%,rgba(207,250,254,0.95)_0_7%,transparent_9%),radial-gradient(circle_at_38%_74%,rgba(14,116,144,0.7)_0_11%,transparent_13%),linear-gradient(135deg,#ecfeff_0%,#67e8f9_36%,#0e7490_72%,#083344_100%)]"
      />
      <span
        aria-hidden
        className="absolute -bottom-1 left-1/2 h-3 w-5 -translate-x-1/2 rounded-[45%] bg-cyan-950/30 blur-[1px]"
      />
      <span className="relative drop-shadow-[0_1px_1px_rgba(0,0,0,0.5)]">🛡</span>
    </span>
  );
}

export function CityBlessingBadge() {
  const { t } = useTranslation("game");
  return (
    <span
      role="img"
      aria-label={t("badges.cityBlessing")}
      title={t("badges.cityBlessingTooltip")}
      className="relative inline-flex h-6 min-w-6 shrink-0 items-center justify-center overflow-hidden rounded-full px-1 text-[12px] leading-none ring-1 bg-yellow-400 ring-yellow-200/80 shadow-[0_0_14px_rgba(250,204,21,0.6)]"
    >
      <span
        aria-hidden
        className="absolute inset-0 rounded-full bg-[radial-gradient(circle_at_50%_40%,rgba(255,255,255,0.95)_0_18%,transparent_22%),radial-gradient(circle_at_50%_50%,rgba(254,240,138,0.85)_0_36%,transparent_42%),linear-gradient(135deg,#fefce8_0%,#fde047_36%,#ca8a04_72%,#422006_100%)]"
      />
      <span className="relative drop-shadow-[0_1px_1px_rgba(0,0,0,0.4)]">☀</span>
    </span>
  );
}

interface DungeonBadgeProps {
  dungeonName: DungeonId;
  roomIndex: number;
}

const DUNGEON_DISPLAY_NAMES: Record<DungeonId, string> = {
  LostMineOfPhandelver: "Lost Mine",
  DungeonOfTheMadMage: "Mad Mage",
  TombOfAnnihilation: "Tomb",
  Undercity: "Undercity",
  BaldursGateWilderness: "Baldur's Gate",
};

export function DungeonBadge({ dungeonName, roomIndex }: DungeonBadgeProps) {
  const { t } = useTranslation("game");
  const display = DUNGEON_DISPLAY_NAMES[dungeonName];
  const room = roomIndex + 1;
  return (
    <span
      role="img"
      aria-label={t("badges.dungeonAriaLabel", { name: display, room })}
      title={t("badges.dungeonTooltip", { name: display, room })}
      className="relative inline-flex h-6 shrink-0 items-center gap-1 overflow-hidden rounded-full bg-violet-500/85 px-2 text-[10px] font-semibold uppercase tracking-[0.12em] text-violet-50 ring-1 ring-violet-300/70 shadow-[0_0_12px_rgba(139,92,246,0.45)]"
    >
      <span aria-hidden className="text-[12px] leading-none drop-shadow-[0_1px_1px_rgba(0,0,0,0.5)]">🏰</span>
      <span className="relative truncate">{display}</span>
      <span className="relative tabular-nums text-white">{room}</span>
    </span>
  );
}

type CounterBadgeKind = "poison" | "speed" | "rad" | "energy" | "ring";

interface CounterBadgeProps {
  kind: CounterBadgeKind;
  value: number;
  ringBearerName?: string | null;
}

export function CounterBadge({ kind, value, ringBearerName }: CounterBadgeProps) {
  const { t } = useTranslation("game");
  if (kind === "poison") {
    return (
      <span
        role="img"
        aria-label={t("badges.poisonAriaLabel", { count: value })}
        title={t("badges.poisonTooltip", { count: value })}
        className={`relative inline-flex h-6 min-w-6 shrink-0 items-center justify-center overflow-hidden rounded-full px-1 text-[11px] font-black leading-none tabular-nums text-lime-950 ring-1 ${
          value >= 8
            ? "bg-lime-300 ring-lime-100 shadow-[0_0_16px_rgba(217,249,157,0.55)]"
            : "bg-lime-400 ring-lime-200/70 shadow-[0_0_12px_rgba(190,242,100,0.34)]"
        }`}
      >
        <span
          aria-hidden
          className="absolute inset-0 rounded-full bg-[radial-gradient(circle_at_30%_24%,rgba(255,255,255,0.9)_0_9%,transparent_11%),radial-gradient(circle_at_68%_30%,rgba(254,240,138,0.95)_0_7%,transparent_9%),radial-gradient(circle_at_38%_74%,rgba(132,204,22,0.72)_0_11%,transparent_13%),linear-gradient(135deg,#f7fee7_0%,#bef264_36%,#65a30d_72%,#1a2e05_100%)]"
        />
        <span
          aria-hidden
          className="absolute -bottom-1 left-1/2 h-3 w-5 -translate-x-1/2 rounded-[45%] bg-lime-950/28 blur-[1px]"
        />
        <span className="relative">{value}</span>
      </span>
    );
  }

  if (kind === "energy") {
    return (
      <span
        role="img"
        aria-label={t("badges.energyAriaLabel", { count: value })}
        title={t("badges.energyTooltip", { count: value })}
        className="relative inline-flex h-6 min-w-6 shrink-0 items-center justify-center gap-px overflow-hidden rounded-full px-1 text-[11px] font-black leading-none tabular-nums text-cyan-950 ring-1 bg-cyan-300 ring-cyan-100 shadow-[0_0_12px_rgba(103,232,249,0.5)]"
      >
        <span
          aria-hidden
          className="absolute inset-0 rounded-full bg-[radial-gradient(circle_at_30%_24%,rgba(255,255,255,0.95)_0_9%,transparent_11%),radial-gradient(circle_at_68%_30%,rgba(207,250,254,0.9)_0_7%,transparent_9%),radial-gradient(circle_at_38%_74%,rgba(8,145,178,0.7)_0_11%,transparent_13%),linear-gradient(135deg,#ecfeff_0%,#67e8f9_36%,#0891b2_72%,#083344_100%)]"
        />
        <span className="relative">⚡{value}</span>
      </span>
    );
  }

  if (kind === "ring") {
    const ringTitle = [
      t("badges.ringTooltip", { level: value }),
      ringBearerName
        ? t("badges.ringBearerTooltip", { name: ringBearerName })
        : t("badges.noRingBearerTooltip"),
      t("badges.ringLevel1"),
      t("badges.ringLevel2"),
      t("badges.ringLevel3"),
      t("badges.ringLevel4"),
    ].join("\n");
    return (
      <span
        role="img"
        aria-label={t("badges.ringAriaLabel", {
          level: value,
          bearer: ringBearerName ?? t("badges.noRingBearer"),
        })}
        title={ringTitle}
        className="relative inline-flex h-6 min-w-6 shrink-0 items-center justify-center gap-px overflow-hidden rounded-full px-1 text-[11px] font-black leading-none tabular-nums text-amber-950 ring-1 bg-yellow-600 ring-yellow-300/70 shadow-[0_0_12px_rgba(202,138,4,0.55)]"
      >
        <span
          aria-hidden
          className="absolute inset-0 rounded-full bg-[radial-gradient(circle_at_50%_50%,transparent_0_30%,rgba(0,0,0,0.45)_32%,transparent_38%),linear-gradient(135deg,#fde68a_0%,#d97706_45%,#78350f_100%)]"
        />
        <span className="relative drop-shadow-[0_1px_1px_rgba(0,0,0,0.6)]">{value}</span>
      </span>
    );
  }

  if (kind === "rad") {
    return (
      <span
        role="img"
        aria-label={t("badges.radAriaLabel", { count: value })}
        title={t("badges.radTooltip", { count: value })}
        className={`relative inline-flex h-6 min-w-6 shrink-0 items-center justify-center gap-px overflow-hidden rounded-full px-1 text-[11px] font-black leading-none tabular-nums text-amber-950 ring-1 ${
          value >= 8
            ? "bg-amber-300 ring-amber-100 shadow-[0_0_16px_rgba(252,211,77,0.55)]"
            : "bg-amber-500 ring-amber-300/70 shadow-[0_0_12px_rgba(245,158,11,0.4)]"
        }`}
      >
        <span
          aria-hidden
          className="absolute inset-0 rounded-full bg-[radial-gradient(circle_at_30%_24%,rgba(255,255,255,0.85)_0_9%,transparent_11%),radial-gradient(circle_at_68%_30%,rgba(254,243,199,0.9)_0_7%,transparent_9%),radial-gradient(circle_at_38%_74%,rgba(217,119,6,0.72)_0_11%,transparent_13%),linear-gradient(135deg,#fffbeb_0%,#fbbf24_36%,#b45309_72%,#451a03_100%)]"
        />
        <span
          aria-hidden
          className="absolute -bottom-1 left-1/2 h-3 w-5 -translate-x-1/2 rounded-[45%] bg-amber-950/28 blur-[1px]"
        />
        <span className="relative">☢{value}</span>
      </span>
    );
  }

  return (
    <span
      role="img"
      aria-label={t("badges.speedAriaLabel", { value })}
      title={t("badges.speedTooltip", { value })}
      className="relative inline-flex h-6 min-w-6 shrink-0 items-center justify-center overflow-hidden rounded-[6px] px-1 text-[11px] font-black leading-none tabular-nums text-white ring-1 ring-slate-100/60 shadow-[0_0_10px_rgba(226,232,240,0.22)]"
    >
      <span
        aria-hidden
        className="absolute inset-0 bg-[linear-gradient(90deg,rgba(15,23,42,0.82)_0_2px,transparent_2px),linear-gradient(45deg,#f8fafc_25%,#020617_25%,#020617_50%,#f8fafc_50%,#f8fafc_75%,#020617_75%,#020617_100%)] bg-[length:100%_100%,7px_7px]"
      />
      <span aria-hidden className="absolute inset-0 bg-cyan-300/10" />
      <span className="relative rounded-sm bg-black/62 px-0.5">{value}</span>
    </span>
  );
}
