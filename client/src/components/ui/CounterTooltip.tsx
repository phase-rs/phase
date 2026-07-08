import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";

import { formatCounterTooltip } from "../../viewmodel/cardProps.ts";
import { GameplayTooltip } from "./GameplayTooltip.tsx";

interface CounterTooltipProps {
  type: string;
  count: number;
  children: ReactNode;
  className?: string;
}

export function CounterTooltip({
  type,
  count,
  children,
  className,
}: CounterTooltipProps) {
  const { t } = useTranslation("game");
  const lines = formatCounterTooltip(type, count, t).split("\n");

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
