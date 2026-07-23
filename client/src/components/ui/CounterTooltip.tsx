import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";

import { formatCounterTooltip } from "../../viewmodel/cardProps.ts";
import { GameplayTooltip } from "./GameplayTooltip.tsx";

interface CounterTooltipProps {
  type: string;
  count: number;
  children: ReactNode;
  className?: string;
  /**
   * CR 732.2a / CR 701.34a: when the counter belongs to an accepted counter-growth
   * loop, the badge renders `∞` — so the tooltip summary must say "unbounded" instead
   * of leaking the (still-finite) real `count`. Optional so the callers that draw no
   * `∞` pill compile unchanged and keep the numeric summary.
   */
  isUnbounded?: boolean;
}

export function CounterTooltip({
  type,
  count,
  children,
  className,
  isUnbounded,
}: CounterTooltipProps) {
  const { t } = useTranslation("game");
  const lines = formatCounterTooltip(type, count, t, isUnbounded).split("\n");

  return (
    <span className="group relative inline-flex">
      {children}
      <GameplayTooltip className={className}>
        {lines.map((line, i) => (
          <span key={i} className="block">
            {line}
          </span>
        ))}
      </GameplayTooltip>
    </span>
  );
}
