import { useTranslation } from "react-i18next";

interface StormCounterProps {
  count: number;
}

/** Displays the engine-provided, table-wide Storm copy count. */
export function StormCounter({ count }: StormCounterProps) {
  const { t } = useTranslation("game");

  if (count === 0) {
    return null;
  }

  const label = t("storm.count", { count });

  return (
    <span
      role="status"
      aria-label={label}
      title={label}
      className="inline-flex h-6 min-w-6 shrink-0 items-center justify-center gap-px rounded-full bg-violet-400/18 px-1.5 text-[11px] font-black leading-none tabular-nums text-violet-100 ring-1 ring-violet-300/40 shadow-[0_0_12px_rgba(167,139,250,0.3)]"
    >
      <span aria-hidden>⛈</span>
      <span>{count}</span>
    </span>
  );
}
