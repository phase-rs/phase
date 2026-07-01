import { useTranslation } from "react-i18next";

interface UnimplementedMechanicsBadgeProps {
  mechanics: string[];
  className?: string;
}

/**
 * Amber "!" overlay warning that a card has partially-supported mechanics.
 * Used on hand, battlefield, and stack surfaces for visual parity.
 */
export function UnimplementedMechanicsBadge({
  mechanics,
  className = "absolute top-0.5 left-0.5",
}: UnimplementedMechanicsBadgeProps) {
  const { t } = useTranslation("game");

  if (mechanics.length === 0) {
    return null;
  }

  return (
    <span
      className={`${className} z-10 bg-amber-500 text-black text-[8px] font-bold rounded-sm px-0.5 leading-tight`}
      title={t("card.unimplemented", { mechanics: mechanics.join(", ") })}
    >
      !
    </span>
  );
}
