import type { CSSProperties } from "react";

import { ManaFontIcon } from "../icons/ManaFontIcon.tsx";

type LoyaltyBadgeProps = {
  amount: number;
  kind: "cost" | "total";
  size?: "default" | "battlefield";
  reinforcedTopRim?: boolean;
  className?: string;
  style?: CSSProperties;
};

function loyaltyText(amount: number, kind: LoyaltyBadgeProps["kind"]): string {
  if (kind === "total") return String(amount);
  if (amount > 0) return `+${amount}`;
  return String(amount).replace("-", "−");
}

function loyaltyShapeClass(amount: number, kind: LoyaltyBadgeProps["kind"]): string {
  if (kind === "total") return "ms-loyalty-start";
  if (amount > 0) return "ms-loyalty-up";
  if (amount < 0) return "ms-loyalty-down";
  return "ms-loyalty-zero";
}

/**
 * One visual contract for loyalty totals and loyalty activation costs. The
 * layered mana-font silhouette keeps the native loyalty-marker shape while
 * giving it a readable, beveled silver rim; the real text overlay remains
 * legible for every value, including values without a mana-font numeral.
 */
export function LoyaltyBadge({
  amount,
  kind,
  size = "default",
  reinforcedTopRim = false,
  className,
  style,
}: LoyaltyBadgeProps) {
  const text = loyaltyText(amount, kind);
  const iconClass = loyaltyShapeClass(amount, kind);

  return (
    <span
      data-loyalty-badge={kind}
      data-loyalty-value={amount}
      role="img"
      aria-label={text}
      className={[
        "relative inline-flex shrink-0 items-center justify-center align-middle leading-none",
        size === "battlefield"
          ? "h-[2.25em] w-[2.25em] text-[22px]"
          : "h-[2.15em] w-[2.15em] text-[20px]",
        className,
      ].filter(Boolean).join(" ")}
      style={style}
    >
      <span aria-hidden className="absolute inset-0 flex items-center justify-center">
        <ManaFontIcon
          iconClass={iconClass}
          fallbackText=""
          className="drop-shadow-[-1px_-1px_0_rgba(255,255,255,0.8)] drop-shadow-[1px_1px_1px_rgba(15,23,42,0.98)]"
          style={{
            color: "#f1f5f9",
            ...(reinforcedTopRim && {
              // The loyalty shield has a concave top contour. Add an upward
              // silver shadow only there, keeping the side and lower rim at
              // the standard width.
              filter: "drop-shadow(-1px -1px 0 rgba(255,255,255,0.8)) drop-shadow(0 -1.25px 0 #e2e8f0) drop-shadow(1px 1px 1px rgba(15,23,42,0.98))",
            }),
          }}
        />
      </span>
      <span
        aria-hidden
        className="absolute inset-0 flex items-center justify-center"
        style={{ transform: "translate(0.04em, 0.05em) scale(0.92)" }}
      >
        <ManaFontIcon
          iconClass={iconClass}
          fallbackText=""
          style={{ color: "#94a3b8" }}
        />
      </span>
      <span aria-hidden className="absolute inset-0 flex scale-[0.78] items-center justify-center">
        <ManaFontIcon
          iconClass={iconClass}
          fallbackText=""
          style={{ color: "#111827" }}
        />
      </span>
      <span className="relative z-10 font-medium text-[0.55em] leading-none tabular-nums text-white [text-shadow:0_1px_2px_rgba(0,0,0,0.95)]">
        {text}
      </span>
    </span>
  );
}
