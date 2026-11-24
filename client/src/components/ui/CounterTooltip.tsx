import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";

import { formatCounterTooltip } from "../../viewmodel/cardProps.ts";
import { GameplayTooltip, type TooltipTouch } from "./GameplayTooltip.tsx";

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
  /**
   * What a touch tap on the counter does (see `GameplayTooltip`). The default
   * suits a counter on a card whose tap acts; a counter that only explains
   * itself, as in the card preview, passes `"toggle"`.
   */
  touch?: TooltipTouch;
}

export function CounterTooltip({
  type,
  count,
  children,
  className,
  isUnbounded,
  touch,
}: CounterTooltipProps) {
  const { t } = useTranslation("game");
  const lines = formatCounterTooltip(type, count, t, isUnbounded).split("\n");

  return (
    <span className="group relative inline-flex">
      {children}
      <GameplayTooltip className={className} touch={touch}>
        {lines.map((line, i) => (
          <span key={i} className="block">
            {line}
          </span>
        ))}
      </GameplayTooltip>
    </span>
  );
}
