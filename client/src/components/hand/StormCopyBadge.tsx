import { useTranslation } from "react-i18next";

type StormCopyBadgeVariant = "drawer" | "held" | "fan";

const BADGE_CLASS_BY_VARIANT: Record<StormCopyBadgeVariant, string> = {
  drawer:
    "pointer-events-none absolute right-1 top-1 rounded-full bg-violet-700 px-1.5 py-0.5 text-[11px] font-bold leading-none text-white shadow-md",
  held:
    "absolute right-1 top-1 rounded-full bg-violet-700 px-1.5 py-0.5 text-[11px] font-bold leading-none text-white shadow-md",
  fan:
    "pointer-events-none absolute -right-1 -top-2 rounded-full bg-violet-700 px-1.5 py-0.5 text-[10px] font-bold leading-none text-white shadow-md",
};

export function StormCopyBadge({
  count,
  variant,
}: {
  count: number;
  variant: StormCopyBadgeVariant;
}) {
  const { t } = useTranslation("game");

  return (
    <span className={BADGE_CLASS_BY_VARIANT[variant]} title={t("storm.copies", { count })}>
      {count}
    </span>
  );
}
